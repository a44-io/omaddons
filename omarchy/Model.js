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
  "install":        { "argv": ["install", "-t", "{target}", "--", "{ref}"],         "mutates": true },
  "errors":         { "argv": ["errors", "--limit", "10"],                          "mutates": false, "lane": "local" },
  "errors_raw":     { "argv": ["errors", "--raw-details", "-t", "{target}", "--addon={addon}"], "mutates": false, "lane": "local" },
  "dismiss_errors": { "argv": ["errors", "dismiss", "-t", "{target}"],              "mutates": true, "bind": true },
  "search_all":     { "argv": ["search", "--limit", "20", "--", "{query}"],         "mutates": false, "lane": "network" },
  "backups":        { "argv": ["backup", "list", "--limit", "50"],                  "mutates": false, "lane": "local" },
  "backups_detail": { "argv": ["backup", "list", "-v", "-t", "{target}", "--limit", "50"], "mutates": false, "lane": "local" },
  "backup_target":  { "argv": ["backup", "create", "-t", "{target}"],               "mutates": true },
  "backup_all":     { "argv": ["backup", "create"],                                 "mutates": true },
  "restore":        { "argv": ["backup", "restore", "-t", "{target}", "--", "{backup}"], "mutates": true, "confirm": true },
  "restore_addon":  { "argv": ["backup", "restore", "-t", "{target}", "--addon={addon}", "--", "{backup}"], "mutates": true, "confirm": true },
  "prune_backup":   { "argv": ["backup", "prune", "-t", "{target}", "--", "{backup}"],   "mutates": true, "confirm": true },
  "errors_target":  { "argv": ["errors", "--limit", "10", "-t", "{target}"],        "mutates": false, "lane": "local" },
  "backup_auto":    { "argv": ["backup", "create", "--auto", "-t", "{target}"],     "mutates": true, "silent": true }
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
  "backup":          { "mutates": false, "destructive": false, "binds": false },
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
  "auth github":     { "mutates": true,  "destructive": false, "binds": false },
  "omarchy":         { "mutates": false, "destructive": false, "binds": false },
  "omarchy status":  { "mutates": false, "destructive": false, "binds": false },
  "omarchy install": { "mutates": true,  "destructive": false, "binds": false },
  "omarchy remove":  { "mutates": true,  "destructive": false, "binds": false },
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
  // The confirmed run of a preview: a CLI argv that came from a dry run's own fix (its
  // alternatives repeat `--dry-run`) runs for real once the user said Yes.
  if (o.preview !== true && (o.confirmed === true || !!o.token)) {
    var end = rest.indexOf("--")
    rest = rest.filter(function(arg, i) { return arg !== "--dry-run" || (end !== -1 && i > end) })
  }
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
    // `null` when the process table could not be read (`running_unknown`).
    "runningUnknown": target.running === null,
    // From `status -v`: where `o` opens Interface/AddOns.
    "clientDir": str(target.client_dir),
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
      } : null,
      // For the Backups view's header of a client `backup list` names no backup of.
      "auto": autoOf(backups.auto),
      "freeBytes": typeof backups.free_bytes === "number" ? backups.free_bytes : null
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

// "25 min ago", "3 hours ago", "2 days ago", "just now"; "" without a time.
function sinceText(iso, nowMs) {
  var at = Date.parse(iso)
  if (isNaN(at) || typeof nowMs !== "number") return ""
  var minutes = Math.max(0, Math.floor((nowMs - at) / 60000))
  if (minutes < 1) return "just now"
  if (minutes < 60) return minutes + " min ago"
  var hours = Math.floor(minutes / 60)
  if (hours < 48) return plural(hours, "hour") + " ago"
  return plural(Math.floor(hours / 24), "day") + " ago"
}

// "checked 25 min ago".
function ageText(iso, nowMs) {
  var since = sinceText(iso, nowMs)
  return since ? "checked " + since : ""
}

function tooltipOf(summary, targets, nowMs) {
  if (!summary) return ["omaddons has not answered yet"]
  if (targets.length === 0 && num(summary.targets) === 0) return ["No World of Warcraft clients found"]
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
      "targets": [], "summary": null, "lastChange": null, "widget": null, "commands": [], "warnings": [], "issues": []
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
    "commands": commands,
    "warnings": warningsOf(env),
    // The problems of no client (a pending journal naming none): counted in the summary,
    // so listed too.
    "issues": Array.isArray(data.issues) ? data.issues.map(issueOf) : []
  }
}

// A run's warnings with their fixes: a button where the classifier accepts the argv,
// else the fix as text.
function warningsOf(env) {
  var out = []
  if (!env || !Array.isArray(env.warnings)) return out
  for (var i = 0; i < env.warnings.length; i++) {
    var w = env.warnings[i]
    if (!w || typeof w !== "object") continue
    var fix = Array.isArray(w.fix_argv) ? cliAction(w.fix, w.fix_argv, false) : null
    out.push({
      "code": str(w.code), "message": str(w.message),
      "fix": fix && fix.runnable ? fix : null,
      "fixText": fix && fix.runnable ? "" : str(w.fix)
    })
  }
  return out
}

function hintsOf(compat) {
  var out = {}
  for (var key in compat) if (compat.hasOwnProperty(key)) out[key] = str(compat[key])
  return out
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
        "unmanaged": t.counts && typeof t.counts === "object" ? num(t.counts.unmanaged) : 0,
        "addons": addons.map(function(a) {
          var latest = a.latest && typeof a.latest === "object" ? a.latest : {}
          var drift = a.drift && typeof a.drift === "object" ? a.drift : {}
          return {
            "name": str(a.name),
            "version": strOrNull(a.version),
            "ref": strOrNull(a.ref),
            "source": sourceKind(a.ref),
            "pinned": a.pinned === true,
            "state": str(a.state),
            "latest": strOrNull(latest.version),
            // current, update_available, pinned, ahead, blocked, failed, unchecked, static.
            "status": str(latest.status),
            "updateAvailable": latest.status === "update_available",
            "drift": str(drift.state),
            "dependency": a.reason === "dependency",
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
        "compat": keys.length ? str(compat[keys[0]]) : "",
        // Every client's hint, by id (one envelope answers for all of them under All).
        "hints": hintsOf(compat)
      }
    }),
    "skipped": Array.isArray(env.data.sources_skipped) ? env.data.sources_skipped.slice() : []
  }
}

// `errors`: per target, per addon. Accounts are never read (M3-D21); the CLI already
// redacts what it prints.
var CONFIDENCE_RANK = { "high": 3, "medium": 2, "low": 1 }

// The `commands[]` entry with `action` for client `id` (its argv names it after -t).
function targetCommand(env, action, id) {
  if (!env || !Array.isArray(env.commands)) return null
  for (var i = 0; i < env.commands.length; i++) {
    var c = env.commands[i]
    if (!c || c.action !== action || !Array.isArray(c.argv)) continue
    var at = c.argv.indexOf("-t")
    if (at >= 0 && c.argv[at + 1] === id) return cliAction(c.command, c.argv, c.destructive)
  }
  return null
}

