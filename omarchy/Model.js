// The omaddons bar widget's model: every decision the widget makes that is not drawing.
//
// The CLI is the authority (M3-D2, M3-D3): this file reduces `--json` envelopes into
// view models, builds the argv arrays the widget may run, classifies argv the CLI hands
// out, and holds the scheduler's pure decisions (M3-D11). The service (Service.qml)
// only applies them, so bun tests the widget's logic against envelopes the real binary
// writes (M3-D41).
//
// No `.pragma`: each importer gets its own copy. Engine-neutral JavaScript only (no
// QML globals, ES2017 at most, `var`/`function`), so Qt's V4 and bun behave alike. It
// ends with a guarded export for bun's `require`.

// ---------------------------------------------------------------- settings (M3-D10)

// Omarchy ignores manifest defaults, so they live here too; a test holds them equal to
// manifest.json's `barWidget.defaults`.
var DEFAULT_SETTINGS = {
  "binary": "",
  "panelWidth": 540,
  "autoCheck": "On",
  "notifyUpdates": "Off",
  "notifyErrors": "On",
  "refreshIntervalSec": 300
}

// What each setting accepts; anything else falls back to its default.
var SETTING_RULES = {
  "binary": { "type": "path" },
  "panelWidth": { "type": "integer", "min": 400, "max": 800 },
  "autoCheck": { "type": "switch" },
  "notifyUpdates": { "type": "switch" },
  "notifyErrors": { "type": "switch" },
  "refreshIntervalSec": { "type": "integer", "min": 30, "max": 3600 }
}

function isInteger(value) {
  return typeof value === "number" && isFinite(value) && Math.floor(value) === value
}

// `settings[key]` when it is valid for `key`, else the key's default. A missing,
// wrong-typed or out-of-range value never reaches the widget.
function setting(settings, key) {
  var rule = SETTING_RULES[key]
  if (!rule) return undefined
  var value = settings && typeof settings === "object" ? settings[key] : undefined
  if (value === undefined || value === null) return DEFAULT_SETTINGS[key]
  if (rule.type === "integer") {
    return isInteger(value) && value >= rule.min && value <= rule.max ? value : DEFAULT_SETTINGS[key]
  }
  if (rule.type === "switch") {
    if (value === "On" || value === "Off") return value
    if (value === true) return "On"
    if (value === false) return "Off"
    return DEFAULT_SETTINGS[key]
  }
  // A path is empty (the stamped binary) or absolute; a bare name would go to PATH.
  return typeof value === "string" && (value === "" || value.charAt(0) === "/") ? value : DEFAULT_SETTINGS[key]
}

// An integer setting with explicit bounds: `def` unless `settings[key]` is an integer
// in [min, max]. BarWidget.qml's thin wrapper calls this with its own `settings`.
function intSetting(settings, key, def, min, max) {
  var value = settings && typeof settings === "object" ? settings[key] : undefined
  return isInteger(value) && value >= min && value <= max ? value : def
}

// An On/Off setting as a boolean: `def` unless `settings[key]` is "On", "Off" or a
// boolean.
function boolSetting(settings, key, def) {
  var value = settings && typeof settings === "object" ? settings[key] : undefined
  if (value === "On" || value === true) return true
  if (value === "Off" || value === false) return false
  return def
}

// ------------------------------------------------------------- constants (§7.11)

// QML cannot read src/limits.rs, so the widget's own limits live here.
// A local read's deadline from its start; a plain UI choice.
var WIDGET_LOCAL_DEADLINE_SEC = 20
// A network read's inactivity deadline, reset by every progress line. tests/
// omarchy_widget.rs holds it above the longest request politeness.rs allows.
var WIDGET_NETWORK_DEADLINE_SEC = 600
// SIGTERM first, then signal(9) this much later; a plain UI choice.
var WIDGET_KILL_GRACE_SEC = 5
// stdout above this is discarded as `oversized`; tests/omarchy_widget.rs holds every
// template's worst-case output below it.
var WIDGET_OUTPUT_MAX_BYTES = 8 * 1024 * 1024
// How long a finished run's stdout may stay open after `exited`: a killed omaddons can
// leave a child (git) holding the pipe, and the lane must not wait on it forever.
var WIDGET_STREAM_GRACE_SEC = 2

// ------------------------------------------------------ the templates (M3-D2)

// BEGIN COMMANDS: checked by tests/omarchy_widget.rs; every entry must parse as an omaddons command line
// F1 ships the entries for commands that exist at F1 time; F8 adds errors_target and backup_auto (B6, D3);
// F6 adds errors, errors_raw and dismiss_errors (B6, B8); F9 adds backups, backup_target, backup_all,
// restore, restore_addon and prune_backup (D2, D4, D7, D8).
var COMMANDS = {
  "status":         { "argv": ["status", "--cached", "-v"],                         "mutates": false, "lane": "local" },
  "status_full":    { "argv": ["status", "-v"],                                     "mutates": false, "lane": "local" },
  "list":           { "argv": ["list", "-v"],                                       "mutates": false, "lane": "local" },
  "changelog":      { "argv": ["info", "-t", "{target}", "--changelog", "--", "{addon}"], "mutates": false, "lane": "network" },
  "search":         { "argv": ["search", "--limit", "20", "-t", "{target}", "--", "{query}"], "mutates": false, "lane": "network" },
  "check":          { "argv": ["outdated"],                                         "mutates": false, "lane": "network" },
  "update_all":     { "argv": ["update"],                                           "mutates": true },
  "update_target":  { "argv": ["update", "-t", "{target}"],                         "mutates": true },
  "update_addon":   { "argv": ["update", "-t", "{target}", "--", "{addon}"],        "mutates": true },
  "reinstall":      { "argv": ["install", "-t", "{target}", "--reinstall", "--", "{addon}"], "mutates": true },
  "remove":         { "argv": ["remove", "-t", "{target}", "--", "{addon}"],        "mutates": true, "confirm": true },
  "pin":            { "argv": ["pin", "-t", "{target}", "--", "{addon}"],           "mutates": true },
  "unpin":          { "argv": ["unpin", "-t", "{target}", "--", "{addon}"],         "mutates": true },
  "install":        { "argv": ["install", "-t", "{target}", "--", "{ref}"],         "mutates": true }
}
// END COMMANDS

