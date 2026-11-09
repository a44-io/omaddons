// scripts/widget-bench.sh's runner: lets the real Service.qml, under qmltestrunner, run
// the real binary. qmltestrunner cannot start processes, so the bench's Process
// stand-in (qml/imports/Quickshell/Io/Process.qml) posts each argv here, and this
// server runs it and answers with its exit code, stdout and stderr lines.
//
// It runs one binary only, the one it was started with, offline (OMADDONS_OFFLINE=1),
// with the environment the widget gave the child laid over a fixed base, and only the
// reads the widget makes (READS). The bench is read-only: the after-start checkpoint the
// widget asks for (`backup create --auto`) is answered as skipped without running, and
// any other command that changes something is refused. Anything that is not the binary
// (the doorbell's inotifywait, a desktop tool) is refused too; the stand-in never sends
// it. It listens on 127.0.0.1 only, on a port the kernel picks, written to --port-file.
//
// GET /log answers every run so far: its words, exit code and the binary's own wall
// time, measured here around the spawn, so the bench can tell the binary's time from
// the widget's. GET /quit stops it.
//
// Usage: bun runner.js --binary ABS --tool-version V --env-file FILE --port-file FILE

const fs = require("fs")

const args = process.argv.slice(2)
function option(name) {
  const at = args.indexOf(name)
  return at >= 0 && at + 1 < args.length ? args[at + 1] : ""
}
const binary = option("--binary")
const toolVersion = option("--tool-version")
const envFile = option("--env-file")
const portFile = option("--port-file")
if (!binary.startsWith("/") || !toolVersion || !envFile || !portFile) {
  console.error("usage: bun runner.js --binary ABS --tool-version V --env-file FILE --port-file FILE")
  process.exit(2)
}

// The command words after the widget's standard flags (Model.standardFlags: the binary,
// --json, --actor widget, --reason <label>, then --progress json, --dry-run and
// --if-match <token> when present).
function commandWords(argv) {
  let i = 6
  while (i < argv.length) {
    if (argv[i] === "--progress" || argv[i] === "--if-match") i += 2
    else if (argv[i] === "--dry-run") i += 1
    else break
  }
  return argv.slice(i)
}

// The reads the widget makes, by command path.
const READS = ["status", "list", "errors", "backup list", "search", "history", "info", "outdated"]
function pathOf(words) {
  if (words[0] === "backup" || words[0] === "errors") return words[1] && !words[1].startsWith("-") ? `${words[0]} ${words[1]}` : words[0]
  return words[0] || ""
}

function envelope(command, data, error) {
  const ok = !error
  return JSON.stringify({
    ok, schema_version: "1.1", tool_version: toolVersion, command, exit_code: ok ? 0 : 4,
    ...(ok ? { partial: false, data } : { error }),
    warnings: [], commands: [], meta: { elapsed_ms: 0, offline: true },
  })
}

// The bench home's environment (bench.env: HOME, the XDG directories, TZ), then PATH.
const base = { PATH: process.env.PATH || "/usr/bin:/bin" }
for (const line of fs.readFileSync(envFile, "utf8").split("\n")) {
  const at = line.indexOf("=")
  if (at > 0) base[line.slice(0, at)] = line.slice(at + 1)
}
base.OMADDONS_OFFLINE = "1"

const origin = performance.now()
const runs = []

async function run(body) {
  const command = Array.isArray(body.command) ? body.command.map(String) : []
  if (command[0] !== binary) {
    return { refused: true, code: 127, stdout: "", stderr: [] }
  }
  const words = commandWords(command)
  const path = pathOf(words)
  if (path === "backup create" && words.includes("--auto")) {
    runs.push({ words: words.join(" "), code: 0, answered: "skipped" })
    return { code: 0, stdout: envelope("backup create", { dry_run: false, run: null, targets: [] }), stderr: [] }
  }
  if (!READS.includes(path) || command.includes("--dry-run")) {
    runs.push({ words: words.join(" "), code: 4, answered: "refused" })
    const error = { code: "usage", message: `widget-bench runs reads only, not ${path}`, fix: null, fix_argv: null, alternatives: [], retryable: false, details: {} }
    return { code: 4, stdout: envelope(path, null, error), stderr: [] }
  }
  const env = { ...base }
  const overlay = body.environment && typeof body.environment === "object" ? body.environment : {}
  for (const [key, value] of Object.entries(overlay)) env[key] = String(value)
  env.OMADDONS_OFFLINE = "1"
  const started = performance.now()
  const child = Bun.spawn(command, { env, stdin: "ignore", stdout: "pipe", stderr: "pipe" })
  const [stdout, stderr, code] = await Promise.all([
    new Response(child.stdout).text(),
    new Response(child.stderr).text(),
    child.exited,
  ])
  const ended = performance.now()
  runs.push({
    words: words.join(" "),
    code,
    wall_ms: Math.round((ended - started) * 1000) / 1000,
    started_ms: Math.round((started - origin) * 1000) / 1000,
    ended_ms: Math.round((ended - origin) * 1000) / 1000,
    stdout_bytes: stdout.length,
  })
  return { code, stdout, stderr: stderr.split("\n").filter((l) => l !== "") }
}

const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  async fetch(request) {
    const url = new URL(request.url)
    if (request.method === "POST" && url.pathname === "/run") {
      // An argv and its environment: a few kilobytes.
      const text = await request.text()
      if (text.length > 65536) return new Response("too large", { status: 413 })
      return Response.json(await run(JSON.parse(text)))
    }
    if (url.pathname === "/log") return Response.json(runs)
    if (url.pathname === "/clear") {
      runs.length = 0
      return Response.json({ ok: true })
    }
    if (url.pathname === "/now") return Response.json({ now_ms: performance.now() - origin })
    if (url.pathname === "/quit") {
      setTimeout(() => process.exit(0), 10)
      return Response.json({ ok: true })
    }
    return new Response("not found", { status: 404 })
  },
})
fs.writeFileSync(portFile, String(server.port))
console.error(`runner: ${binary} on 127.0.0.1:${server.port}`)