// `errors --json` as the Issues view shows it (M3-D21: strict text only; a custom
// message is its length; raw details only when the read asked for them; received
// reports only as a count).
function reduceErrors(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || !Array.isArray(env.data.targets)) return { "targets": [], "raw": false }
  return {
    "raw": env.data.raw_details === true,
    "targets": env.data.targets.map(function(t) {
      var target = t.target && typeof t.target === "object" ? t.target : {}
      var dismiss = t.dismiss && typeof t.dismiss === "object" ? t.dismiss : null
      var summary = t.summary && typeof t.summary === "object" ? t.summary : {}
      var files = t.files && typeof t.files === "object" ? t.files : {}
      var databases = Array.isArray(t.databases) ? t.databases : []
      var addons = Array.isArray(t.addons) ? t.addons : []
      var clientId = str(target.id)
      return {
        "id": clientId,
        "capture": str(t.capture),
        "coverage": str(t.coverage),
        "dismissToken": dismiss && typeof dismiss.precondition === "string" ? dismiss.precondition : null,
        "new": num(summary["new"]),
        "received": num(summary.received),
        // The game was writing: what came before the cut is kept, and nothing re-reads it.
        "partial": num(files.partial) > 0 || databases.some(function(d) { return d && d.state === "partial" }),
        "install": str(t.capture) === "none" ? targetCommand(env, "install_buggrabber", clientId) : null,
        "addons": addons.map(function(a) {
          var errors = Array.isArray(a.errors) ? a.errors : []
          var latest = a.latest && typeof a.latest === "object" ? a.latest : {}
          var confidence = ""
          var reduced = errors.map(function(e) {
            var attribution = e.attribution && typeof e.attribution === "object" ? e.attribution : {}
            var c = str(attribution.confidence)
            if ((CONFIDENCE_RANK[c] || 0) > (CONFIDENCE_RANK[confidence] || 0)) confidence = c
            var raw = e.raw && typeof e.raw === "object" ? e.raw : null
            return {
              "fingerprint": str(e.fingerprint),
              "messageKind": str(e.message_kind),
              "message": strOrNull(e.message),
              "messageChars": typeof e.message_chars === "number" ? e.message_chars : null,
              "location": strOrNull(e.location),
              "confidence": c,
              "type": str(e.type),
              "count": num(e.count),
              "new": e["new"] === true,
              "stack": stringList(e.stack),
              "raw": raw ? { "message": strOrNull(raw.message), "stack": stringList(raw.stack), "locals": strOrNull(raw.locals) } : null
            }
          })
          return {
            "addon": str(a.addon),
            "managed": a.managed === true,
            "kind": str(a.kind),
            "version": strOrNull(a.version),
            "latest": strOrNull(latest.version),
            "updateAvailable": latest.status === "update_available",
            "count": num(a.count),
            "new": num(a["new"]),
            "confidence": confidence,
            "errors": reduced
          }
        })
      }
    })
  }
}