// One row per command path of the binary's registry (`app::COMMANDS`); a command task
// adds its row with its registry row, and tests/omarchy_widget.rs holds them equal.
// `destructive` paths always go through a preview; `binds` paths take `--if-match`.
// BEGIN CLI_PATHS
var CLI_PATHS = {
  "status":          { "mutates": false, "destructive": false, "binds": false },
  "list":            { "mutates": false, "destructive": false, "binds": false },
  "outdated":        { "mutates": false, "destructive": false, "binds": false },
  "info":            { "mutates": false, "destructive": false, "binds": false },
  "search":          { "mutates": false, "destructive": false, "binds": false },
  "errors":          { "mutates": false, "destructive": false, "binds": false },
  "errors dismiss":  { "mutates": true,  "destructive": false, "binds": true },
  "export":          { "mutates": true,  "destructive": false, "binds": false },
  "history":         { "mutates": false, "destructive": false, "binds": false },
  "net-log":         { "mutates": false, "destructive": false, "binds": false },
  "install":         { "mutates": true,  "destructive": false, "binds": false },
  "update":          { "mutates": true,  "destructive": false, "binds": false },
  "pin":             { "mutates": true,  "destructive": false, "binds": false },
  "unpin":           { "mutates": true,  "destructive": false, "binds": false },
  "adopt":           { "mutates": true,  "destructive": false, "binds": false },
  "sync":            { "mutates": true,  "destructive": false, "binds": false },
  "copy":            { "mutates": true,  "destructive": false, "binds": false },
  "remove":          { "mutates": true,  "destructive": true,  "binds": true },
  "undo":            { "mutates": true,  "destructive": true,  "binds": true },
  "backup":          { "mutates": true,  "destructive": false, "binds": false },
  "backup list":     { "mutates": false, "destructive": false, "binds": false },
  "backup create":   { "mutates": true,  "destructive": false, "binds": false },
  "backup restore":  { "mutates": true,  "destructive": true,  "binds": true },
  "backup prune":    { "mutates": true,  "destructive": true,  "binds": true },
  "backup export":   { "mutates": true,  "destructive": false, "binds": false },
  "backup import":   { "mutates": true,  "destructive": false, "binds": false },
  "backup verify":   { "mutates": true,  "destructive": false, "binds": false },
  "targets":         { "mutates": false, "destructive": false, "binds": false },
  "targets add":     { "mutates": true,  "destructive": false, "binds": false },
  "targets rm":      { "mutates": true,  "destructive": true,  "binds": false },
  "targets rename":  { "mutates": true,  "destructive": false, "binds": false },
  "targets default": { "mutates": true,  "destructive": false, "binds": false },
  "config":          { "mutates": false, "destructive": false, "binds": false },
  "config get":      { "mutates": false, "destructive": false, "binds": false },
  "config set":      { "mutates": true,  "destructive": false, "binds": false },
  "config path":     { "mutates": false, "destructive": false, "binds": false },
  "auth":            { "mutates": true,  "destructive": false, "binds": false },
  "omarchy":         { "mutates": true,  "destructive": false, "binds": false },
  "omarchy install": { "mutates": true,  "destructive": false, "binds": false },
  "help":            { "mutates": false, "destructive": false, "binds": false }
}
// END CLI_PATHS

// ------------------------------------------------------------- building argv

