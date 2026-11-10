// scripts/widget-bench.sh --window-events: a passive listener on Hyprland's event
// socket (socket2) for a number of minutes. It counts events by name and replays the
// widget's window-event path twice, to say how many status runs each would have made:
// - before 1n2p.38, every `openwindow` and `closewindow` restarted a 500 ms debounce
//   whose end ran `status --cached -v` (`status_runs_unfiltered`);
// - now, omarchy/Service.qml windowEvent asks Model.windowEventAction, which lets through
//   only a window that may be WoW's and the closing of one it remembered (`status_runs`).
//   The listener cannot know whether a client runs, so it replays the case where none
//   does; while one runs, every closewindow also refreshes.
//
// Privacy: what follows an event's `>>` (window addresses, classes, titles, workspaces)
// is read only to decide as the widget does, in memory, and never kept: the output holds
// counts and one bucket per minute.
//
// Usage: bun window-events.js --minutes N --out FILE [--debounce-ms 500]

const Model = require("../../omarchy/Model.js")

const args = process.argv.slice(2)
function option(name, fallback) {
  const at = args.indexOf(name)
  return at >= 0 && at + 1 < args.length ? args[at + 1] : fallback
}
const minutes = Number(option("--minutes", "60"))
const out = option("--out", "")
const debounceMs = Number(option("--debounce-ms", "500"))
if (!(minutes > 0) || !out || !(debounceMs > 0)) {
  console.error("usage: bun window-events.js --minutes N --out FILE [--debounce-ms 500]")
  process.exit(2)
}

const runtime = process.env.XDG_RUNTIME_DIR
const signature = process.env.HYPRLAND_INSTANCE_SIGNATURE
if (!runtime || !signature) {
  console.error("window-events: no Hyprland session (XDG_RUNTIME_DIR or HYPRLAND_INSTANCE_SIGNATURE unset)")
  process.exit(1)
}
const socket = `${runtime}/hypr/${signature}/.socket2.sock`

const startedAt = new Date()
const started = performance.now()
const counts = {}
const perMinute = Array.from({ length: Math.ceil(minutes) }, () => ({ window_events: 0, status_runs_unfiltered: 0, status_runs: 0 }))
const totals = { window_events: 0, maybe_wow_events: 0, status_runs_unfiltered: 0, status_runs: 0 }
// The addresses of windows that may be WoW's, as the service remembers them.
let remembered = []
let buffered = ""

function minuteOf(now) {
  return Math.min(perMinute.length - 1, Math.floor((now - started) / 60000))
}

// The widget's Timer.restart(): each event pushes the run back by debounceMs; the run
// counts under `field`.
function debouncer(field) {
  let timer = null
  let pending = false
  const run = () => {
    pending = false
    totals[field]++
    perMinute[minuteOf(performance.now())][field]++
  }
  return {
    restart() {
      pending = true
      if (timer) clearTimeout(timer)
      timer = setTimeout(run, debounceMs)
    },
    // A debounce still pending at the end would have run once.
    finish() {
      if (timer) clearTimeout(timer)
      if (pending) run()
    },
  }
}
const unfiltered = debouncer("status_runs_unfiltered")
const filtered = debouncer("status_runs")

function windowEvent(name, data, now) {
  totals.window_events++
  perMinute[minuteOf(now)].window_events++
  unfiltered.restart()
  const verdict = Model.windowEventAction(name, data, remembered, false)
  if (!verdict) return
  totals.maybe_wow_events++
  if (verdict.remember) remembered = remembered.concat([verdict.remember]).slice(-Model.WINDOW_MEMORY)
  if (verdict.forget) remembered = remembered.filter((a) => a !== verdict.forget)
  if (verdict.refresh) filtered.restart()
}

function line(text) {
  const at = text.indexOf(">>")
  const name = at >= 0 ? text.slice(0, at) : text
  if (!/^[a-z0-9_]+$/.test(name)) return
  counts[name] = (counts[name] || 0) + 1
  if (name === "openwindow" || name === "closewindow") windowEvent(name, at >= 0 ? text.slice(at + 2) : "", performance.now())
}

function perHour(runs, elapsedS) {
  return elapsedS > 0 ? Math.round((runs * 3600) / elapsedS * 10) / 10 : null
}

function finish(reason) {
  unfiltered.finish()
  filtered.finish()
  const elapsedS = Math.round((performance.now() - started) / 1000)
  const report = {
    started_at: startedAt.toISOString(),
    finished_at: new Date().toISOString(),
    elapsed_s: elapsedS,
    ended: reason,
    debounce_ms: debounceMs,
    events_by_name: Object.fromEntries(Object.entries(counts).sort((a, b) => b[1] - a[1])),
    window_events: totals.window_events,
    maybe_wow_events: totals.maybe_wow_events,
    status_runs_unfiltered: totals.status_runs_unfiltered,
    status_runs_unfiltered_per_hour: perHour(totals.status_runs_unfiltered, elapsedS),
    status_runs: totals.status_runs,
    status_runs_per_hour: perHour(totals.status_runs, elapsedS),
    busiest_minute: perMinute.reduce((best, m) => (m.window_events > best.window_events ? m : best), perMinute[0]),
    per_minute: perMinute,
  }
  require("fs").writeFileSync(out, JSON.stringify(report, null, 2) + "\n")
  console.error(`window-events: ${totals.window_events} window events (${totals.maybe_wow_events} may be WoW's): `
    + `${totals.status_runs} status runs now, ${totals.status_runs_unfiltered} unfiltered, in ${elapsedS} s → ${out}`)
  process.exit(0)
}

process.on("SIGINT", () => finish("interrupted"))
process.on("SIGTERM", () => finish("interrupted"))
setTimeout(() => finish("completed"), minutes * 60000)

await Bun.connect({
  unix: socket,
  socket: {
    data(_socket, chunk) {
      buffered += Buffer.from(chunk).toString("utf8")
      let nl
      while ((nl = buffered.indexOf("\n")) >= 0) {
        line(buffered.slice(0, nl))
        buffered = buffered.slice(nl + 1)
      }
    },
    close() {
      finish("socket_closed")
    },
    error(_socket, error) {
      console.error(`window-events: ${error}`)
      finish("socket_error")
    },
  },
})
console.error(`window-events: listening on Hyprland's event socket for ${minutes} min`)