// An `auto` object (`{mode, capability, last}`), as `backup list` and `status` give it.
function autoOf(auto) {
  if (!auto || typeof auto !== "object") return null
  var last = auto.last && typeof auto.last === "object" ? auto.last : null
  return { "mode": str(auto.mode), "capability": str(auto.capability),
    "last": last ? { "at": str(last.at), "result": str(last.result) } : null }
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
        "shown": num(t.shown),
        "bytes": typeof t.bytes === "number" ? t.bytes : null,
        "freeBytes": typeof t.free_bytes === "number" ? t.free_bytes : null,
        "auto": autoOf(t.auto),
        "backups": backups.map(function(b) {
          var addons = Array.isArray(b.addons) ? b.addons : null
          return {
            "id": str(b.id),
            "capturedAt": str(b.captured_at),
            "kind": str(b.kind),
            "trigger": strOrNull(b.trigger),
            "files": num(b.files),
            "bytes": num(b.bytes),
            "restorable": b.restorable === true,
            "qualifying": b.qualifying === true,
            "consistency": str(b.consistency),
            "integrity": str(b.integrity),
            "baseline": strOrNull(b.baseline),
            "busy": b.busy === true,
            // `backup list -v`: the addon versions that wrote these settings.
            "addons": addons ? addons.map(function(a) {
              return { "name": str(a.name), "version": strOrNull(a.version), "folders": stringList(a.folders) }
            }) : null
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
// The result line of a backup command: what it backed up, restored or deleted; "" for
// any other command.
// The result line of a command whose envelope reports no items: what it did. An undo
// names the change it reverted and where; a dismissal counts the errors that stopped
// being new. "" for any other command.
function commandResultLine(command, data) {
  if (command === "undo") {
    var reverted = data.reverted && typeof data.reverted === "object" ? data.reverted : {}
    var where = []
    // A partial undo: the clients where a transaction failed are said apart, with the
    // first failure, never counted as undone.
    var failedAt = []
    var why = ""
    var txs = Array.isArray(data.transactions) ? data.transactions : []
    for (var i = 0; i < txs.length; i++) {
      var id = txs[i] && txs[i].target ? str(txs[i].target.id) : ""
      if (txs[i] && txs[i].status === "failed") {
        if (id && failedAt.indexOf(id) === -1) failedAt.push(id)
        if (!why && txs[i].error) why = str(txs[i].error.message)
        continue
      }
      if (id && where.indexOf(id) === -1) where.push(id)
    }
    if (txs.length === 0 && reverted.target) where.push(str(reverted.target.id))
    var line = (where.length || failedAt.length === 0)
      ? "Undid " + (str(reverted.kind) || "the last change") + (where.length ? " on " + joinNames(where) : "")
      : "Undid nothing"
    return failedAt.length ? line + "; not on " + joinNames(failedAt) + (why ? ": " + why : "") : line
  }
  if (command === "errors dismiss") {
    var targets = Array.isArray(data.targets) ? data.targets : []
    var count = 0
    var names = []
    for (var t = 0; t < targets.length; t++) {
      var n = targets[t] ? num(targets[t].errors) : 0
      count += n
      if (n > 0 && targets[t].target) names.push(str(targets[t].target.id))
    }
    return count === 0 ? "No new errors to dismiss"
      : "Dismissed " + plural(count, "new error") + " on " + joinNames(names)
  }
  return backupResultLine(command, data)
}

function backupResultLine(command, data) {
  var targets = Array.isArray(data.targets) ? data.targets : [data]
  if (command === "backup restore") {
    var where = data.target && data.target.id ? " on " + data.target.id : ""
    if (data.mode === "addons") {
      var names = (Array.isArray(data.addons) ? data.addons : []).map(function(a) { return str(a.addon) + "'s" })
      return "Restored " + joinNames(names) + " settings" + where
    }
    return "Restored the whole WTF/" + where
  }
  if (command === "backup create") {
    var made = targets.filter(function(t) { return t && t.backup }).map(function(t) { return t.target && t.target.id ? t.target.id : "" })
    // A client that failed is said, with its error: a partial run is not "backed up".
    var broke = targets.filter(function(t) { return t && t.error })
    var failure = broke.length
      ? "; " + joinNames(broke.map(function(t) { return t.target && t.target.id ? t.target.id : "" })) + " failed: " + str(broke[0].error.message)
      : ""
    return (made.length ? "Backed up " + joinNames(made) : broke.length ? "No backup was made" : "No backup was needed") + failure
  }
  if (command === "backup prune") {
    var count = 0
    for (var i = 0; i < targets.length; i++) count += targets[i] && Array.isArray(targets[i].pruned) ? targets[i].pruned.length : 0
    return "Deleted " + plural(count, "backup")
  }
  return ""
}

// The items of a change result that failed, each with its error: `[{name, error}]`.
function failedItems(env) {
  var out = []
  var data = env && env.data && typeof env.data === "object" ? env.data : {}
  var targets = Array.isArray(data.targets) ? data.targets : [data]
  for (var i = 0; i < targets.length; i++) {
    var items = targets[i] && Array.isArray(targets[i].items) ? targets[i].items : []
    for (var j = 0; j < items.length; j++) {
      var item = items[j]
      if (!item || !(item.error || item.status === "failed")) continue
      var error = item.error && typeof item.error === "object" ? item.error : {}
      out.push({ "name": str(item.name) || str(item.request), "error": error })
    }
  }
  return out
}

// The fixes of a partial result's failed items, as buttons when the classifier accepts
// them (one per command).
function itemFixes(env) {
  var out = []
  var seen = {}
  var failed = failedItems(env)
  for (var i = 0; i < failed.length; i++) {
    var error = failed[i].error
    if (!Array.isArray(error.fix_argv)) continue
    var fix = cliAction(error.fix, error.fix_argv, false)
    if (!fix.runnable || seen[fix.command]) continue
    seen[fix.command] = true
    out.push(fix)
  }
  return out
}

function resultLine(env) {
  if (!envelopeOk(env)) return "omaddons did not answer"
  if (!env.ok) return env.error && typeof env.error.message === "string" ? env.error.message : "omaddons failed"
  var data = env.data && typeof env.data === "object" ? env.data : {}
  var targets = Array.isArray(data.targets) ? data.targets : [data]
  var changed = 0
  var touched = 0
  // Whether the result reports items at all: a read run from a fix (`list -v`) has none,
  // and is "Done", never "Done: 0 addons".
  var reportsItems = false
  for (var i = 0; i < targets.length; i++) {
    var items = targets[i] && Array.isArray(targets[i].items) ? targets[i].items : []
    if (targets[i] && Array.isArray(targets[i].items)) reportsItems = true
    var before = changed
    for (var j = 0; j < items.length; j++) if (isChange(items[j])) changed++
    if (changed > before) touched++
  }
  var verb = RESULT_VERBS[env.command] || "Done:"
  var failed = failedItems(env)
  var own = commandResultLine(env.command, data)
  var line = own ? own
    : !reportsItems && failed.length === 0 && !RESULT_VERBS[env.command] ? "Done"
    : changed === 0 && failed.length === 0 && env.command === "update" ? "Everything was up to date"
    : failed.length === 0 ? verb + " " + plural(changed, "addon") + (touched > 1 ? " on " + plural(touched, "client") : "")
    : (changed > 0 ? verb + " " + changed + " of " + (changed + failed.length) + "; " : "")
      + failed[0].name + " failed: " + str(failed[0].error.message)
      + (failed.length > 1 ? " (and " + (failed.length - 1) + " more)" : "")
  var reload = reloadOf(targets)
  if (reload === "reload") line += " · /reload in game"
  if (reload === "restart") line += " · restart the game"
  return line
}

function isSafeHost(host) {
  return ["github.com", "www.wowinterface.com", "tukui.org", "www.tukui.org", "www.wowace.com"].indexOf(host) !== -1
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
      // An alternative is a suggestion: `{display, argv}`, its display shell-quoted.
      var action = cliAction(alt.display || alt.argv.join(" "), alt.argv, false)
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
    // The undo of a whole restore reverts every play since it too: `changed_since`
    // counts the files the game changed (kept in the undo's snapshot), `null` when they
    // cannot be counted.
    if (typeof data.changed_since === "number" && data.changed_since > 0) {
      lines.push("Also reverts every play since the restore: " + plural(data.changed_since, "file")
        + " the game changed (kept in the undo's snapshot)")
    } else if (data.changed_since === null) {
      lines.push("Also reverts every play since the restore (the files it changed cannot be counted)")
    }
  } else if (env.command === "targets rm") {
    var managed = num(data.managed_addons)
    lines.push("Forget client " + str(data.id) + "; nothing in its directory is touched"
      + (managed ? " (its record of " + plural(managed, "managed addon") + " is kept)" : ""))
  } else if (env.command === "backup restore") {
    lines = lines.concat(restoreLines(data, env))
  } else if (env.command === "backup prune") {
    lines = lines.concat(pruneLines(data))
  } else {
    lines.push(resultLine(env))
  }
  var still = warningMessages(env, "still_required")
  for (var w = 0; w < still.length; w++) lines.push(still[w])
  if (data.reload_required === "reload") lines.push("Then /reload in game")
  if (data.reload_required === "restart") lines.push("Then restart the game")
  return lines.join("\n")
}

// "2026-10-22 22:46" (UTC, as the CLI writes it).
// An instant as local "YYYY-MM-DD HH:MM", the zone the bar's clock shows; a text that
// does not parse, as it is.
function dateText(iso) {
  var s = str(iso)
  var at = new Date(s)
  if (s === "" || isNaN(at.getTime())) return s
  var two = function(n) { return n < 10 ? "0" + n : String(n) }
  return at.getFullYear() + "-" + two(at.getMonth() + 1) + "-" + two(at.getDate())
    + " " + two(at.getHours()) + ":" + two(at.getMinutes())
}

// A restore's dry run (D7, D8): what it would put back, the versions that wrote the
// settings and what is not in the backup; whole adds what else it changes, and the
// current WTF/ kept as a backup (or why not).
function restoreLines(data, env) {
  var out = []
  var backup = data.backup && typeof data.backup === "object" ? data.backup : {}
  var from = "the backup of " + dateText(backup.captured_at)
  if (data.mode === "addons") {
    var addons = Array.isArray(data.addons) ? data.addons : []
    for (var i = 0; i < addons.length; i++) {
      var a = addons[i]
      var f = a.files && typeof a.files === "object" ? a.files : {}
      out.push("Restore " + str(a.addon) + "'s settings from " + from + ": " + num(f.replaced) + " replaced, "
        + num(f.restored) + " restored (absent now), " + num(f.created_since) + " created since (moved aside)")
      var captured = str(a.captured_version)
      var installed = str(a.installed_version)
      if (captured && installed && captured !== installed) {
        out.push("Settings written by " + str(a.addon) + " " + captured + "; installed now: " + installed)
      }
      var absent = stringList(a.not_in_backup)
      for (var j = 0; j < absent.length; j++) out.push(absent[j] + ": not in this backup; left as it is")
    }
    return out
  }
  var files = data.files && typeof data.files === "object" ? data.files : {}
  out.push("Restore the whole WTF/ from " + from + ": " + plural(num(files.restored), "file") + " restored, "
    + num(files.differed) + " differ now, " + num(files.created_since) + " created since (moved aside)")
  out.push("Also restores keybindings, macros, chat settings and every other addon's settings")
  var skipped = warningMessages(env, "before_restore_skipped")
  if (skipped.length) return out.concat(skipped)
  out.push("The current WTF/ is kept as a backup (before restore)")
  return out
}

// A prune's dry run: the backups it would delete, and those in use it keeps.
function pruneLines(data) {
  var out = []
  var targets = Array.isArray(data.targets) ? data.targets : []
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    var where = t.target && t.target.id ? " from " + t.target.id : ""
    var pruned = Array.isArray(t.pruned) ? t.pruned : []
    for (var j = 0; j < pruned.length; j++) {
      out.push("Delete backup " + str(pruned[j].id) + " (" + kindText({ "kind": str(pruned[j].kind), "trigger": null }) + ", "
        + bytesText(num(pruned[j].bytes)) + ")" + where)
    }
    var busy = Array.isArray(t.busy) ? t.busy : []
    for (var k = 0; k < busy.length; k++) out.push(str(busy[k].id || busy[k]) + " is in use and is kept")
  }
  return out
}

// A refused restore's alternatives (`ref_ambiguous`: ownership changed since the
// backup): the `--addon` value each names, for the restore_addon template. The CLI's
// argv repeat the widget's own flags, so they are never run as they are.
function ambiguityChoices(env) {
  var error = env && env.error && typeof env.error === "object" ? env.error : null
  if (!error || error.code !== "ref_ambiguous" || !Array.isArray(error.alternatives)) return []
  var out = []
  for (var i = 0; i < error.alternatives.length; i++) {
    var argv = error.alternatives[i] && Array.isArray(error.alternatives[i].argv) ? error.alternatives[i].argv : []
    var value = ""
    for (var j = 0; j < argv.length && argv[j] !== "--"; j++) {
      if (argv[j] === "--addon" && j + 1 < argv.length) value = str(argv[j + 1])
      else if (str(argv[j]).indexOf("--addon=") === 0) value = argv[j].slice(8)
    }
    if (value) out.push({ "addon": value, "label": value.indexOf("folder:") === 0 ? "the " + value.slice(7) + " folder" : value })
  }
  return out
}

// A refused restore's backup folder (`filesystem_unsupported`): the plain copy to
// restore by hand with the game closed.
function backupFolderOf(env) {
  var error = env && env.error && typeof env.error === "object" ? env.error : null
  return error && error.details && typeof error.details.backup_dir === "string" ? error.details.backup_dir : ""
}

// A `plan_stale` refusal: what changed since the preview (M3-D46).
function staleText(env) {
  var error = env && env.error && typeof env.error === "object" ? env.error : {}
  var changed = error.details ? stringList(error.details.changed) : []
  return "Changed since you looked: " + (changed.length ? joinNames(changed) : "the plan") + ". Nothing ran; check the new preview."
}

// A failure's fix as text, when it is no button: prose, or an argv the classifier
// refuses (M3-D2); "" when fixButtons shows it.
function fixText(env) {
  var error = env && env.error && typeof env.error === "object" ? env.error : null
  if (!error) return ""
  if (Array.isArray(error.fix_argv) && cliAction(error.fix, error.fix_argv, false).runnable) return ""
  return str(error.fix)
}

// -------------------------------------------------------------- the panel frame (F4)

// The panel's views, in switcher order: h/l step through them, 1–5 jump.
var VIEWS = ["updates", "installed", "issues", "backups", "search"]
var VIEW_TITLES = { "updates": "Updates", "installed": "Installed", "issues": "Issues", "backups": "Backups", "search": "Search" }

// The letter keys of the panel key map (PanelKeyCatcher keeps j/k/h/l and the arrows,
// Enter, Space, x, Esc and Tab). The frame runs update, check, undo, open, filter and
// search itself; the rest go to the current view, or to the task that registered them.
var PANEL_KEYS = {
  "u": "update", "c": "check", "z": "undo", "o": "open", "t": "filter", "/": "search",
  "b": "backup", "d": "dismiss", "r": "restore", "s": "raw", "i": "notes", "p": "pin"
}

// What the letter `text` does: `{name}`, `{name: "view", view}` for 1–5, or null.
function panelKey(text) {
  if (typeof text !== "string" || text.length !== 1) return null
  var digit = "12345".indexOf(text)
  if (digit >= 0) return { "name": "view", "view": VIEWS[digit] }
  return PANEL_KEYS.hasOwnProperty(text) ? { "name": PANEL_KEYS[text] } : null
}

// The view `step` places from `view`, wrapping around.
function stepView(view, step) {
  var at = VIEWS.indexOf(view)
  if (at < 0) at = 0
  var n = VIEWS.length
  return VIEWS[(((at + step) % n) + n) % n]
}

// The hero's meta line: "6 clients · 3 updates · 1 issue".
function heroMeta(summary) {
  if (!summary) return ""
  var parts = [plural(summary.targets, "client"), plural(summary.updates.available, "update")]
  if (summary.issuesTotal > 0) parts.push(plural(summary.issuesTotal, "issue"))
  if (summary.errors && summary.errors["new"] > 0) parts.push(plural(summary.errors["new"], "new error"))
  return parts.join(" · ")
}

// The hero's detail: "checked 25 min ago · retail running" (the oldest check, as the
// tooltip says it), or "running: unknown" when the process table could not be read.
function heroDetail(summary, targets, nowMs) {
  if (!summary) return ""
  var parts = []
  var oldest = null
  var unknown = false
  for (var i = 0; i < targets.length; i++) {
    var checked = targets[i].updates ? targets[i].updates.checkedAt : null
    if (checked && (!oldest || Date.parse(checked) < Date.parse(oldest))) oldest = checked
    if (targets[i].runningUnknown) unknown = true
  }
  var age = oldest ? ageText(oldest, nowMs) : ""
  if (age) parts.push(age)
  if (summary.running.length > 0) parts.push(joinNames(summary.running) + " running")
  else if (unknown) parts.push("running: unknown")
  return parts.join(" · ")
}

function targetById(targets, id) {
  if (!id || !Array.isArray(targets)) return null
  for (var i = 0; i < targets.length; i++) if (targets[i].id === id) return targets[i]
  return null
}

// `t`: All (""), then each target in order, then All again. A filter whose target is
// gone starts over at All.
function nextFilter(targets, current) {
  var ids = Array.isArray(targets) ? targets.map(function(t) { return t.id }) : []
  if (ids.length === 0) return ""
  if (!current) return ids[0]
  var at = ids.indexOf(current)
  return at < 0 || at === ids.length - 1 ? "" : ids[at + 1]
}

// `o`: the selected target's AddOns folder for the desktop's opener (M3-D2: a literal
// tool, an argv); null without a selected target or an absolute client directory.
function addonsDirArgv(target) {
  if (!target || !target.clientDir || target.clientDir.charAt(0) !== "/") return null
  return ["xdg-open", target.clientDir.replace(/\/+$/, "") + "/Interface/AddOns"]
}

// `u`: every client's updates, or the selected one's (no confirmation: undoable and
// expected, M3-D9).
function updateRequest(filter) {
  if (filter) return { "template": "update_target", "slots": { "target": filter }, "label": "Update " + filter }
  return { "template": "update_all", "slots": {}, "label": "Update all" }
}

// The Undo-last control: its label ("Undo update on retail", "Redo …"), the CLI argv
// it runs through the confirm flow, or why there is none.
function undoAction(lastChange) {
  if (!lastChange) return { "label": "Undo last", "argv": null, "why": "nothing to undo" }
  if (lastChange.unknown) return { "label": "Undo last", "argv": null, "why": "last change: unknown" }
  var undo = lastChange.undo
  if (!undo) return { "label": lastChange.label, "argv": null, "why": "nothing to undo" }
  if (!undo.runnable) return { "label": lastChange.label, "argv": null, "why": "not a command the widget runs: " + undo.command }
  return { "label": lastChange.label, "argv": undo.argv, "why": "" }
}

// A running action's progress as a fraction, or -1 when it has no count yet.
function progressFraction(action) {
  if (!action || typeof action.total !== "number" || action.total <= 0) return -1
  var current = typeof action.current === "number" ? action.current : 0
  return Math.max(0, Math.min(1, current / action.total))
}

// -------------------------------------------------------------- the Updates and Installed views (F5)

// The source a ref names (README's ref grammar): gh, wowi, tukui, git, wowace, or file
// for a URL or a local path.
var SOURCE_KINDS = ["gh", "wowi", "tukui", "git", "wowace"]

function sourceKind(ref) {
  var r = str(ref)
  var colon = r.indexOf(":")
  var prefix = colon > 0 ? r.slice(0, colon) : ""
  return SOURCE_KINDS.indexOf(prefix) >= 0 ? prefix : "file"
}

// The Updates view's rows, in the filter: per client, updates first, then pinned
// addons with a newer release, blocked ones, and those whose check failed. In the All
// filter the first row of each client carries `group`, its id. `reload` marks a running
// client's rows ("then /reload").
var UPDATE_KINDS = { "update_available": "update", "pinned": "pinned", "blocked": "blocked", "failed": "failed" }
var UPDATE_ORDER = ["update", "pinned", "blocked", "failed"]
var UPDATE_LABELS = { "update": "", "pinned": "pinned", "blocked": "blocked", "failed": "check failed" }

function listTargets(lists, filter) {
  var targets = lists && Array.isArray(lists.targets) ? lists.targets : []
  return targets.filter(function(t) { return !filter || t.id === filter })
}

function updateRows(lists, filter, statusTargets) {
  var out = []
  var targets = listTargets(lists, filter)
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    var status = targetById(statusTargets, t.id)
    var rows = []
    for (var j = 0; j < t.addons.length; j++) {
      var a = t.addons[j]
      var kind = UPDATE_KINDS[a.status]
      if (!kind || (kind === "pinned" && (!a.latest || a.latest === a.version))) continue
      rows.push({
        "key": t.id + "/" + a.name, "target": t.id, "name": a.name, "version": a.version, "latest": a.latest,
        "kind": kind, "label": UPDATE_LABELS[kind], "pinned": a.pinned, "reload": !!status && status.running === true,
        "group": ""
      })
    }
    rows.sort(function(x, y) { return UPDATE_ORDER.indexOf(x.kind) - UPDATE_ORDER.indexOf(y.kind) })
    if (!filter && rows.length > 0) rows[0].group = t.id
    out = out.concat(rows)
  }
  return out
}

// What `u` would update in the filter: the "Update all (N)" header's N.
function updateCount(rows) {
  return rows.filter(function(r) { return r.kind === "update" }).length
}

// The Updates view with nothing to update: "Everything is current · checked 25 min
// ago", the oldest check of the clients in the filter.
function updatesEmptyText(statusTargets, filter, nowMs) {
  var oldest = null
  var targets = Array.isArray(statusTargets) ? statusTargets : []
  for (var i = 0; i < targets.length; i++) {
    if (filter && targets[i].id !== filter) continue
    var checked = targets[i].updates ? targets[i].updates.checkedAt : null
    if (checked && (!oldest || Date.parse(checked) < Date.parse(oldest))) oldest = checked
  }
  var age = oldest ? ageText(oldest, nowMs) : ""
  return "Everything is current" + (age ? " · " + age : "")
}

// The Installed view's rows, in the filter: every managed addon, its source and its
// markers (dep, orphan, pinned, modified or missing), and what Enter does: reinstall
// drifted files, else update when one is available, else nothing ("").
function installedRows(lists, filter) {
  var out = []
  var targets = listTargets(lists, filter)
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    for (var j = 0; j < t.addons.length; j++) {
      var a = t.addons[j]
      var markers = []
      if (a.dependency) markers.push("dep")
      if (a.orphan) markers.push("orphan")
      if (a.pinned) markers.push("pinned")
      if (a.drift === "modified" || a.drift === "missing") markers.push(a.drift)
      out.push({
        "key": t.id + "/" + a.name, "target": t.id, "name": a.name, "version": a.version, "source": a.source,
        "markers": markers, "pinned": a.pinned,
        "primary": a.drift === "modified" || a.drift === "missing" ? "reinstall" : (a.updateAvailable ? "update" : ""),
        "group": !filter && j === 0 ? t.id : ""
      })
    }
  }
  return out
}