// The flags the widget owns: it sets them on every run, and a CLI-provided argv that
// carries one could re-attribute itself or skip a preview.
var WIDGET_FLAGS = ["--json", "--robot", "--format", "--actor", "--reason", "--progress", "--if-match"]
var SLOTS = { "target": true, "addon": true, "ref": true, "backup": true, "query": true }
var TARGET_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._@-]*$/
var TOKEN_PATTERN = /^pc1:[0-9a-f]{64}$/
// What `Paths::from_env` reads, so a child given them resolves the installing run's
// state (M3-D7).
var CONTEXT_KEYS = ["HOME", "XDG_CONFIG_HOME", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME"]
var LABEL_MAX_CHARS = 120

// Every refusal is an Error whose `reason` names the rule, and whose message starts
// with it.
function refuse(reason, message) {
  var error = new Error(reason + ": " + message)
  error.reason = reason
  return error
}

function flagName(arg) {
  var at = arg.indexOf("=")
  return at === -1 ? arg : arg.slice(0, at)
}

// The command path an argument list starts with (no program name): the longest
// CLI_PATHS key formed by its leading words, or "" when they form none.
function pathOf(args) {
  var words = []
  for (var i = 0; i < args.length && words.length < 2; i++) {
    if (typeof args[i] !== "string" || args[i] === "" || args[i].charAt(0) === "-") break
    words.push(args[i])
  }
  if (words.length === 2 && CLI_PATHS.hasOwnProperty(words[0] + " " + words[1])) return words[0] + " " + words[1]
  if (words.length >= 1 && CLI_PATHS.hasOwnProperty(words[0])) return words[0]
  return ""
}

// A template's path and slots, after checking its shape: command words first, no flag
// the widget owns, every positional slot after `--`, every option slot in the
// one-element `--opt={slot}` form before it. `table` replaces COMMANDS (the service's
// tests add fixture templates over existing commands).
function templateShape(name, table) {
  var commands = table || COMMANDS
  var template = commands.hasOwnProperty(name) ? commands[name] : null
  if (!template) throw refuse("unknown_template", name)
  var argv = template.argv
  var path = pathOf(argv)
  if (!path) throw refuse("bad_template", name + " starts with no command path")
  var separator = argv.indexOf("--")
  var slots = []
  for (var i = 0; i < argv.length; i++) {
    var part = argv[i]
    if (i !== separator && part.charAt(0) === "-") {
      if (separator !== -1 && i > separator) throw refuse("bad_template", name + " has a flag after --")
      if (WIDGET_FLAGS.indexOf(flagName(part)) !== -1 || flagName(part) === "--dry-run") {
        throw refuse("bad_template", name + " sets " + flagName(part) + ", which the widget owns")
      }
    }
    var open = part.indexOf("{")
    if (open === -1) continue
    var close = part.indexOf("}", open)
    var slot = close === -1 ? "" : part.slice(open + 1, close)
    if (!SLOTS.hasOwnProperty(slot) || close !== part.length - 1) {
      throw refuse("bad_template", name + " has a malformed slot in " + part)
    }
    var positional = open === 0
    var option = open > 0 && part.slice(0, 2) === "--" && part.charAt(open - 1) === "="
    // `-t {target}` is the one value in its own element: no target starts with `-`.
    var targetValue = positional && slot === "target" && i > 0 && (argv[i - 1] === "-t" || argv[i - 1] === "--target")
    if (positional && !targetValue && (separator === -1 || i < separator)) {
      throw refuse("bad_template", name + " has the positional {" + slot + "} before --")
    }
    if (!positional && !(option && (separator === -1 || i < separator))) {
      throw refuse("bad_template", name + " has {" + slot + "} outside --opt={slot}")
    }
    if (slot === "query" && template.mutates) {
      throw refuse("bad_template", name + " takes free text but mutates")
    }
    slots.push(slot)
  }
  return { path: path, slots: slots }
}

function checkSlot(name, slot, value, template) {
  if (typeof value !== "string" || value === "") throw refuse("bad_value", name + ": {" + slot + "} is empty")
  if (value.indexOf("\u0000") !== -1) throw refuse("bad_value", name + ": {" + slot + "} holds a NUL")
  if (slot === "target" && !TARGET_PATTERN.test(value)) {
    throw refuse("bad_target", name + ": " + JSON.stringify(value) + " is no target id or key")
  }
  if (slot === "query" && template.mutates) throw refuse("query_in_mutation", name)
}

function binaryOf(opts) {
  if (typeof opts.binary !== "string" || opts.binary.charAt(0) !== "/") {
    throw refuse("bad_binary", "the binary must be an absolute path")
  }
  return opts.binary
}

// `--reason`: "<button label> (bar widget)".
function reasonOf(opts) {
  var label = opts.label
  if (typeof label !== "string" || label === "" || label.length > LABEL_MAX_CHARS || /[\r\n]/.test(label)) {
    throw refuse("bad_label", "a button label is one line of 1 to " + LABEL_MAX_CHARS + " characters")
  }
  return label + " (bar widget)"
}

// A copy of the stamped context, the child's environment overlay.
function contextOf(context) {
  if (!context || typeof context !== "object") throw refuse("bad_context", "no context")
  var env = {}
  for (var i = 0; i < CONTEXT_KEYS.length; i++) {
    var key = CONTEXT_KEYS[i]
    var value = context[key]
    if (typeof value !== "string" || value.charAt(0) !== "/") {
      throw refuse("bad_context", key + " must be an absolute path")
    }
    env[key] = value
  }
  return env
}

// The `--if-match` value a run carries ("" for none), after the preview rules (M3-D9,
// M3-D46): a destructive run that is no preview needs its preview's token when its
// path binds, or an explicit confirmation when it does not; a `bind` run needs the
// token of the view it acts on. A token is a preview's fingerprint, no secret.
function ifMatchFor(what, row, needsConfirm, needsBinding, opts, preview) {
  var given = opts.token
  if (given !== undefined && given !== null && typeof given !== "string") throw refuse("bad_token", what)
  var ifMatch = typeof given === "string" ? given : ""
  var present = ifMatch.length > 0
  if (present) {
    if (!TOKEN_PATTERN.test(ifMatch)) throw refuse("bad_token", what)
    if (!row.binds) throw refuse("token_not_bound", what + " takes no --if-match")
    if (preview) throw refuse("token_in_preview", what)
  }
  if (!preview && (needsConfirm || row.destructive) && (row.binds ? !present : opts.confirmed !== true)) {
    throw refuse("destructive_without_preview", what + " runs only after its preview was confirmed")
  }
  if (!preview && needsBinding && !present) throw refuse("token_required", what)
  return ifMatch
}

// The flags every run starts with, right after the binary (M3-D2).
function standardFlags(opts, progress, preview, ifMatch) {
  var argv = [binaryOf(opts), "--json", "--actor", "widget", "--reason", reasonOf(opts)]
  if (progress) argv.push("--progress", "json")
  if (preview) argv.push("--dry-run")
  if (ifMatch.length > 0) argv.push("--if-match", ifMatch)
  return argv
}

// Template `name` with `slots` filled: `{argv, env}`, where `argv` starts with the
// binary and the standard flags, and `env` is the stamped context. Options: `binary`,
// `label` (the button's), `context`, `preview`, `token` or `confirmed` for the preview
// flow, and `templates` in place of COMMANDS. Throws on anything the template does not
// allow.
function buildArgv(name, slots, opts) {
  var o = opts || {}
  var shape = templateShape(name, o.templates)
  var template = (o.templates || COMMANDS)[name]
  var given = slots || {}
  for (var key in given) {
    if (!given.hasOwnProperty(key)) continue
    if (!SLOTS.hasOwnProperty(key)) throw refuse("unknown_slot", name + ": {" + key + "}")
    if (shape.slots.indexOf(key) === -1) throw refuse("unknown_slot", name + " has no {" + key + "}")
  }
  var filled = []
  for (var i = 0; i < template.argv.length; i++) {
    var part = template.argv[i]
    var open = part.indexOf("{")
    if (open === -1) {
      filled.push(part)
      continue
    }
    var slot = part.slice(open + 1, part.length - 1)
    if (!given.hasOwnProperty(slot)) throw refuse("unfilled_slot", name + ": {" + slot + "}")
    checkSlot(name, slot, given[slot], template)
    // Slicing, never String.replace, whose `$&` patterns would rewrite the value.
    filled.push(part.slice(0, open) + given[slot])
  }
  var row = CLI_PATHS[shape.path]
  var preview = o.preview === true
  var ifMatch = ifMatchFor(name, row, template.confirm === true, template.bind === true, o, preview)
  var progress = template.mutates === true || name === "check"
  return { argv: standardFlags(o, progress, preview, ifMatch).concat(filled), env: contextOf(o.context) }
}

// ---------------------------------------------------------- argv from the CLI

function hasDryRun(args) {
  for (var i = 0; i < args.length && args[i] !== "--"; i++) {
    if (args[i] === "--dry-run") return true
  }
  return false
}

// Whether an argv the CLI handed out (`fix_argv`, alternatives, `commands[]`, issue
// remediations, `last_change.undo`) may become a button (M3-D2 (a)): `omaddons`, a
// known path, and no flag the widget owns before `--`. The verdict reports the path's
// flags, so the runner previews every destructive one.
function classifyCliArgv(argv) {
  var verdict = { "ok": false, "path": "", "mutates": false, "destructive": false, "binds": false, "reason": "" }
  if (!Array.isArray(argv) || argv.length < 2) {
    verdict.reason = "malformed"
    return verdict
  }
  for (var i = 0; i < argv.length; i++) {
    if (typeof argv[i] !== "string" || argv[i].indexOf("\u0000") !== -1) {
      verdict.reason = "malformed"
      return verdict
    }
  }
  if (argv[0] !== "omaddons") {
    verdict.reason = "not_omaddons"
    return verdict
  }
  var end = argv.indexOf("--")
  if (end === -1) end = argv.length
  for (var j = 1; j < end; j++) {
    if (argv[j].charAt(0) === "-" && WIDGET_FLAGS.indexOf(flagName(argv[j])) !== -1) {
      verdict.reason = "widget_flag"
      return verdict
    }
  }
  var path = pathOf(argv.slice(1, end))
  if (!path) {
    verdict.reason = "unknown_path"
    return verdict
  }
  var row = CLI_PATHS[path]
  verdict.ok = true
  verdict.path = path
  verdict.mutates = row.mutates
  verdict.destructive = row.destructive
  verdict.binds = row.binds
  return verdict
}

// An accepted CLI argv as `{argv, env}`: the binary for `omaddons` and the standard
// flags right after it. The options are buildArgv's; a destructive path runs only as a
// preview or after one.
function cliArgv(argv, opts) {
  var verdict = classifyCliArgv(argv)
  if (!verdict.ok) throw refuse(verdict.reason, "not a command the widget runs")
  var o = opts || {}
  var rest = argv.slice(1)
  var dryRun = hasDryRun(rest)
  var preview = o.preview === true || dryRun
  var ifMatch = ifMatchFor(verdict.path, CLI_PATHS[verdict.path], false, false, o, preview)
  var progress = verdict.mutates || verdict.path === "outdated"
  return { argv: standardFlags(o, progress, preview && !dryRun, ifMatch).concat(rest), env: contextOf(o.context) }
}

// --------------------------------------------------------- compatibility (M3-D42)

function envelopeOk(env) {
  return !!env && typeof env === "object" && !Array.isArray(env)
    && typeof env.ok === "boolean" && typeof env.schema_version === "string"
    && typeof env.tool_version === "string" && typeof env.command === "string"
}

// The envelope on a run's stdout (one compact line), or null.
function parseEnvelope(stdout) {
  var text = typeof stdout === "string" ? stdout.trim() : ""
  if (text === "") return null
  var lines = text.split("\n")
  try {
    var env = JSON.parse(lines[lines.length - 1])
    return envelopeOk(env) ? env : null
  } catch (e) {
    return null
  }
}

function schemaMajor(version) {
  var match = /^([0-9]+)\./.exec(version)
  return match ? parseInt(match[1], 10) : -1
}

// Whether an envelope came from the binary this widget was built for: schema 1.x, the
// stamped tool version, and an installed widget that is not outdated. `build` is
// Build.js.
function compatible(env, build) {
  if (!envelopeOk(env) || !build || schemaMajor(env.schema_version) !== 1) return false
  if (env.tool_version !== build.TOOL_VERSION) return false
  var widget = env.data && typeof env.data === "object" ? env.data.widget : null
  return !(widget && widget.outdated === true)
}

// The banner of the read-only incompatible state.
function incompatibleText(env, build) {
  var mine = build && build.TOOL_VERSION ? build.TOOL_VERSION : "an unknown build"
  if (!envelopeOk(env)) return "The binary's answer is not one this widget reads (it is from omaddons " + mine + "); reinstall the widget"
  if (env.tool_version !== mine) return "The widget is from omaddons " + mine + ", the binary is " + env.tool_version + "; reinstall the widget"
  return "The widget's files differ from the binary's; reinstall the widget"
}

// --------------------------------------------------------- the scheduler (M3-D11)

// A request is `{id, contextGeneration, target, resource, kind, priority, selection}`:
// `id` grows with every request, `resource` is a template name (or a CLI path), `kind`
// is "data", "view" or "mutation", `priority` is "user" or "background", and
// `selection` is what a view-bound read was asked for. An optional `lane` overrides
// the template's.
var NETWORK_QUEUE_MAX = 4

function emptyQueues() {
  return { "mutation": [], "local": [], "network": [] }
}

function laneOf(request) {
  if (request.lane === "mutation" || request.lane === "local" || request.lane === "network") return request.lane
  if (request.kind === "mutation") return "mutation"
  var template = COMMANDS.hasOwnProperty(request.resource) ? COMMANDS[request.resource] : null
  if (template) return template.mutates ? "mutation" : template.lane
  return "local"
}

// The key a request coalesces under, and against which a result is "newer".
function coalesceKey(request) {
  return request.resource + "|" + (request.target || "")
}

function copyQueues(queues) {
  var out = emptyQueues()
  for (var lane in out) out[lane] = queues && queues[lane] ? queues[lane].slice() : []
  return out
}

function indexWhere(list, test) {
  for (var i = 0; i < list.length; i++) if (test(list[i])) return i
  return -1
}

// `request` queued under the lane rules: `{queues, accepted, replaced}`, where
// `replaced` is the id of a queued request it took the place of. Mutations: at most
// one queued user request (a newer one replaces it) and one background request per
// key. Local reads: one per key, the newest replacing the queued one. Network reads:
// a newer search replaces a queued one, other reads coalesce per key, at most
// NETWORK_QUEUE_MAX queued.
function enqueue(queues, request) {
  var out = copyQueues(queues)
  var lane = laneOf(request)
  var list = out[lane]
  var key = coalesceKey(request)
  var at = -1
  if (lane === "mutation") {
    at = request.priority === "user"
      ? indexWhere(list, function(r) { return r.priority === "user" })
      : indexWhere(list, function(r) { return r.priority !== "user" && coalesceKey(r) === key })
  } else if (lane === "network" && request.resource === "search") {
    at = indexWhere(list, function(r) { return r.resource === "search" })
  } else {
    at = indexWhere(list, function(r) { return coalesceKey(r) === key })
  }
  if (at !== -1) {
    var replaced = list[at].id
    var next = request
    // A newer cached status never downgrades a queued full one (the panel asked for
    // the rescan).
    if (list[at].template === "status_full" && request.template === "status") {
      next = {}
      for (var field in request) if (request.hasOwnProperty(field)) next[field] = request[field]
      next.template = "status_full"
    }
    list[at] = next
    return { "queues": out, "accepted": true, "replaced": replaced }
  }
  if (lane === "network" && list.length >= NETWORK_QUEUE_MAX) {
    return { "queues": copyQueues(queues), "accepted": false, "replaced": null }
  }
  list.push(request)
  return { "queues": out, "accepted": true, "replaced": null }
}

// The request each free lane starts next: `{start: {mutation, local, network},
// queues}`. User mutations go before background ones (none preempts a running one),
// and `status` jumps the local queue.
function nextRequest(queues, lanesBusy) {
  var out = copyQueues(queues)
  var busy = lanesBusy || {}
  var start = { "mutation": null, "local": null, "network": null }
  for (var lane in start) {
    var list = out[lane]
    if (busy[lane] || list.length === 0) continue
    var at = 0
    if (lane === "mutation") at = Math.max(0, indexWhere(list, function(r) { return r.priority === "user" }))
    if (lane === "local") at = Math.max(0, indexWhere(list, function(r) { return r.resource === "status" }))
    start[lane] = list.splice(at, 1)[0]
  }
  return { "start": start, "queues": out }
}

function jsonEqual(a, b) {
  if (a === b) return true
  if (a === null || b === null || typeof a !== "object" || typeof b !== "object") return false
  if (Array.isArray(a) !== Array.isArray(b)) return false
  var keysA = Object.keys(a)
  var keysB = Object.keys(b)
  if (keysA.length !== keysB.length) return false
  for (var i = 0; i < keysA.length; i++) {
    if (!b.hasOwnProperty(keysA[i]) || !jsonEqual(a[keysA[i]], b[keysA[i]])) return false
  }
  return true
}

// Whether a finished request's result may be applied. Every result needs the current
// context generation. A mutation's always shows. A data result applies when it is
// newer than the last applied for its key, even with a newer request still queued, so
// a busy lane never starves the bar. A view-bound result applies only while the view
// still shows what it was asked for.
function acceptResult(request, lastApplied, currentSelection, contextGeneration) {
  if (!request || request.contextGeneration !== contextGeneration) return false
  if (request.kind === "mutation") return true
  var applied = lastApplied ? lastApplied[coalesceKey(request)] : undefined
  if (typeof applied === "number" && request.id <= applied) return false
  if (request.kind === "view") return jsonEqual(request.selection, currentSelection)
  return true
}

// `lastApplied` with `request` recorded as applied.
function markApplied(lastApplied, request) {
  var out = {}
  for (var key in lastApplied) if (lastApplied.hasOwnProperty(key)) out[key] = lastApplied[key]
  out[coalesceKey(request)] = request.id
  return out
}

function utf8Length(text) {
  var bytes = 0
  for (var i = 0; i < text.length; i++) {
    var c = text.charCodeAt(i)
    if (c < 0x80) bytes += 1
    else if (c < 0x800) bytes += 2
    else if (c >= 0xd800 && c <= 0xdbff) {
      bytes += 4
      i++
    } else bytes += 3
  }
  return bytes
}

// How a run ended: "missing" (never started), "cancelled", "timeout", "oversized"
// (stdout over WIDGET_OUTPUT_MAX_BYTES), "malformed" (no envelope), "incompatible"
// (when `build` is given and the envelope is from another build), "failed" (an
// envelope with `ok: false`) or "ok". A missing binary never emits `exited` (F0), and
// a signal death has no envelope, so neither relies on `exitCode`.
function outcomeOf(exitCode, started, stdout, timedOut, cancelled, build) {
  if (!started) return "missing"
  if (cancelled) return "cancelled"
  if (timedOut) return "timeout"
  var text = typeof stdout === "string" ? stdout : ""
  if (text.length > WIDGET_OUTPUT_MAX_BYTES || utf8Length(text) > WIDGET_OUTPUT_MAX_BYTES) return "oversized"
  var env = parseEnvelope(text)
  if (!env) return "malformed"
  if (build && !compatible(env, build)) return "incompatible"
  return env.ok ? "ok" : "failed"
}

// -------------------------------------------------------------- the reducers

function plural(n, one, many) {
  return n + " " + (n === 1 ? one : (many || one + "s"))
}

function joinNames(names) {
  if (names.length <= 1) return names.join("")
  return names.slice(0, -1).join(", ") + " and " + names[names.length - 1]
}

function num(value) {
  return typeof value === "number" && isFinite(value) ? value : 0
}

function str(value) {
  return typeof value === "string" ? value : ""
}

function strOrNull(value) {
  return typeof value === "string" ? value : null
}

function stringList(value) {
  var out = []
  if (Array.isArray(value)) for (var i = 0; i < value.length; i++) if (typeof value[i] === "string") out.push(value[i])
  return out
}

// A CLI-provided argv as a button, or as text when the classifier refuses it.
function cliAction(command, argv, destructive) {
  var verdict = Array.isArray(argv) ? classifyCliArgv(argv) : null
  return {
    "command": str(command),
    "argv": verdict && verdict.ok ? argv.slice() : null,
    "destructive": destructive === true || (verdict !== null && verdict.destructive),
    "runnable": verdict !== null && verdict.ok
  }
}

function issueOf(issue) {
  var severity = str(issue.severity)
  var remediation = issue.remediation && typeof issue.remediation === "object" ? issue.remediation : {}
  return {
    "id": str(issue.id),
    "severity": severity,
    "urgent": severity === "P0" || severity === "P1",
    "title": str(issue.title),
    "addon": strOrNull(issue.addon),
    "remediation": cliAction(remediation.command, remediation.argv, remediation.destructive)
  }
}

function summaryOf(summary) {
  if (!summary || typeof summary !== "object") return null
  var issues = summary.issues || {}
  var counts = { "p0": num(issues.p0), "p1": num(issues.p1), "p2": num(issues.p2), "p3": num(issues.p3) }
  var updates = summary.updates || {}
  return {
    "state": str(summary.state),
    "targets": num(summary.targets),
    "running": stringList(summary.running),
    "updates": { "available": num(updates.available), "targets": stringList(updates.targets) },
    "issues": counts,
    "issuesTotal": counts.p0 + counts.p1 + counts.p2 + counts.p3,
    "issuesUrgent": counts.p0 + counts.p1,
    // Absent until the binary reports it: not yet known, never zero.
    "errors": summary.errors && typeof summary.errors === "object"
      ? { "new": num(summary.errors["new"]), "targets": stringList(summary.errors.targets) } : null,
    "checkDue": summary.check_due === true,
    "widgetOutdated": summary.widget_outdated === true
  }
}

function targetOf(target) {
  var updates = target.updates && typeof target.updates === "object" ? target.updates : null
  var errors = target.errors && typeof target.errors === "object" ? target.errors : null
  var backups = target.backups && typeof target.backups === "object" ? target.backups : null
  var issues = []
  if (Array.isArray(target.issues)) for (var i = 0; i < target.issues.length; i++) issues.push(issueOf(target.issues[i]))
  return {
    "id": str(target.id),
    "key": str(target.key),
    "label": str(target.label) || str(target.id),
    "running": target.running === true,
    "missing": target.missing === true,
    "addons": target.addons && typeof target.addons === "object" ? num(target.addons.total) : null,
    "updates": updates ? {
      "available": num(updates.available), "pinned": num(updates.pinned), "blocked": num(updates.blocked),
      "failed": num(updates.failed), "unchecked": num(updates.unchecked),
      "checkedAt": strOrNull(updates.checked_at), "stale": updates.stale === true
    } : null,
    "issues": issues,
    "errors": errors ? {
      "state": str(errors.state), "coverage": str(errors.coverage), "new": num(errors["new"]),
      "total": num(errors.total), "addons": num(errors.addons),
      "top": Array.isArray(errors.top) ? errors.top.map(function(t) {
        return { "addon": str(t.addon), "folder": str(t.folder), "new": num(t["new"]), "count": num(t.count) }
      }) : [],
      "lastSessionAt": strOrNull(errors.last_session_at)
    } : null,
    "backups": backups ? {
      "count": num(backups.count), "stale": backups.stale === true,
      "latest": backups.latest && typeof backups.latest === "object" ? {
        "id": str(backups.latest.id), "capturedAt": str(backups.latest.captured_at),
        "kind": str(backups.latest.kind), "trigger": strOrNull(backups.latest.trigger)
      } : null
    } : null
  }
}

var KIND_WORDS = { "install": "install", "update": "update", "remove": "removal", "undo": "undo", "restore": "restore" }

// The last change, or `{unknown: true}` when the history projection could not tell
// (never "nothing to undo"); null when there is none.
function lastChangeOf(change, state) {
  if (state === "unknown") return { "unknown": true }
  if (!change || typeof change !== "object") return null
  var kind = str(change.kind)
  var targets = stringList(change.targets)
  var undo = change.undo && typeof change.undo === "object" ? cliAction(change.undo.command, change.undo.argv, true) : null
  return {
    "unknown": false,
    "run": str(change.run),
    "kind": kind,
    "at": str(change.at),
    "actor": str(change.actor),
    "reason": strOrNull(change.reason),
    "targets": targets,
    "changes": change.changes && typeof change.changes === "object" ? change.changes : {},
    "redo": kind === "undo",
    "label": (kind === "undo" ? "Redo " : "Undo ") + (KIND_WORDS[kind] || kind) + (targets.length ? " on " + joinNames(targets) : ""),
    "undo": undo
  }
}

// "checked 25 min ago".
function ageText(iso, nowMs) {
  var at = Date.parse(iso)
  if (isNaN(at) || typeof nowMs !== "number") return ""
  var minutes = Math.max(0, Math.floor((nowMs - at) / 60000))
  if (minutes < 1) return "checked just now"
  if (minutes < 60) return "checked " + minutes + " min ago"
  var hours = Math.floor(minutes / 60)
  if (hours < 48) return "checked " + plural(hours, "hour") + " ago"
  return "checked " + plural(Math.floor(hours / 24), "day") + " ago"
}

function tooltipOf(summary, targets, nowMs) {
  if (!summary) return ["omaddons has not answered yet"]
  var lines = []
  if (summary.updates.available > 0) {
    var per = []
    for (var i = 0; i < targets.length; i++) {
      if (targets[i].updates && targets[i].updates.available > 0) per.push(targets[i].id + " " + targets[i].updates.available)
    }
    lines.push(plural(summary.updates.available, "update") + " on " + plural(summary.updates.targets.length, "client")
      + (per.length ? " (" + per.join(", ") + ")" : ""))
  }
  if (summary.issuesUrgent > 0) {
    var first = null
    for (var t = 0; t < targets.length && !first; t++) {
      for (var j = 0; j < targets[t].issues.length; j++) {
        if (targets[t].issues[j].urgent) {
          first = targets[t].id + ": " + targets[t].issues[j].title
          break
        }
      }
    }
    lines.push(first ? first + (summary.issuesUrgent > 1 ? " (and " + (summary.issuesUrgent - 1) + " more)" : "")
      : plural(summary.issuesUrgent, "urgent issue"))
  } else if (summary.issuesTotal > 0) {
    lines.push(plural(summary.issuesTotal, "issue"))
  }
  if (summary.errors && summary.errors["new"] > 0) {
    lines.push(plural(summary.errors["new"], "new Lua error") + " on " + joinNames(summary.errors.targets))
  }
  if (summary.running.length > 0) {
    lines.push(joinNames(summary.running) + (summary.running.length === 1 ? " is" : " are")
      + " running: updates land, then /reload")
  }
  if (summary.checkDue) lines.push("An update check is due")
  if (summary.widgetOutdated) lines.push("The widget is out of date; reinstall it")
  var oldest = null
  for (var k = 0; k < targets.length; k++) {
    var checked = targets[k].updates ? targets[k].updates.checkedAt : null
    if (checked && (!oldest || Date.parse(checked) < Date.parse(oldest))) oldest = checked
  }
  var age = oldest ? ageText(oldest, nowMs) : ""
  if (age) lines.push(age)
  if (lines.length === 0 || (lines.length === 1 && age)) lines.unshift("Everything is up to date")
  return lines
}

// `status --cached -v`'s envelope as the bar's model: `{bar, targets, summary,
// lastChange, widget, commands}`. `bar` is `{badge, urgent, dimmed, spinning,
// tooltip[]}`; `spinning` is the service's to set. `nowMs` adds the check age to the
// tooltip. A failed run keeps no data and says why.
function reduceStatus(env, nowMs) {
  if (!envelopeOk(env) || !env.ok || !env.data || typeof env.data !== "object") {
    var message = env && env.error && typeof env.error.message === "string" ? env.error.message : "omaddons did not answer"
    return {
      "bar": { "badge": 0, "urgent": false, "dimmed": true, "spinning": false, "tooltip": [message] },
      "targets": [], "summary": null, "lastChange": null, "widget": null, "commands": []
    }
  }
  var data = env.data
  var summary = summaryOf(data.summary)
  var targets = Array.isArray(data.targets) ? data.targets.map(targetOf) : []
  var commands = []
  if (Array.isArray(env.commands)) {
    for (var i = 0; i < env.commands.length; i++) {
      var c = env.commands[i]
      var action = cliAction(c.command, c.argv, c.destructive)
      action.action = str(c.action)
      commands.push(action)
    }
  }
  return {
    "bar": {
      "badge": summary ? summary.updates.available : 0,
      "urgent": !!summary && (summary.state === "attention" || summary.state === "errors"),
      "dimmed": !summary || targets.length === 0,
      "spinning": false,
      "tooltip": tooltipOf(summary, targets, nowMs)
    },
    "targets": targets,
    "summary": summary,
    "lastChange": lastChangeOf(data.last_change, data.last_change_state),
    // Absent until the binary reports it.
    "widget": data.widget && typeof data.widget === "object" ? data.widget : null,
    "commands": commands
  }
}

// The badge's text: hidden ("") at 0, the count up to 99, then "99+" (past that the
// number only says "a lot").
function badgeText(count) {
  var n = num(count)
  if (n <= 0) return ""
  return n > 99 ? "99+" : String(Math.floor(n))
}

// The bar of a model the service can no longer vouch for (an incompatible binary):
// the last numbers, dimmed and marked stale.
function staleBar(bar) {
  var out = { "badge": 0, "urgent": false, "dimmed": true, "spinning": false, "tooltip": [], "stale": true }
  if (bar && typeof bar === "object") {
    out.badge = num(bar.badge)
    out.urgent = bar.urgent === true
    out.tooltip = Array.isArray(bar.tooltip) ? bar.tooltip.slice() : []
  }
  return out
}

// `list -v`: one row per managed addon, per target.
function reduceList(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || !Array.isArray(env.data.targets)) return { "targets": [] }
  return {
    "targets": env.data.targets.map(function(t) {
      var addons = Array.isArray(t.addons) ? t.addons : []
      return {
        "id": str(t.id),
        "key": str(t.key),
        "label": str(t.label) || str(t.id),
        "counts": t.counts && typeof t.counts === "object" ? t.counts : {},
        "addons": addons.map(function(a) {
          var latest = a.latest && typeof a.latest === "object" ? a.latest : {}
          var drift = a.drift && typeof a.drift === "object" ? a.drift : {}
          return {
            "name": str(a.name),
            "version": strOrNull(a.version),
            "ref": strOrNull(a.ref),
            "pinned": a.pinned === true,
            "state": str(a.state),
            "latest": strOrNull(latest.version),
            "updateAvailable": latest.status === "update_available",
            "drift": str(drift.state),
            "orphan": a.orphan === true,
            "requiredBy": stringList(a.required_by),
            "folders": stringList(a.folders)
          }
        })
      }
    })
  }
}

