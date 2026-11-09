// scripts/widget-bench.sh --window-events: a passive listener on Hyprland's event
// socket (socket2) for a number of minutes. It counts events by name and replays the
// widget's window-event path (omarchy/Service.qml windowEvent: every `openwindow` and
// `closewindow` restarts a 500 ms debounce whose end runs `status --cached -v`) to say
// how many status runs the widget would have made for them.
//
// Privacy: only an event's name is kept, never what follows `>>` (window addresses,
// classes, titles, workspaces). The output holds counts and one bucket per minute.
//
// Usage: bun window-events.js --minutes N --out FILE [--debounce-ms 500]

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
const perMinute = Array.from({ length: Math.ceil(minutes) }, () => ({ window_events: 0, status_runs: 0 }))
let windowEvents = 0
let statusRuns = 0
let lastWindowEvent = -Infinity
let pending = false
let debounce = null
let buffered = ""

function minuteOf(now) {
  return Math.min(perMinute.length - 1, Math.floor((now - started) / 60000))
}

// The widget's Timer.restart(): each window event pushes the run back by debounceMs.
function windowEvent(now) {
  windowEvents++
  perMinute[minuteOf(now)].window_events++
  lastWindowEvent = now
  pending = true
  if (debounce) clearTimeout(debounce)
  debounce = setTimeout(() => {
    pending = false
    statusRuns++
    perMinute[minuteOf(performance.now())].status_runs++
  }, debounceMs)
}

function line(text) {
  const at = text.indexOf(">>")
  const name = at >= 0 ? text.slice(0, at) : text
  if (!/^[a-z0-9_]+$/.test(name)) return
  counts[name] = (counts[name] || 0) + 1
  if (name === "openwindow" || name === "closewindow") windowEvent(performance.now())
}

function finish(reason) {
  if (debounce) clearTimeout(debounce)
  // A debounce still pending at the end would have run once.
  if (pending) {
    statusRuns++
    perMinute[minuteOf(performance.now())].status_runs++
  }
  const elapsedS = Math.round((performance.now() - started) / 1000)
  const report = {
    started_at: startedAt.toISOString(),
    finished_at: new Date().toISOString(),
    elapsed_s: elapsedS,
    ended: reason,
    debounce_ms: debounceMs,
    events_by_name: Object.fromEntries(Object.entries(counts).sort((a, b) => b[1] - a[1])),
    window_events: windowEvents,
    status_runs: statusRuns,
    status_runs_per_hour: elapsedS > 0 ? Math.round((statusRuns * 3600) / elapsedS * 10) / 10 : null,
    busiest_minute: perMinute.reduce((best, m) => (m.window_events > best.window_events ? m : best), { window_events: 0, status_runs: 0 }),
    per_minute: perMinute,
  }
  require("fs").writeFileSync(out, JSON.stringify(report, null, 2) + "\n")
  console.error(`window-events: ${windowEvents} window events, ${statusRuns} status runs in ${elapsedS} s → ${out}`)
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