// The unmanaged folders of the clients in the filter, as text (adoption is the CLI's
// in M3): "1 unmanaged folder on retail: omaddons adopt -t retail".
function unmanagedLines(lists, filter) {
  return listTargets(lists, filter)
    .filter(function(t) { return t.unmanaged > 0 })
    .map(function(t) { return plural(t.unmanaged, "unmanaged folder") + " on " + t.id + ": omaddons adopt -t " + t.id })
}

// A row's action, as a template run: Enter's, and `p`'s pin or unpin.
function rowAction(row, which) {
  var slots = { "target": row.target, "addon": row.name }
  if (which === "pin") return row.pinned ? { "template": "unpin", "slots": slots, "label": "Unpin " + row.name }
    : { "template": "pin", "slots": slots, "label": "Pin " + row.name }
  if (which === "update") return { "template": "update_addon", "slots": slots, "label": "Update " + row.name }
  if (which === "reinstall") return { "template": "reinstall", "slots": slots, "label": "Reinstall " + row.name }
  if (which === "remove") return { "template": "remove", "slots": slots, "label": "Remove " + row.name }
  return null
}

var NOTES_MAX_CHARS = 400
var MORE_HOSTS = { "github.com": "GitHub", "www.wowinterface.com": "WoWInterface", "tukui.org": "Tukui", "www.tukui.org": "Tukui", "www.wowace.com": "WowAce" }