// `search`: result rows; `compat` is the one target's verdict.
function reduceSearch(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || !Array.isArray(env.data.results)) {
    return { "query": "", "rows": [], "skipped": [] }
  }
  return {
    "query": str(env.data.query),
    "rows": env.data.results.map(function(r) {
      var compat = r.compat && typeof r.compat === "object" ? r.compat : {}
      var keys = Object.keys(compat)
      var popularity = r.popularity && typeof r.popularity === "object" ? r.popularity : {}
      return {
        "name": str(r.name),
        "ref": strOrNull(r.ref),
        "source": str(r.source),
        "author": strOrNull(r.author),
        "summary": strOrNull(r.summary),
        "downloads": typeof popularity.downloads === "number" ? popularity.downloads : null,
        "stars": typeof popularity.stars === "number" ? popularity.stars : null,
        "updatedAt": strOrNull(r.updated_at),
        "compat": keys.length ? str(compat[keys[0]]) : ""
      }
    }),
    "skipped": Array.isArray(env.data.sources_skipped) ? env.data.sources_skipped.slice() : []
  }
}

// `errors`: per target, per addon. Accounts are never read (M3-D21); the CLI already
// redacts what it prints.
function reduceErrors(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || !Array.isArray(env.data.targets)) return { "targets": [] }
  return {
    "targets": env.data.targets.map(function(t) {
      var target = t.target && typeof t.target === "object" ? t.target : {}
      var dismiss = t.dismiss && typeof t.dismiss === "object" ? t.dismiss : null
      var addons = Array.isArray(t.addons) ? t.addons : []
      return {
        "id": str(target.id),
        "capture": str(t.capture),
        "coverage": str(t.coverage),
        "dismissToken": dismiss && typeof dismiss.precondition === "string" ? dismiss.precondition : null,
        "addons": addons.map(function(a) {
          var errors = Array.isArray(a.errors) ? a.errors : []
          return {
            "addon": str(a.addon),
            "managed": a.managed === true,
            "count": num(a.count),
            "new": num(a["new"]),
            "errors": errors.map(function(e) {
              return {
                "fingerprint": str(e.fingerprint),
                "message": strOrNull(e.message),
                "location": strOrNull(e.location),
                "count": num(e.count),
                "new": e["new"] === true,
                "stack": stringList(e.stack)
              }
            })
          }
        })
      }
    })
  }
}

