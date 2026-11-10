// scripts/widget-bench.sh's report: every number the bench took, from the files it wrote
// under <out>, as <out>/report.md (printed) and <out>/report.json. Beside each number
// stands the budget proposed for the performance pass (omad-widget-user-first-1n2p.38),
// which asserts them in its tests; a baseline is expected to miss some.
//
// Usage: bun report.js <out>

const fs = require("fs")
const path = require("path")

const out = process.argv[2]
if (!out) {
  console.error("usage: bun report.js <out>")
  process.exit(2)
}

function read(file) {
  const at = path.join(out, file)
  return fs.existsSync(at) && fs.statSync(at).size > 0 ? JSON.parse(fs.readFileSync(at, "utf8")) : null
}

// The budgets proposed for 1n2p.38, each on what this bench measures (the synthetic home,
// the offscreen software renderer, whose one frame is in every QML number).
const BUDGETS = {
  // `status --cached -v`, warm p95, on both homes.
  status_cached_p95_ms: 10,
  // A later open: its first render, from data already read.
  panel_rendered_ms: 50,
  // A later open: every read it makes, done (median).
  panel_data_ms: 100,
  // A list view's first visit, at 150 and at 500 rows.
  view_first_ms_150: 50,
  view_first_ms_500: 100,
  // A kept view shown again.
  view_again_ms: 20,
  // A view's data read again, unchanged, at 150 rows.
  view_reread_ms_150: 50,
  // RSS growth per open and close, from cycle 10 to the last (the first ten warm the
  // allocator).
  leak_kb_per_cycle: 25,
  // status runs caused by window events that cannot be a WoW client's.
  window_status_runs_non_wow: 0,
}

const r2 = (x) => (x === null || x === undefined ? null : Math.round(x * 100) / 100)
function percentile(sorted, p) {
  if (!sorted.length) return null
  return sorted[Math.min(sorted.length - 1, Math.max(0, Math.ceil(p * sorted.length) - 1))]
}

// p50, p95, max and peak RSS of one hyperfine export.
function stats(file) {
  const data = read(file)
  if (!data) return null
  const result = data.results[0]
  const times = result.measurements.map((m) => m.time_wall_clock.value * 1000).sort((a, b) => a - b)
  const rss = result.measurements.map((m) => (m.memory_peak_resident ? m.memory_peak_resident.value : 0))
  const exits = new Set(result.measurements.map((m) => m.exit_code))
  return {
    runs: times.length,
    p50_ms: r2(percentile(times, 0.5)),
    p95_ms: r2(percentile(times, 0.95)),
    max_ms: r2(times[times.length - 1]),
    peak_rss_mib: r2(Math.max(...rss) / 1048576),
    exit_codes: [...exits],
  }
}

const meta = read("meta.json") || {}
const READS = [
  ["status_cached", "status --cached -v"],
  ["status_full", "status -v"],
  ["list", "list -v"],
  ["errors", "errors --limit 10"],
  ["backups", "backup list --limit 50"],
  ["history", "history --limit 50"],
  ["catalog", "search --catalog"],
]
const cli = {}
for (const home of ["synthetic", "real"]) {
  if (!fs.existsSync(path.join(out, "cli", home))) continue
  cli[home] = {}
  for (const [name, words] of READS) {
    cli[home][name] = { words, warm: stats(`cli/${home}/warm/${name}.json`), cold: stats(`cli/${home}/cold/${name}.json`) }
  }
}
const open = read("qml/open.json")
const lists = read("qml/lists.json")
const leak = read("qml/leak.json")
const windows = read("window-events.json")

const lines = []
const say = (s = "") => lines.push(s)
const mark = (value, budget) => (value === null || value === undefined ? "" : value <= budget ? " ✓" : " ✗")

say(`# Widget costs`)
say()
say(`Commit ${meta.commit || "?"}${meta.dirty_files ? ` (+${meta.dirty_files} uncommitted files under omarchy/, src/, Cargo.*)` : ""}, omaddons ${meta.version || "?"}, measured ${meta.measured_at || "?"}${meta.quick ? " (--quick: a smoke test, not a baseline)" : ""}.`)
say(`${meta.cpu || ""}, Linux ${meta.kernel || ""}.`)
for (const [home, shape] of Object.entries(meta.homes || {})) {
  if (shape) say(`- ${home} home: ${shape.clients} clients (${shape.present} present), ${shape.addons} addons; summary.running ${JSON.stringify(shape.running)}`)
}
say()

for (const [home, table] of Object.entries(cli)) {
  say(`## 1. CLI reads, ${home} home (wall ms; warm ${meta.runs ? meta.runs.warm : "?"} runs, cold ${meta.runs ? meta.runs.cold : "?"})`)
  say()
  say("| Read | warm p50 | warm p95 | cold p50 | cold p95 | peak RSS MiB | exit |")
  say("|---|---|---|---|---|---|---|")
  for (const [name, row] of Object.entries(table)) {
    const w = row.warm || {}
    const c = row.cold || {}
    const budget = name === "status_cached" ? mark(w.p95_ms, BUDGETS.status_cached_p95_ms) : ""
    say(`| \`${row.words}\` | ${w.p50_ms ?? ""} | ${w.p95_ms ?? ""}${budget} | ${c.p50_ms ?? ""} | ${c.p95_ms ?? ""} | ${Math.max(w.peak_rss_mib || 0, c.peak_rss_mib || 0) || ""} | ${[...new Set([...(w.exit_codes || []), ...(c.exit_codes || [])])].join(",")} |`)
  }
  const panelReads = ["status_full", "list", "errors", "backups"].map((n) => (table[n] && table[n].warm ? table[n].warm.p50_ms : 0))
  const cached = table.status_cached && table.status_cached.warm ? table.status_cached.warm.p50_ms : null
  say()
  say(`A later panel open reads \`status --cached -v\` (${cached ?? "?"} ms warm p50), then the shown view's data only when it is old or a doorbell rang (1n2p.38). The four reads every open queued before, one after another on the local lane: ${r2(panelReads.reduce((a, b) => a + b, 0))} ms of warm p50 binary time.`)
  say()
}