// `i`'s release notes for `row`, from the service's view-bound changelog (`{selection,
// data}`): null until the answer for this row arrives (a late one for another row never
// matches). The newest entry, cut at 400 characters, and a link only when isSafeUrl
// accepts it.
function notesFor(changelog, row) {
  if (!changelog || !changelog.selection || !row) return null
  if (changelog.selection.target !== row.target || changelog.selection.addon !== row.name) return null
  // A read that failed: said in the row, which would otherwise wait for it forever.
  if (changelog.failed) return { "version": "", "text": "The release notes could not be read: " + str(changelog.failed), "url": "", "link": "" }
  var log = changelog.data && changelog.data.changelog && typeof changelog.data.changelog === "object" ? changelog.data.changelog : {}
  var entries = Array.isArray(log.entries) ? log.entries : []
  var first = entries.length ? entries[0] : null
  var text = first ? str(first.text) : ""
  var more = str(log.more_url)
  var safe = more && isSafeUrl(more)
  return {
    "version": first ? str(first.version) : "",
    "text": text.length > NOTES_MAX_CHARS ? text.slice(0, NOTES_MAX_CHARS) + "…" : (text || "No release notes"),
    "url": safe ? more : "",
    "link": safe ? "more on " + (MORE_HOSTS[more.split("/")[2].toLowerCase()] || "the web") : ""
  }
}

// -------------------------------------------------------------- the Issues and Search views (F6)

var SEVERITY_ORDER = ["P0", "P1", "P2", "P3"]