// `backup list`: per target, newest first as the CLI lists them.
function reduceBackups(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || !Array.isArray(env.data.targets)) return { "targets": [] }
  return {
    "targets": env.data.targets.map(function(t) {
      var target = t.target && typeof t.target === "object" ? t.target : {}
      var backups = Array.isArray(t.backups) ? t.backups : []
      return {
        "id": str(target.id),
        "total": num(t.total),
        "bytes": typeof t.bytes === "number" ? t.bytes : null,
        "backups": backups.map(function(b) {
          return {
            "id": str(b.id),
            "capturedAt": str(b.captured_at),
            "kind": str(b.kind),
            "trigger": strOrNull(b.trigger),
            "files": num(b.files),
            "bytes": num(b.bytes),
            "restorable": b.restorable === true,
            "qualifying": b.qualifying === true
          }
        })
      }
    })
  }
}

// One NDJSON progress event as a line: "download 1/4 · gh:o/bar@v2 on era".
function progressLine(event) {
  if (!event || event.event !== "progress") return ""
  var count = typeof event.current === "number" && typeof event.total === "number" && event.total > 0
    ? " " + event.current + "/" + event.total : ""
  var message = str(event.message)
  return str(event.phase) + count + (message ? " · " + message : "")
}

var RESULT_VERBS = { "update": "Updated", "install": "Installed", "remove": "Removed", "pin": "Pinned", "unpin": "Unpinned" }