if (open) {
  const s = open.summary
  say(`## 2. Panel open (real Service and PanelView, the release binary on the synthetic home; ${open.opens.length} opens)`)
  say()
  say(`- First open: rendered ${s.first_open_rendered_ms} ms, every read done ${s.first_open_data_ms} ms.`)
  say(`- Later opens (data from the open before): rendered median ${s.rendered_ms_median} ms (max ${s.rendered_ms_max})${mark(s.rendered_ms_median, BUDGETS.panel_rendered_ms)}; every read done median ${s.data_ms_median} ms (max ${s.data_ms_max})${mark(s.data_ms_median, BUDGETS.panel_data_ms)}.`)
  const order = open.opens[open.opens.length - 1].reads.map((r) => `${r.resource} ${r.done_ms} ms`).join(", ")
  say(`- Lane order (last open, done at): ${order}. One hop to the runner and back: ${open.hop_ms_median} ms.`)
  const byWords = {}
  for (const run of open.runner) {
    if (run.wall_ms === undefined) continue
    ;(byWords[run.words] = byWords[run.words] || []).push(run.wall_ms)
  }
  say(`- The binary's own time per read (median over the opens): ${Object.entries(byWords).map(([w, v]) => `\`${w}\` ${r2(percentile(v.slice().sort((a, b) => a - b), 0.5))} ms`).join(", ")}.`)
  say()
}

if (lists) {
  say(`## 3. Lists (median of ${lists.repeats}; ms from showView to the next render)`)
  say()
  say(`Every number includes one frame of the offscreen software renderer: ${lists.frame_ms} ms for a change that builds nothing.`)
  say()
  say("| View | count | row delegates | items | first visit | again | data read again |")
  say("|---|---|---|---|---|---|---|")
  for (const row of lists.results) {
    const firstBudget = row.count === 150 ? mark(row.first_ms, BUDGETS.view_first_ms_150) : row.count === 500 ? mark(row.first_ms, BUDGETS.view_first_ms_500) : ""
    const rereadBudget = row.count === 150 ? mark(row.reread_ms, BUDGETS.view_reread_ms_150) : ""
    say(`| ${row.view} | ${row.count} | ${row.rows} | ${row.items} | ${row.first_ms}${firstBudget} | ${row.again_ms}${row.count >= 150 ? mark(row.again_ms, BUDGETS.view_again_ms) : ""} | ${row.reread_ms ?? "—"}${rereadBudget} |`)
  }
  say()
}

if (leak) {
  say(`## 5. Open and close ${leak.cycles} times (qmltestrunner's RSS, ${leak.rows} addons, every view visited each time)`)
  say()
  say(`- RSS ${leak.rss_kb_before} kB before, ${leak.rss_kb_after} kB after: ${leak.growth_kb} kB in all. Every 10 cycles: ${leak.marks.map((m) => m.rss_kb).join(", ")} kB.`)
  if (leak.marks.length >= 2) {
    const first = leak.marks[0]
    const last = leak.marks[leak.marks.length - 1]
    const steady = r2((last.rss_kb - first.rss_kb) / (last.cycles - first.cycles))
    say(`- From cycle ${first.cycles} to ${last.cycles}: ${steady} kB per cycle${mark(steady, BUDGETS.leak_kb_per_cycle)}.`)
  }
  say()
}

if (windows) {
  say(`## 4. Window events (${Math.round(windows.elapsed_s / 60)} min from ${windows.started_at}, ${windows.ended})`)
  say()
  say(`- ${windows.window_events} openwindow/closewindow events, ${windows.maybe_wow_events ?? "?"} of them from windows that may be WoW's → ${windows.status_runs} \`status --cached -v\` runs after the ${windows.debounce_ms} ms debounce (${windows.status_runs_per_hour} an hour) with no client running; ${windows.status_runs_unfiltered ?? "?"} (${windows.status_runs_unfiltered_per_hour ?? "?"} an hour) without 1n2p.38's filter. Busiest minute: ${windows.busiest_minute.window_events} events.`)
  say(`- Every event, by name: ${Object.entries(windows.events_by_name).map(([n, c]) => `${n} ${c}`).join(", ") || "none"}.`)
  say()
}

say(`## Proposed budgets (1n2p.38 asserts these)`)
say()
for (const [name, value] of Object.entries(BUDGETS)) say(`- \`${name}\`: ${value}`)

fs.writeFileSync(path.join(out, "report.md"), lines.join("\n") + "\n")
fs.writeFileSync(path.join(out, "report.json"), JSON.stringify({ meta, cli, open, lists, leak, windows, budgets: BUDGETS }, null, 2) + "\n")
console.log(lines.join("\n"))