// One error's line: where, and for Lua's and Blizzard's own message templates (their
// variable parts are identifiers from code) the message; a custom message an addon
// wrote shows only its length (M3-D21).
function errorLine(e) {
  var where = e.location || "unknown location"
  if (e.messageKind === "custom") {
    return where + ": custom message (" + (e.messageChars === null ? "?" : e.messageChars) + " characters, hidden)"
  }
  return where + (e.message ? ": " + e.message : "")
}

// The Issues view, in the filter, as sections (a title, lines of text, rows) whose rows
// the cursor walks in order (`index`): problems from `status` by severity; each client's
// Lua errors (an Install row where BugGrabber is missing, then its addons; received
// reports and an incomplete read as lines); the run warnings. A row's `action` is the
// CLI argv Enter runs (the classifier accepted it), its `text` the command it shows
// instead.
function issueSections(statusTargets, errors, warnings, filter, machineIssues) {
  var sections = []
  var problems = []
  // The problems of no client, unless the view is narrowed to one.
  var machine = !filter && Array.isArray(machineIssues) ? machineIssues : []
  for (var n = 0; n < machine.length; n++) {
    var mi = machine[n]
    var mr = mi.remediation || {}
    problems.push({
      "kind": "problem", "key": "p//" + n, "target": "", "severity": mi.severity,
      "urgent": mi.urgent, "title": mi.title,
      "action": mr.runnable ? mr : null, "text": mr.runnable ? "" : str(mr.command)
    })
  }
  var targets = Array.isArray(statusTargets) ? statusTargets : []
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    if (filter && t.id !== filter) continue
    for (var j = 0; j < t.issues.length; j++) {
      var issue = t.issues[j]
      var r = issue.remediation || {}
      problems.push({
        "kind": "problem", "key": "p/" + t.id + "/" + j, "target": t.id, "severity": issue.severity,
        "urgent": issue.urgent, "title": issue.title,
        "action": r.runnable ? r : null, "text": r.runnable ? "" : str(r.command)
      })
    }
  }
  problems.sort(function(a, b) { return SEVERITY_ORDER.indexOf(a.severity) - SEVERITY_ORDER.indexOf(b.severity) })
  if (problems.length) sections.push({ "title": "Problems", "lines": [], "rows": problems })

  var errTargets = errors && Array.isArray(errors.targets) ? errors.targets : []
  for (var k = 0; k < errTargets.length; k++) {
    var e = errTargets[k]
    if (filter && e.id !== filter) continue
    var rows = []
    var lines = []
    if (e.capture === "none") {
      rows.push({
        "kind": "capture", "key": "c/" + e.id, "target": e.id,
        "title": e.install ? "BugGrabber is not installed" : "No BugGrabber build for this client is known",
        "action": e.install && e.install.runnable ? e.install : null,
        "text": e.install && !e.install.runnable ? e.install.command : ""
      })
    }
    for (var m = 0; m < e.addons.length; m++) {
      var a = e.addons[m]
      rows.push({
        "kind": "addon", "key": "a/" + e.id + "/" + a.addon, "target": e.id, "addon": a.addon, "count": a.count,
        "new": a["new"], "confidence": a.confidence, "updateAvailable": a.updateAvailable, "version": a.version,
        "latest": a.latest, "lines": a.errors.map(errorLine),
        "stack": a.errors.length ? a.errors[0].stack : []
      })
    }
    if (e.received > 0) {
      lines.push(e.received === 1 ? "1 error received from another player (not counted)"
        : e.received + " errors received from other players (not counted)")
    }
    if (e.partial) lines.push("Incomplete read (the game may be writing)")
    if (e.capture !== "none" && e.addons.length === 0 && !e.partial) lines.push("No Lua errors")
    sections.push({ "title": "Lua errors (" + e.id + ")", "lines": lines, "rows": rows })
  }

  var runWarnings = (Array.isArray(warnings) ? warnings : []).map(function(w, n) {
    return { "kind": "warning", "key": "w/" + n, "title": w.message, "action": w.fix, "text": w.fixText }
  })
  if (runWarnings.length) sections.push({ "title": "Run warnings", "lines": [], "rows": runWarnings })

  var index = 0
  for (var s = 0; s < sections.length; s++) {
    for (var n = 0; n < sections[s].rows.length; n++) sections[s].rows[n].index = index++
  }
  return sections
}

// The rows of issueSections, in cursor order.
function issueRows(sections) {
  var out = []
  for (var i = 0; i < sections.length; i++) out = out.concat(sections[i].rows)
  return out
}

// `d`: what to dismiss, bound to the shown observation: the filtered client, or each
// client with new errors, with the token of the `errors` read the panel shows (one call
// per client: `--if-match` binds exactly its token's client).
function dismissPlan(errors, filter) {
  var clients = errors && Array.isArray(errors.targets) ? errors.targets : []
  return clients
    .filter(function(t) { return (!filter || t.id === filter) && t["new"] > 0 && !!t.dismissToken })
    .map(function(t) { return { "target": t.id, "token": t.dismissToken } })
}

// Search: the template for the filter. All searches every client in one request
// (`search_all`, no -t), so each result carries every client's hint.
function searchRequest(query, filter) {
  if (filter) return { "template": "search", "slots": { "target": filter, "query": query } }
  return { "template": "search_all", "slots": { "query": query } }
}

var INSTALLABLE_HINTS = ["yes", "likely"]

// Where a search result may be installed: every client with its hint; only `yes` and
// `likely` are eligible (the CLI's own rule for its `commands`).
function installTargets(result, statusTargets) {
  var hints = result && result.hints ? result.hints : {}
  return (Array.isArray(statusTargets) ? statusTargets : []).map(function(t) {
    var hint = hints.hasOwnProperty(t.id) ? hints[t.id] : "unknown"
    return { "id": t.id, "hint": hint, "eligible": INSTALLABLE_HINTS.indexOf(hint) >= 0 }
  })
}

// `s`: the raw details of `row`'s errors, from the service's view-bound read (`{selection,
// data}`): null until the answer for this row arrives. Each error's raw message, then
// its raw stack.
function rawLines(rawErrors, row) {
  if (!rawErrors || !rawErrors.selection || !row) return null
  if (rawErrors.selection.target !== row.target || rawErrors.selection.addon !== row.addon) return null
  var targets = rawErrors.data && Array.isArray(rawErrors.data.targets) ? rawErrors.data.targets : []
  var lines = []
  for (var i = 0; i < targets.length; i++) {
    if (targets[i].id !== row.target) continue
    for (var j = 0; j < targets[i].addons.length; j++) {
      var a = targets[i].addons[j]
      if (a.addon !== row.addon) continue
      for (var k = 0; k < a.errors.length; k++) {
        var raw = a.errors[k].raw
        if (!raw) continue
        if (raw.message) lines.push(raw.message)
        for (var n = 0; n < raw.stack.length; n++) lines.push("  " + raw.stack[n])
      }
    }
  }
  return lines
}

// How sure the attribution is, as a dot scale.
function confidenceDots(confidence) {
  var full = CONFIDENCE_RANK[confidence] || 0
  var dots = ""
  for (var i = 0; i < 3; i++) dots += String.fromCodePoint(i < full ? 0x25CF : 0x25CB)
  return dots
}

// A search result's popularity: "2.1M downloads", "412 stars", or "".
function popularityText(row) {
  var compact = function(n) {
    if (n >= 1000000) return (Math.round(n / 100000) / 10) + "M"
    if (n >= 1000) return (Math.round(n / 100) / 10) + "k"
    return String(n)
  }
  if (row && typeof row.downloads === "number") return compact(row.downloads) + " downloads"
  if (row && typeof row.stars === "number") return compact(row.stars) + " stars"
  return ""
}