function reloadOf(targets) {
  var reload = "none"
  for (var i = 0; i < targets.length; i++) {
    var r = targets[i] && targets[i].reload_required
    if (r === "restart") return "restart"
    if (r === "reload") reload = "reload"
  }
  return reload
}

function isChange(item) {
  return !!item && !item.error && ["unchanged", "skipped", "failed"].indexOf(item.status) === -1
}

// A finished command as one line: "Updated 4 addons on 2 clients", with the reload a
// client needs; a failure's message. Multi-target commands (update, pin, unpin) carry
// `targets[]`; install and remove carry one target's `items`.
function resultLine(env) {
  if (!envelopeOk(env)) return "omaddons did not answer"
  if (!env.ok) return env.error && typeof env.error.message === "string" ? env.error.message : "omaddons failed"
  var data = env.data && typeof env.data === "object" ? env.data : {}
  var targets = Array.isArray(data.targets) ? data.targets : [data]
  var changed = 0
  var touched = 0
  for (var i = 0; i < targets.length; i++) {
    var items = targets[i] && Array.isArray(targets[i].items) ? targets[i].items : []
    var before = changed
    for (var j = 0; j < items.length; j++) if (isChange(items[j])) changed++
    if (changed > before) touched++
  }
  var verb = RESULT_VERBS[env.command] || "Done:"
  var line = changed === 0 && env.command === "update" ? "Everything was up to date"
    : verb + " " + plural(changed, "addon") + (touched > 1 ? " on " + plural(touched, "client") : "")
  var reload = reloadOf(targets)
  if (reload === "reload") line += " · /reload in game"
  if (reload === "restart") line += " · restart the game"
  return line
}