// -------------------------------------------------------------- the Backups view and the restore chooser (F9)

// "1.2 KiB", "15.2 MiB", "1.0 GiB".
function bytesText(n) {
  var value = num(n)
  if (value < 1024) return value + " B"
  var units = ["KiB", "MiB", "GiB", "TiB"]
  var at = -1
  while (value >= 1024 && at < units.length - 1) {
    value /= 1024
    at++
  }
  return (Math.round(value * 10) / 10).toFixed(1) + " " + units[at]
}

var BACKUP_KINDS = { "manual": "manual", "auto": "automatic", "before_restore": "before restore", "imported": "imported", "recovered": "recovered" }

// "automatic (post session)".
function kindText(b) {
  var kind = BACKUP_KINDS[b.kind] || b.kind
  return b.trigger ? kind + " (" + String(b.trigger).replace(/_/g, " ") + ")" : kind
}

// What a backup proves (M3-D47): "verified" only for `verified`, with its baseline;
// damaged says so; metadata only says nothing.
function integrityText(b) {
  if (b.integrity === "verified") return "verified" + (b.baseline ? " (" + b.baseline + ")" : "")
  if (b.integrity === "damaged") return "damaged"
  return ""
}

var AUTO_RESULTS = {
  "created": "made one", "unchanged": "nothing changed", "done_today": "done today", "in_progress": "in progress",
  "running": "skipped while the game ran", "failed": "failed", "no_wtf": "no WTF/ yet", "space_low": "skipped: low disk space",
  "no_reflink": "this filesystem cannot clone"
}

// A client's automatic backups (M3-D30): its mode, what the filesystem allows, and the
// last run.
function autoLabel(auto, nowMs) {
  if (!auto) return ""
  if (auto.mode === "off") return "Automatic backups: off"
  var last = auto.last
  if (last && last.result === "space_low") return "Automatic backups: skipped: low disk space"
  // Only `daily` needs clones: `daily-copy` copies on a filesystem that cannot clone.
  if (auto.mode === "daily" && auto.capability === "unsupported") return "Automatic backups: daily, but this filesystem cannot clone"
  var mode = auto.mode === "daily-copy" ? "daily (copies)" : "daily"
  var lastText = last ? (AUTO_RESULTS[last.result] || last.result) + (last.at ? " " + sinceText(last.at, nowMs) : "") : "not run yet"
  var copying = auto.mode === "daily-copy" && last !== null && last !== undefined
    && ["created", "unchanged", "done_today", "running", "in_progress"].indexOf(last.result) !== -1
  var working = auto.capability === "supported" || copying
  return "Automatic backups: " + mode + (working ? ", working" : "") + " · last: " + lastText
}

// The Backups view, in the filter: per client, its automatic backups and space, then
// its backups (newest first, as listed), walked by one cursor (`index`).
function backupSections(backups, filter, nowMs, statusTargets) {
  var listed = backups && Array.isArray(backups.targets) ? backups.targets : []
  // `backup list` names only clients with backups: every other client `status` knows
  // gets its header (automatic backups, free space), so "daily, but this filesystem
  // cannot clone" shows exactly where none is ever made.
  var targets = listed.slice()
  var known = Array.isArray(statusTargets) ? statusTargets : []
  for (var s = 0; s < known.length; s++) {
    var st = known[s]
    if (!st.backups || targetById(listed, st.id)) continue
    targets.push({ "id": st.id, "total": 0, "shown": 0, "bytes": null, "freeBytes": st.backups.freeBytes,
      "auto": st.backups.auto, "backups": [] })
  }
  targets.sort(function(a, b) { return a.id < b.id ? -1 : a.id > b.id ? 1 : 0 })
  var sections = []
  var index = 0
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    if (filter && t.id !== filter) continue
    var space = (t.freeBytes !== null ? bytesText(t.freeBytes) + " free · " : "") + (t.bytes !== null ? bytesText(t.bytes) + " apparent" : "")
    var lines = [autoLabel(t.auto, nowMs), space].filter(function(l) { return l !== "" })
    if (t.total > t.shown) lines.push(t.shown + " of " + t.total + " backups shown")
    if (t.total === 0) lines.push("No backups yet")
    var rows = t.backups.map(function(b) {
      var parts = [sinceText(b.capturedAt, nowMs) || dateText(b.capturedAt), kindText(b), plural(b.files, "file"), bytesText(b.bytes),
        b.consistency === "stable" ? "stable" : "best effort", integrityText(b)]
      if (b.busy) parts.push("in use")
      if (!b.restorable) parts.push("not restorable")
      return { "key": t.id + "/" + b.id, "index": index++, "target": t.id, "id": b.id, "busy": b.busy, "restorable": b.restorable,
        "line": parts.filter(function(p) { return p !== "" }).join(" · ") }
    })
    sections.push({ "title": t.id, "lines": lines, "rows": rows })
  }
  return sections
}

// `b`: the filtered client, or every client (no confirmation: creation is undoable and
// expected, M3-D9).
function backupRequest(filter) {
  if (filter) return { "template": "backup_target", "slots": { "target": filter }, "label": "Back up " + filter }
  return { "template": "backup_all", "slots": {}, "label": "Back up all" }
}

// Why a client's restores are off: the game runs, or nobody can tell (M3-D29: the CLI
// refuses then too).
function restoreBlocked(statusTargets, target) {
  var t = targetById(statusTargets, target)
  if (!t) return ""
  if (t.running) return "quit the game first"
  if (t.runningUnknown) return "quit the game first (running: unknown)"
  return ""
}

// The chooser's backups for `target`, newest first: when, kind and, from the `-v`
// read, the version of `addon` that wrote its settings.
function chooserBackups(backups, detail, target, addon, nowMs) {
  var listed = targetById(backups && backups.targets, target)
  var detailed = targetById(detail && detail.targets, target)
  var versions = {}
  if (detailed) {
    for (var i = 0; i < detailed.backups.length; i++) {
      var b = detailed.backups[i]
      for (var j = 0; b.addons && j < b.addons.length; j++) {
        if (b.addons[j].name === addon) versions[b.id] = b.addons[j].version
      }
    }
  }
  return (listed ? listed.backups : []).filter(function(b) { return b.restorable }).map(function(b) {
    var version = versions.hasOwnProperty(b.id) && versions[b.id] ? " · " + addon + " " + versions[b.id] : ""
    return { "id": b.id, "label": (sinceText(b.capturedAt, nowMs) || dateText(b.capturedAt)) + " · " + kindText(b) + version }
  })
}

// The restore scopes, in this order on purpose: one addon's settings first, the whole
// WTF/ second, labelled with everything else it changes (risk 11).
function restoreScopes(addon) {
  return [
    { "template": "restore_addon", "label": addon ? addon + "'s settings" : "One addon's settings…", "note": "", "needsAddon": !addon },
    { "template": "restore", "label": "Whole WTF/", "note": "also restores keybindings, macros, chat settings and every other addon's settings", "needsAddon": false }
  ]
}

// The managed addons of `target`, for "one addon's settings…".
function managedAddons(lists, target) {
  var t = targetById(lists && lists.targets, target)
  return t ? t.addons.map(function(a) { return a.name }) : []
}

// -------------------------------------------------------------- the service's desktop triggers (F8)

// The clients that stopped between two compatible statuses: running before, and
// running now known to be false (an unknown state is no transition).
function stoppedTargets(before, after) {
  var out = []
  var now = Array.isArray(after) ? after : []
  for (var i = 0; i < now.length; i++) {
    var was = targetById(before, now[i].id)
    if (was && was.running === true && now[i].running === false && now[i].runningUnknown !== true) out.push(now[i].id)
  }
  return out
}

// The clients whose Lua errors need reading again (M3-D20): `stale` or `unchecked`,
// never `partial` (an incomplete read, not retried on every doorbell).
function errorsToRefresh(targets) {
  return (Array.isArray(targets) ? targets : [])
    .filter(function(t) { return t.errors && (t.errors.state === "stale" || t.errors.state === "unchecked") })
    .map(function(t) { return t.id })
}

// The clients known to be stopped (startup catch-up, M3-D13).
function stoppedNow(targets) {
  return (Array.isArray(targets) ? targets : [])
    .filter(function(t) { return t.running === false && t.runningUnknown !== true && t.missing !== true })
    .map(function(t) { return t.id })
}

var AUTO_CHECK_INTERVAL_MS = 15 * 60 * 1000

// An automatic update check (M3-D12): on, due by `status`, and the last one started
// `intervalMs` (AUTO_CHECK_INTERVAL_MS, 15 minutes) ago or more.
function autoCheckDue(enabled, summary, lastAutoCheckMs, nowMs, intervalMs) {
  var interval = typeof intervalMs === "number" ? intervalMs : AUTO_CHECK_INTERVAL_MS
  return enabled === true && !!summary && summary.checkDue === true && nowMs - lastAutoCheckMs >= interval
}

// A count that rose since the last status; never on the first one.
function rose(before, after, first) {
  return first !== true && num(after) > num(before)
}

// What an error toast may say (M3-D21): the addons and their new counts, and the
// clients, from `status`'s per-client top list. Nothing else of the errors reaches it.
function errorFacts(targets) {
  var names = []
  var counts = []
  var where = []
  var list = Array.isArray(targets) ? targets : []
  for (var i = 0; i < list.length; i++) {
    var errors = list[i].errors
    if (!errors || errors["new"] <= 0) continue
    where.push(list[i].id)
    for (var j = 0; j < errors.top.length; j++) {
      if (errors.top[j]["new"] <= 0) continue
      names.push(errors.top[j].addon)
      counts.push(errors.top[j]["new"])
    }
  }
  return { "names": names, "counts": counts, "targets": where }
}

// U+F0297 nf-md-gamepad_variant: the widget's own glyph.
var TOAST_GLYPH = String.fromCodePoint(0xF0297)

// A toast as an argv for `omarchy-notification-send`; a click summons (opens, never
// toggles) the panel of `pluginId`.
function toastArgv(title, body, urgency, pluginId) {
  return ["omarchy-notification-send", "--app-name", "omaddons", "-g", TOAST_GLYPH, "-u", urgency, title, body,
    "--exec", "omarchy-shell", "shell", "summon", String(pluginId), "{}"]
}

// "Lua errors in your last session · DBM 3 · WeakAuras 1": only `{names, counts,
// targets}`, so no message, path or other error text can reach a toast (F7 scan 7).
function errorToastArgv(facts, pluginId) {
  var names = Array.isArray(facts && facts.names) ? facts.names : []
  var counts = Array.isArray(facts && facts.counts) ? facts.counts : []
  var where = Array.isArray(facts && facts.targets) ? facts.targets : []
  var parts = []
  for (var i = 0; i < names.length; i++) parts.push(String(names[i]) + " " + num(counts[i]))
  var title = "Lua errors in your last session" + (where.length ? " on " + joinNames(where.map(String)) : "")
  return toastArgv(title, parts.join(" · "), "normal", pluginId)
}

// "3 updates on retail and era".
function updateToastArgv(count, targets, pluginId) {
  var where = Array.isArray(targets) ? targets.map(String) : []
  return toastArgv("Addon updates", plural(num(count), "update") + (where.length ? " on " + joinNames(where) : ""), "low", pluginId)
}

// A panel action that finished while no panel was open: its result line.
function resultToastArgv(line, pluginId) {
  return toastArgv("omaddons", String(line), "low", pluginId)
}

// The bound action's refusal after its observation changed (`plan_stale`).
function boundStaleText(template, env) {
  return template === "dismiss_errors" ? "New errors arrived; review them" : staleText(env)
}

// The last result's Undo: its `undo` command when the classifier accepted it.
function resultUndo(result) {
  if (!result || !Array.isArray(result.commands)) return null
  // A change on several clients undoes as one run (`undo --run`); `undo <tx>` would
  // revert one client's part only.
  var single = null
  for (var i = 0; i < result.commands.length; i++) {
    var c = result.commands[i]
    if (!c || !c.runnable || !Array.isArray(c.argv)) continue
    if (c.action === "undo_run") return c
    if (c.action === "undo" && single === null) single = c
  }
  return single
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
    failedItems: failedItems,
    itemFixes: itemFixes,
    isSafeUrl: isSafeUrl,
    fixButtons: fixButtons,
    confirmText: confirmText,
    staleText: staleText,
    fixText: fixText,
    VIEWS: VIEWS,
    VIEW_TITLES: VIEW_TITLES,
    PANEL_KEYS: PANEL_KEYS,
    panelKey: panelKey,
    stepView: stepView,
    heroMeta: heroMeta,
    heroDetail: heroDetail,
    targetById: targetById,
    nextFilter: nextFilter,
    addonsDirArgv: addonsDirArgv,
    updateRequest: updateRequest,
    undoAction: undoAction,
    progressFraction: progressFraction,
    resultUndo: resultUndo,
    sourceKind: sourceKind,
    updateRows: updateRows,
    updateCount: updateCount,
    updatesEmptyText: updatesEmptyText,
    installedRows: installedRows,
    unmanagedLines: unmanagedLines,
    rowAction: rowAction,
    notesFor: notesFor,
    errorLine: errorLine,
    issueSections: issueSections,
    issueRows: issueRows,
    dismissPlan: dismissPlan,
    searchRequest: searchRequest,
    installTargets: installTargets,
    boundStaleText: boundStaleText,
    rawLines: rawLines,
    confidenceDots: confidenceDots,
    popularityText: popularityText,
    sinceText: sinceText,
    dateText: dateText,
    bytesText: bytesText,
    kindText: kindText,
    integrityText: integrityText,
    autoLabel: autoLabel,
    backupSections: backupSections,
    backupRequest: backupRequest,
    restoreBlocked: restoreBlocked,
    chooserBackups: chooserBackups,
    restoreScopes: restoreScopes,
    managedAddons: managedAddons,
    ambiguityChoices: ambiguityChoices,
    backupFolderOf: backupFolderOf,
    stoppedTargets: stoppedTargets,
    errorsToRefresh: errorsToRefresh,
    stoppedNow: stoppedNow,
    AUTO_CHECK_INTERVAL_MS: AUTO_CHECK_INTERVAL_MS,
    autoCheckDue: autoCheckDue,
    rose: rose,
    errorFacts: errorFacts,
    errorToastArgv: errorToastArgv,
    updateToastArgv: updateToastArgv,
    resultToastArgv: resultToastArgv
  }
}