function isSafeHost(host) {
  return ["github.com", "www.wowinterface.com", "www.tukui.org", "www.wowace.com"].indexOf(host) !== -1
}

// Whether a link from CLI output may open in a browser: https on a known host, no
// credentials, no port.
function isSafeUrl(url) {
  if (typeof url !== "string") return false
  var match = /^https:\/\/([^\/?#]*)([\/?#].*)?$/.exec(url)
  if (!match) return false
  var host = match[1].toLowerCase()
  if (host.indexOf("@") !== -1 || host.indexOf(":") !== -1) return false
  return isSafeHost(host)
}

// Buttons from a failure's `fix_argv` and alternatives, where the classifier accepts
// them; the rest stay text.
function fixButtons(env) {
  var out = []
  var error = env && env.error && typeof env.error === "object" ? env.error : null
  if (!error) return out
  if (Array.isArray(error.fix_argv)) {
    var fix = cliAction(error.fix, error.fix_argv, false)
    if (fix.runnable) out.push(fix)
  }
  if (Array.isArray(error.alternatives)) {
    for (var i = 0; i < error.alternatives.length; i++) {
      var alt = error.alternatives[i]
      if (!alt || !Array.isArray(alt.argv)) continue
      var action = cliAction(alt.command || alt.argv.join(" "), alt.argv, false)
      if (action.runnable) out.push(action)
    }
  }
  return out
}

function warningMessages(env, code) {
  var out = []
  if (env && Array.isArray(env.warnings)) {
    for (var i = 0; i < env.warnings.length; i++) {
      if (env.warnings[i] && env.warnings[i].code === code) out.push(str(env.warnings[i].message))
    }
  }
  return out
}

// The confirmation dialog's text from a dry run (M3-D9): what would change, what
// stays required, and a running client.
function confirmText(name, env) {
  if (!envelopeOk(env)) return "omaddons did not answer"
  if (!env.ok) {
    var error = env.error || {}
    return (error.code === "game_running" ? "The game is running: " : "") + str(error.message)
  }
  var data = env.data && typeof env.data === "object" ? env.data : {}
  var lines = []
  if (env.command === "remove") {
    var items = Array.isArray(data.items) ? data.items : []
    var names = []
    var folders = 0
    for (var i = 0; i < items.length; i++) {
      if (items[i].error) continue
      names.push(str(items[i].name || items[i].request) + (items[i].version ? " " + items[i].version : ""))
      folders += Array.isArray(items[i].folders) ? items[i].folders.length : 0
    }
    var target = data.target && data.target.id ? data.target.id : ""
    lines.push("Remove " + joinNames(names) + (target ? " from " + target : "") + " (" + plural(folders, "folder") + ")")
  } else if (env.command === "undo") {
    var reverted = data.reverted && typeof data.reverted === "object" ? data.reverted : {}
    var txs = Array.isArray(data.transactions) ? data.transactions : []
    var where = []
    for (var t = 0; t < txs.length; t++) if (txs[t].target && txs[t].target.id) where.push(txs[t].target.id)
    var folders2 = data.folders || {}
    var restored = stringList(folders2.restored).length
    var removed = stringList(folders2.removed).length
    lines.push((reverted.kind === "undo" ? "Redo " : "Undo ") + (KIND_WORDS[reverted.kind] || str(reverted.kind))
      + (where.length ? " on " + joinNames(where) : "")
      + ": " + plural(restored, "folder") + " restored, " + removed + " removed")
    if (typeof data.changed_since === "number" && data.changed_since > 0) {
      lines.push(plural(data.changed_since, "later change") + " touched these folders since")
    }
  } else {
    lines.push(resultLine(env))
  }
  var still = warningMessages(env, "still_required")
  for (var w = 0; w < still.length; w++) lines.push(still[w])
  if (data.reload_required === "reload") lines.push("Then /reload in game")
  if (data.reload_required === "restart") lines.push("Then restart the game")
  return lines.join("\n")
}

// A `plan_stale` refusal: what changed since the preview (M3-D46).
function staleText(env) {
  var error = env && env.error && typeof env.error === "object" ? env.error : {}
  var changed = error.details ? stringList(error.details.changed) : []
  return "Changed since you looked: " + (changed.length ? joinNames(changed) : "the plan") + ". Nothing ran; check the new preview."
}

if (typeof module !== "undefined") {
  module.exports = {
    DEFAULT_SETTINGS: DEFAULT_SETTINGS,
    SETTING_RULES: SETTING_RULES,
    setting: setting,
    intSetting: intSetting,
    boolSetting: boolSetting,
    WIDGET_LOCAL_DEADLINE_SEC: WIDGET_LOCAL_DEADLINE_SEC,
    WIDGET_NETWORK_DEADLINE_SEC: WIDGET_NETWORK_DEADLINE_SEC,
    WIDGET_KILL_GRACE_SEC: WIDGET_KILL_GRACE_SEC,
    WIDGET_OUTPUT_MAX_BYTES: WIDGET_OUTPUT_MAX_BYTES,
    WIDGET_STREAM_GRACE_SEC: WIDGET_STREAM_GRACE_SEC,
    COMMANDS: COMMANDS,
    CLI_PATHS: CLI_PATHS,
    WIDGET_FLAGS: WIDGET_FLAGS,
    CONTEXT_KEYS: CONTEXT_KEYS,
    pathOf: pathOf,
    templateShape: templateShape,
    buildArgv: buildArgv,
    classifyCliArgv: classifyCliArgv,
    cliArgv: cliArgv,
    envelopeOk: envelopeOk,
    parseEnvelope: parseEnvelope,
    compatible: compatible,
    incompatibleText: incompatibleText,
    NETWORK_QUEUE_MAX: NETWORK_QUEUE_MAX,
    emptyQueues: emptyQueues,
    laneOf: laneOf,
    coalesceKey: coalesceKey,
    enqueue: enqueue,
    nextRequest: nextRequest,
    acceptResult: acceptResult,
    markApplied: markApplied,
    outcomeOf: outcomeOf,
    reduceStatus: reduceStatus,
    staleBar: staleBar,
    badgeText: badgeText,
    reduceList: reduceList,
    reduceSearch: reduceSearch,
    reduceErrors: reduceErrors,
    reduceBackups: reduceBackups,
    ageText: ageText,
    progressLine: progressLine,
    resultLine: resultLine,
    isSafeUrl: isSafeUrl,
    fixButtons: fixButtons,
    confirmText: confirmText,
    staleText: staleText
  }
}
