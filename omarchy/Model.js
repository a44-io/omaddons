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
// A failure mode's explanation shows at most this many instances, each with at most
// this many evidence lines; a plain UI choice.
var WIDGET_EXPLAIN_INSTANCES_MAX = 5
var WIDGET_EXPLAIN_EVIDENCE_MAX = 12

// ------------------------------------------------------ the templates (M3-D2)

// BEGIN COMMANDS: checked by tests/omarchy_widget.rs; every entry must parse as an omaddons command line
// F1 ships the entries for commands that exist at F1 time; F8 adds errors_target and backup_auto (B6, D3);
// F6 adds errors, errors_raw and dismiss_errors (B6, B8); F9 adds backups, backup_target, backup_all,
// restore, restore_addon and prune_backup (D2, D4, D7, D8). G2 adds doctor_fix and doctor_fix_target
// (E6). A `preview` template runs only as its dry run; its confirmed run is `apply` of that preview's
// plan (M4-D28): `match` applies a routine plan that is what the view showed, `confirm` always asks
// first. `bind` runs first as its dry run too, and its confirmed run is the same argv bound to the
// preview's token (`--if-match`): a binding path that stores no plan (M4-D44).
var COMMANDS = {
  "status":         { "argv": ["status", "--cached", "-v"],                         "mutates": false, "lane": "local" },
  "status_full":    { "argv": ["status", "-v"],                                     "mutates": false, "lane": "local" },
  "list":           { "argv": ["list", "-v"],                                       "mutates": false, "lane": "local" },
  "changelog":      { "argv": ["info", "-t", "{target}", "--changelog", "--", "{addon}"], "mutates": false, "lane": "network" },
  "search":         { "argv": ["search", "--limit", "20", "-t", "{target}", "--", "{query}"], "mutates": false, "lane": "network" },
  "check":          { "argv": ["outdated"],                                         "mutates": false, "lane": "network" },
  "update_all":     { "argv": ["update"],                                           "mutates": true, "preview": "match" },
  "update_target":  { "argv": ["update", "-t", "{target}"],                         "mutates": true, "preview": "match" },
  "update_addon":   { "argv": ["update", "-t", "{target}", "--", "{addon}"],        "mutates": true, "preview": "match" },
  "reinstall":      { "argv": ["install", "-t", "{target}", "--reinstall", "--", "{addon}"], "mutates": true, "preview": "confirm" },
  "remove":         { "argv": ["remove", "-t", "{target}", "--", "{addon}"],        "mutates": true, "preview": "confirm" },
  "pin":            { "argv": ["pin", "-t", "{target}", "--", "{addon}"],           "mutates": true },
  "unpin":          { "argv": ["unpin", "-t", "{target}", "--", "{addon}"],         "mutates": true },
  "install":        { "argv": ["install", "-t", "{target}", "--", "{ref}"],         "mutates": true, "preview": "match" },
  "errors":         { "argv": ["errors", "--limit", "10"],                          "mutates": false, "lane": "local" },
  "errors_raw":     { "argv": ["errors", "--raw-details", "-t", "{target}", "--addon={addon}"], "mutates": false, "lane": "local" },
  "dismiss_errors": { "argv": ["errors", "dismiss", "-t", "{target}"],              "mutates": true, "bind": true },
  "search_all":     { "argv": ["search", "--limit", "20", "--", "{query}"],         "mutates": false, "lane": "network" },
  "backups":        { "argv": ["backup", "list", "--limit", "50"],                  "mutates": false, "lane": "local" },
  "backups_detail": { "argv": ["backup", "list", "-v", "-t", "{target}", "--limit", "50"], "mutates": false, "lane": "local" },
  "backup_target":  { "argv": ["backup", "create", "-t", "{target}"],               "mutates": true },
  "backup_all":     { "argv": ["backup", "create"],                                 "mutates": true },
  "restore":        { "argv": ["backup", "restore", "-t", "{target}", "--", "{backup}"], "mutates": true, "preview": "confirm" },
  "restore_addon":  { "argv": ["backup", "restore", "-t", "{target}", "--addon={addon}", "--", "{backup}"], "mutates": true, "preview": "confirm" },
  "prune_backup":   { "argv": ["backup", "prune", "-t", "{target}", "--", "{backup}"],   "mutates": true, "preview": "confirm" },
  "errors_target":  { "argv": ["errors", "--limit", "10", "-t", "{target}"],        "mutates": false, "lane": "local" },
  "backup_auto":    { "argv": ["backup", "create", "--auto", "-t", "{target}"],     "mutates": true, "silent": true },
  "doctor_fix":        { "argv": ["doctor", "fix", "--only={fm}"],                   "mutates": true, "preview": "bind" },
  "doctor_fix_target": { "argv": ["doctor", "fix", "-t", "{target}", "--only={fm}"], "mutates": true, "preview": "bind" }
}
// END COMMANDS

// One row per command path of the binary's registry (`app::COMMANDS`); a command task
// adds its row with its registry row, and tests/omarchy_widget.rs holds them equal.
// `destructive` paths always go through a preview; `binds` paths take `--if-match`.
// BEGIN CLI_PATHS
var CLI_PATHS = {
  "status":            { "mutates": false, "destructive": false, "binds": false },
  "list":              { "mutates": false, "destructive": false, "binds": false },
  "outdated":          { "mutates": false, "destructive": false, "binds": false },
  "info":              { "mutates": false, "destructive": false, "binds": false },
  "search":            { "mutates": false, "destructive": false, "binds": false },
  "errors":            { "mutates": false, "destructive": false, "binds": false },
  "errors dismiss":    { "mutates": true,  "destructive": false, "binds": true },
  "export":            { "mutates": true,  "destructive": false, "binds": false },
  "history":           { "mutates": false, "destructive": false, "binds": false },
  "plans show":        { "mutates": false, "destructive": false, "binds": false },
  "net-log":           { "mutates": false, "destructive": false, "binds": false },
  "doctor":            { "mutates": false, "destructive": false, "binds": false },
  "doctor diagnose":   { "mutates": false, "destructive": false, "binds": false },
  "doctor explain":    { "mutates": false, "destructive": false, "binds": false },
  "doctor fix":        { "mutates": true,  "destructive": false, "binds": true },
  "doctor diff":       { "mutates": false, "destructive": false, "binds": false },
  "doctor undo":       { "mutates": true,  "destructive": true,  "binds": false },
  "doctor gc":         { "mutates": true,  "destructive": true,  "binds": true },
  "install":           { "mutates": true,  "destructive": false, "binds": true },
  "update":            { "mutates": true,  "destructive": false, "binds": true },
  "pin":               { "mutates": true,  "destructive": false, "binds": true },
  "unpin":             { "mutates": true,  "destructive": false, "binds": true },
  "adopt":             { "mutates": true,  "destructive": false, "binds": true },
  "sync":              { "mutates": true,  "destructive": false, "binds": true },
  "copy":              { "mutates": true,  "destructive": false, "binds": true },
  "remove":            { "mutates": true,  "destructive": true,  "binds": true },
  "undo":              { "mutates": true,  "destructive": true,  "binds": true },
  "apply":             { "mutates": true,  "destructive": false, "binds": false },
  "backup":            { "mutates": false, "destructive": false, "binds": false },
  "backup list":       { "mutates": false, "destructive": false, "binds": false },
  "backup create":     { "mutates": true,  "destructive": false, "binds": false },
  "backup restore":    { "mutates": true,  "destructive": true,  "binds": true },
  "backup prune":      { "mutates": true,  "destructive": true,  "binds": true },
  "backup export":     { "mutates": true,  "destructive": false, "binds": false },
  "backup import":     { "mutates": true,  "destructive": false, "binds": false },
  "backup verify":     { "mutates": true,  "destructive": false, "binds": false },
  "targets":           { "mutates": false, "destructive": false, "binds": false },
  "targets add":       { "mutates": true,  "destructive": false, "binds": false },
  "targets rm":        { "mutates": true,  "destructive": true,  "binds": true },
  "targets rename":    { "mutates": true,  "destructive": false, "binds": false },
  "targets default":   { "mutates": true,  "destructive": false, "binds": false },
  "config":            { "mutates": false, "destructive": false, "binds": false },
  "config get":        { "mutates": false, "destructive": false, "binds": false },
  "config set":        { "mutates": true,  "destructive": false, "binds": false },
  "config path":       { "mutates": false, "destructive": false, "binds": false },
  "auth github":       { "mutates": true,  "destructive": false, "binds": false },
  "omarchy":           { "mutates": false, "destructive": false, "binds": false },
  "omarchy status":    { "mutates": false, "destructive": false, "binds": false },
  "omarchy install":   { "mutates": true,  "destructive": false, "binds": false },
  "omarchy remove":    { "mutates": true,  "destructive": false, "binds": false },
  "completions":       { "mutates": false, "destructive": false, "binds": false },
  "robot-docs":        { "mutates": false, "destructive": false, "binds": false },
  "robot-docs schema": { "mutates": false, "destructive": false, "binds": false },
  "robot-docs errors": { "mutates": false, "destructive": false, "binds": false },
  "robot-docs guide":  { "mutates": false, "destructive": false, "binds": false },
  "robot-docs install-skill": { "mutates": true, "destructive": false, "binds": false },
  "capabilities":      { "mutates": false, "destructive": false, "binds": false },
  "help":             { "mutates": false, "destructive": false, "binds": false }
}
// END CLI_PATHS

// ------------------------------------------------------------- building argv

// The flags the widget owns: it sets them on every run, and a CLI-provided argv that
// carries one could re-attribute itself or skip a preview.
var WIDGET_FLAGS = ["--json", "--robot", "--format", "--actor", "--reason", "--progress", "--if-match"]
// Flags whose change no undo brings back: `targets rm --purge` deletes omaddons' backups
// and undo snapshots for a client. The widget never runs one, not even behind a Yes: the
// row shows the command to copy, run from a terminal after its preview.
var IRREVERSIBLE_FLAGS = ["--purge"]
// What a row that shows an irreversible command says about it.
var IRREVERSIBLE_NOTE = "It deletes omaddons' backups and undo snapshots for good, so it runs only from a terminal, after its preview."
var SLOTS = { "target": true, "addon": true, "ref": true, "backup": true, "query": true, "fm": true }
// Exactly one failure-mode id, as `doctor fix --only` takes it (M4-D30): no list, no
// subsystem name, lower case only.
var FM_ID_PATTERN = /^fm-[a-z0-9_-]+$/
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
// The flags that give consent, which only a person in a terminal gives (M4-D38).
var CONSENT_FLAGS = ["--force", "--yes"]

// The first two words of an argument list (no program name), up to its first flag, as
// one string: what pathOf reads, whether or not CLI_PATHS has them.
function leadingWords(args) {
  var words = []
  for (var i = 0; i < args.length && words.length < 2; i++) {
    if (typeof args[i] !== "string" || args[i] === "" || args[i].charAt(0) === "-") break
    words.push(args[i])
  }
  return words.join(" ")
}

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
  if (slot === "fm" && !FM_ID_PATTERN.test(value)) {
    throw refuse("bad_fm", name + ": " + JSON.stringify(value) + " is no failure-mode id")
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
// allow; a `preview` template builds only as its preview (its Yes is its plan's apply).
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
  // `match` and `confirm` confirm through their plan's apply; a `bind` template's real
  // run is its own argv bound to its preview's token, so it needs that token instead.
  var planned = template.preview === "match" || template.preview === "confirm"
  if (planned && !preview) throw refuse("preview_required", name + " runs only as its preview; its Yes runs the plan's apply")
  var ifMatch = ifMatchFor(name, row, template.preview === "confirm", template.bind === true || template.preview === "bind", o, preview)
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

// Whether `plan` (a planFrom result) offered exactly `argv`, element for element.
function offeredBy(plan, argv) {
  if (!plan || typeof plan !== "object" || !Array.isArray(plan.argv) || plan.argv.length !== argv.length) return false
  for (var i = 0; i < argv.length; i++) if (plan.argv[i] !== argv[i]) return false
  return true
}

// Whether an argv the CLI handed out (`fix_argv`, alternatives, `commands[]`, issue
// remediations, `last_change.undo`) may become a button (M3-D2 (a)): `omaddons`, a
// known path, and no flag the widget owns before `--`. The verdict reports the path's
// flags, so the runner previews every destructive one. An `apply` runs only the plan a
// preview offered (M4-D28): given that plan, never from any other envelope.
function classifyCliArgv(argv, plan) {
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
  // Never a change no undo brings back, in any position before `--` (after it, a word
  // is an operand, never a flag).
  for (var k = 1; k < end; k++) {
    if (argv[k].charAt(0) === "-" && IRREVERSIBLE_FLAGS.indexOf(flagName(argv[k])) !== -1) {
      verdict.reason = "irreversible"
      return verdict
    }
  }
  // Consent is given in a terminal by someone who read why (M4-D38): an argv holding
  // `--force` or `--yes` anywhere, past `--` too, is never one key away.
  for (var c = 1; c < argv.length; c++) {
    if (CONSENT_FLAGS.indexOf(flagName(argv[c])) !== -1) {
      verdict.reason = "consent_flag"
      return verdict
    }
  }
  // Read on the words, not on CLI_PATHS: `doctor gc` deletes what repairs keep and may
  // hold an only copy (M4-D36), not even its dry run is the widget's; a repair runs only
  // through the bound templates, and its undo only through `undo`'s stored plan (M4-D22).
  var words = leadingWords(argv.slice(1, end))
  if (words === "doctor gc") {
    verdict.reason = "doctor_gc"
    return verdict
  }
  if (words === "doctor fix" || words === "doctor undo") {
    verdict.reason = "own_flow_only"
    return verdict
  }
  var path = pathOf(argv.slice(1, end))
  if (!path) {
    verdict.reason = "unknown_path"
    return verdict
  }
  if (path === "apply" && !offeredBy(plan, argv)) {
    verdict.reason = "apply_outside_plan"
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
// flags right after it. The options are buildArgv's, and `plan` for the `apply` it
// offered; a destructive path runs only as a preview or after one.
function cliArgv(argv, opts) {
  var o = opts || {}
  var verdict = classifyCliArgv(argv, o.plan)
  if (!verdict.ok) throw refuse(verdict.reason, "not a command the widget runs")
  // A plan's `apply` runs as offered: never as a preview, never with a token (it carries
  // its plan's own), with the standard flags and progress only.
  if (verdict.path === "apply") {
    if (o.preview === true || !!o.token) throw refuse("apply_outside_plan", "a plan runs as offered, never previewed or bound")
    return { argv: standardFlags(o, true, false, "").concat(argv.slice(1)), env: contextOf(o.context) }
  }
  // A plan-storing path confirms through its preview's plan (M4-D28), never a token.
  if (storesPlan(verdict.path) && !!o.token) throw refuse("runs_through_apply", verdict.path + " confirms through its preview's apply, never --if-match")
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
// key; a continuation (the run that carries on a preview: its plan, or the preview
// again after a refused plan) goes first and replaces nothing, nor is it replaced.
// Local reads: one per key, the newest replacing the queued one. Network reads: a newer
// search replaces a queued one, other reads coalesce per key, at most NETWORK_QUEUE_MAX
// queued.
function enqueue(queues, request) {
  var out = copyQueues(queues)
  var lane = laneOf(request)
  var list = out[lane]
  var key = coalesceKey(request)
  var at = -1
  if (lane === "mutation" && request.continuation === true) {
    list.unshift(request)
    return { "queues": out, "accepted": true, "replaced": null }
  }
  if (lane === "mutation") {
    at = request.priority === "user"
      ? indexWhere(list, function(r) { return r.priority === "user" && r.continuation !== true })
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

// A CLI-provided argv as a button, or as text when the classifier refuses it. A forced
// repair is never one key away (M4-D38): the classifier refuses a consent flag anywhere
// (`consent_flag`), so a person reads its explanation and runs it in a terminal.
function cliAction(command, argv, destructive) {
  var verdict = Array.isArray(argv) ? classifyCliArgv(argv) : null
  var runnable = verdict !== null && verdict.ok
  return {
    "command": str(command),
    "argv": runnable ? argv.slice() : null,
    "destructive": destructive === true || (verdict !== null && verdict.destructive),
    "runnable": runnable,
    // A change no undo brings back (a purge): shown with what it deletes, and copied,
    // never run.
    "irreversible": verdict !== null && verdict.reason === "irreversible"
  }
}

// The text a row offers to copy (`y`): the command it shows instead of running, or "".
function copyText(row) {
  if (!row || typeof row !== "object" || row.action) return ""
  return typeof row.text === "string" ? row.text : ""
}

// The clipboard's argv for `text` (a command a row shows): wl-copy with the text as one
// argument, never through a shell; null for nothing to copy or a NUL byte.
function copyArgv(text) {
  if (typeof text !== "string" || text === "" || text.indexOf("\u0000") !== -1) return null
  return ["wl-copy", "--", text]
}

function issueOf(issue) {
  var severity = str(issue.severity)
  var remediation = issue.remediation && typeof issue.remediation === "object" ? issue.remediation : {}
  var explain = issue.explain && typeof issue.explain === "object" ? issue.explain : null
  return {
    "id": str(issue.id),
    "severity": severity,
    "urgent": severity === "P0" || severity === "P1",
    "title": str(issue.title),
    "addon": strOrNull(issue.addon),
    "remediation": cliAction(remediation.command, remediation.argv, remediation.destructive),
    // A repair the doctor makes with no force (`doctor fix --only <id>`, M4-D33).
    "autoFixable": issue.auto_fixable === true,
    // `doctor explain <id> [-t <id>]`: the issue's evidence and what its repair does.
    "explain": explain ? cliAction(explain.command, explain.argv, false) : null,
    // A remediation that needs consent (`--force`, `--yes`): read, then run in a terminal.
    "forced": holdsConsent(remediation.argv)
  }
}

// Whether an argv holds a consent flag anywhere (the classifier's `consent_flag`).
function holdsConsent(argv) {
  if (!Array.isArray(argv)) return false
  for (var i = 0; i < argv.length; i++) {
    if (typeof argv[i] === "string" && CONSENT_FLAGS.indexOf(flagName(argv[i])) !== -1) return true
  }
  return false
}

// An auto-fixable issue's repair, as a bound template (M4-D44): `doctor fix --only=<id>`,
// with `-t` for a client's issue; null for any other issue. The label is the repair's
// recorded `--reason`: one line, at most LABEL_MAX_CHARS.
function repairRequest(issue, targetId) {
  if (!issue || issue.autoFixable !== true || !FM_ID_PATTERN.test(str(issue.id))) return null
  var label = ("Repair: " + str(issue.title)).replace(/[\r\n]+/g, " ")
  if (label.length > LABEL_MAX_CHARS) label = label.slice(0, LABEL_MAX_CHARS - 1) + "…"
  return targetId
    ? { "template": "doctor_fix_target", "slots": { "target": targetId, "fm": issue.id }, "label": label }
    : { "template": "doctor_fix", "slots": { "fm": issue.id }, "label": label }
}

// A problem row's fix, explanation, text and hint (§3 of the repair bead): a repair
// previews through its bound template; a `doctor explain` remediation is the row's
// explanation, never its action; anything else keeps its runnable remediation.
function problemRow(issue, targetId) {
  var r = issue.remediation || {}
  var fix = repairRequest(issue, targetId)
  var explainsOnly = !!r.runnable && verdictPath(r.argv) === "doctor explain"
  var issueExplain = issue.explain && issue.explain.runnable && verdictPath(issue.explain.argv) === "doctor explain"
    ? issue.explain : null
  var explain = explainsOnly ? r : issueExplain
  var action = fix || explainsOnly ? null : (r.runnable ? r : null)
  return {
    "fix": fix,
    "explain": explain,
    "forced": issue.forced === true,
    "action": action,
    "text": r.runnable ? "" : str(r.command),
    "note": r.irreversible ? IRREVERSIBLE_NOTE : "",
    // The key comes from the key map, as every other hint's does.
    "hint": (fix || action || explain ? keyHint("enter", "issues") : "")
      + (fix ? " previews the repair" : action ? " runs the fix" : explain ? " explains it" : "")
  }
}

// The registry path an accepted argv runs, or "".
function verdictPath(argv) {
  var verdict = Array.isArray(argv) ? classifyCliArgv(argv) : null
  return verdict && verdict.ok ? verdict.path : ""
}

// A row's explanation to read: a copy of `argv` when the classifier accepts it as the
// read path `doctor explain`, else null. The service reads nothing else for a row, and
// names no doctor command itself (F7 scan 11).
function explainArgv(argv) {
  return verdictPath(argv) === "doctor explain" ? argv.slice() : null
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
    // The name a player knows the client by (design doc §6.3): never the id when the
    // binary gives one.
    "displayName": str(target.display_name) || str(target.id),
    "version": strOrNull(target.version),
    "family": str(target.family),
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
      "top": Array.isArray(errors.top_addons) ? errors.top_addons.map(function(t) {
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

var KIND_WORDS = { "install": "install", "update": "update", "remove": "removal", "undo": "undo", "restore": "restore",
  "repair": "repair" }

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
            // How many of its files changed on disk (`-v`'s drift).
            "driftFiles": num(drift.files),
            // When its release was installed: the Addons view's "recently updated".
            "installedAt": str(a.installed_at),
            "anyFlavor": a.any_flavor === true,
            "dependency": a.reason === "dependency",
            "orphan": a.orphan === true,
            "requiredBy": stringList(a.required_by),
            "folders": stringList(a.folders),
            // The release `latest` names, as an update preview's approval names it: what
            // the plan runner compares with the plan (M4-D28).
            "channel": str(a.channel),
            "latestSource": strOrNull(latest.source),
            "latestReleaseId": strOrNull(latest.release_id),
            "latestAsset": strOrNull(latest.asset)
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
  if (command === "undo" && data.kind === "repair") {
    var undone = data.reverted && typeof data.reverted === "object" ? data.reverted : {}
    var ids = revertedIds(undone)
    var n = Array.isArray(data.repairs) ? data.repairs.length : 0
    return (undone.kind === "undo" ? "Redid repair" : "Undid repair") + (ids.length ? " on " + joinNames(ids) : "")
      + " (" + plural(n, "change") + ")"
  }
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

function resultLine(env, context, targets) {
  if (!envelopeOk(env)) return "omaddons did not answer"
  // A repair's report is an answer whatever its exit (a dry run that plans exits 1).
  if (doctorResult(env)) return doctorResultLine(env, context, targets)
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

// The ids of a reverted repair's clients (`reverted.targets`, `{id, key}` each).
function revertedIds(reverted) {
  var list = reverted && Array.isArray(reverted.targets) ? reverted.targets : []
  return list.map(function(t) { return t && typeof t === "object" ? str(t.id) : str(t) }).filter(function(id) { return !!id })
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

// Why a stored plan cannot run any more, by `plan_stale`'s `details.reason` (M4-D24):
// the COPY id of its words.
var PLAN_STALE_REASONS = {
  "expired": "stale.expired",
  "binary_changed": "stale.binary_changed",
  "input_missing": "stale.input_missing",
  "already_applied": "stale.already_applied"
}

// The words for `plan_stale`'s `details.changed` parts (design doc §6.5.2), and a bound
// repair's (G2).
var STALE_PARTS = ["targets", "selection", "observed", "command"]

// A `plan_stale` refusal as the plan sheet's banner over the new preview (M3-D46, design
// doc §6.5.2): "Changed since you looked", then what changed. For a stored plan, why it
// cannot run (M4-D24), that it is gone, or the clients where it went stale, by their
// player names from `targets`. Chosen from the codes and `details`, never the message.
function staleText(env, targets) {
  var error = env && env.error && typeof env.error === "object" ? env.error : {}
  var details = error.details && typeof error.details === "object" ? error.details : {}
  // The parts it names, each in a person's words; "; " between them, since a part's own
  // words may hold a comma.
  var known = function(prefix) {
    return stringList(details.changed).filter(function(p) { return STALE_PARTS.indexOf(p) !== -1 })
      .map(function(p) { return copy(prefix + p) }).join("; ")
  }
  // A bound repair (G2): what changed.
  if (env && env.command === "doctor fix") {
    return copy("stale.banner", { "what": known("stale.repair.") || copy("stale.repair.unknown") })
  }
  if (error.code === "plan_not_found") return copy("stale.banner", { "what": copy("stale.gone") })
  if (typeof details.reason === "string" && PLAN_STALE_REASONS.hasOwnProperty(details.reason)) {
    return copy("stale.banner", { "what": copy(PLAN_STALE_REASONS[details.reason]) })
  }
  var data = env && env.data && typeof env.data === "object" ? env.data : {}
  var outcomes = Array.isArray(data.outcomes) ? data.outcomes : []
  var stale = []
  for (var i = 0; i < outcomes.length; i++) {
    if (outcomes[i] && outcomes[i].status === "stale" && typeof outcomes[i].target === "string") {
      stale.push(sheetClient(targets, outcomes[i].target, ""))
    }
  }
  if (stale.length) return copy("stale.banner.there", { "what": copy("stale.landed", { "clients": joinNames(stale) }) })
  return copy("stale.banner", { "what": known("stale.changed.") || copy("stale.changed.unknown") })
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

// The panel's views, in tab order (design doc §2.1: Errors and History join with
// their views, .27 and .30): h/l step through them, the digits jump. The ids stay
// what IPC's `show <view>` takes; "installed" is titled Addons (§2.1, decided).
var VIEWS = ["updates", "installed", "search", "issues", "backups"]
var VIEW_TITLES = { "updates": "Updates", "installed": "Addons", "search": "Search", "issues": "Issues", "backups": "Backups" }
// Each view's glyph (§5.4), which the tabs show alone below 480 px (§2.1).
var VIEW_GLYPHS = { "updates": 0xF0CE1, "installed": 0xF0431, "search": 0xF0349, "issues": 0xF0026, "backups": 0xF003C }

// ------------------------------------------------------------------ the key map (design doc §3)

// Where a key means something, in the order the key reference and README list them.
// `panel` is anywhere outside a dialog (a problem card included), `view` every view's
// list, a view's name that view alone; `field` the search box while it has the
// keyboard, `card` the card shown instead of the views when omaddons gave no answer to
// work with, `sheet` every dialog (the confirmation and both choosers), `help` the key
// reference.
// BEGIN KEY_CONTEXTS: checked by tests/omarchy_widget.rs against README's key table
var KEY_CONTEXTS = [
  { "id": "panel", "title": "Anywhere" },
  { "id": "view", "title": "Every view" },
  { "id": "updates", "title": "Updates" },
  { "id": "installed", "title": "Addons" },
  { "id": "search", "title": "Search" },
  { "id": "issues", "title": "Issues" },
  { "id": "backups", "title": "Backups" },
  { "id": "field", "title": "Search box" },
  { "id": "filter", "title": "Filter box" },
  { "id": "card", "title": "Problem card" },
  { "id": "sheet", "title": "Dialog" },
  { "id": "help", "title": "Key reference" }
]
// END KEY_CONTEXTS

// Every key the panel answers to, one row per action and context: the dispatcher looks
// keys up here (panelKey, dialogKey, helpKey), the key reference lists these rows, every
// button's key hint comes from them (keyHint, withKey, keyTip), and README's key table
// is checked against them. `keys` are key names as keyName gives them; `hint` is how a
// tooltip or the reference writes them; `nothing` is what a view-only key says in a
// view where it does nothing; `confirms` marks a key that confirms a dialog, which an
// auto-repeated press never does (M3-D46). A row joins this table with the action it
// runs: no key is listed before it does something.
// BEGIN KEYMAP: checked by tests/omarchy_widget.rs against README's key table
var KEYMAP = [
  { "keys": ["h", "l", "Left", "Right"], "context": "panel", "action": "view_step", "label": "previous or next view (on a page, h goes back)", "hint": "h l" },
  { "keys": ["1", "2", "3", "4", "5"], "context": "panel", "action": "view", "label": "view 1 to 5: Updates, Addons, Search, Issues, Backups", "hint": "1–5" },
  { "keys": ["?"], "context": "panel", "action": "help", "label": "this key reference", "hint": "?" },
  { "keys": ["z"], "context": "panel", "action": "zoom", "label": "fill the screen, and back", "hint": "z" },
  { "keys": ["Ctrl+=", "Ctrl+-", "Ctrl+0"], "context": "panel", "action": "text_zoom", "label": "bigger or smaller text, and back to normal (or Ctrl+wheel)", "hint": "Ctrl+= Ctrl+-" },
  { "keys": ["Esc"], "context": "panel", "action": "close", "label": "back out one layer: a page, then a filter, then the panel closes", "hint": "Esc" },
  { "keys": ["Tab", "Shift+Tab"], "context": "panel", "action": "tab", "label": "the bar's next or previous panel", "hint": "Tab" },
  { "keys": ["j", "k", "Down", "Up"], "context": "view", "action": "move", "label": "move the cursor", "hint": "j k" },
  { "keys": ["g", "G"], "context": "view", "action": "edge", "label": "first or last row", "hint": "g G" },
  { "keys": ["PgUp", "PgDn"], "context": "view", "action": "page", "label": "a page up or down", "hint": "PgUp PgDn" },
  { "keys": ["t", "T"], "context": "view", "action": "client", "label": "next or previous client (All, then each client on this computer)", "hint": "t T" },
  { "keys": ["s"], "context": "view", "action": "search", "label": "Search, its box ready to type in", "hint": "s" },
  { "keys": ["c"], "context": "view", "action": "check", "label": "check for updates now", "hint": "c" },
  { "keys": ["U"], "context": "view", "action": "update_all", "label": "update everything on the chosen client, or on every client", "hint": "U" },
  { "keys": ["Ctrl+Z"], "context": "view", "action": "undo", "label": "undo the last change, after a confirmation", "hint": "Ctrl+Z" },
  { "keys": ["o"], "context": "view", "action": "open", "label": "open the chosen client's AddOns folder (under All, the row's client)", "hint": "o" },
  { "keys": ["b"], "context": "view", "action": "backup", "label": "back up the chosen client's settings now, or every client's", "hint": "b" },
  { "keys": ["d"], "context": "view", "action": "dismiss", "label": "mark the new Lua errors shown as seen", "hint": "d" },
  { "keys": ["Enter", "Space"], "context": "updates", "action": "enter", "label": "update this addon", "hint": "Enter" },
  { "keys": ["u"], "context": "updates", "action": "update", "label": "update this addon", "hint": "u", "nothing": "Nothing to update here" },
  { "keys": ["p"], "context": "updates", "action": "pin", "label": "pin or unpin it", "hint": "p", "nothing": "Nothing to pin here" },
  { "keys": ["e"], "context": "updates", "action": "notes", "label": "show or hide its newest release notes", "hint": "e", "nothing": "No release notes here" },
  { "keys": ["Enter", "Space"], "context": "installed", "action": "enter", "label": "update it, or put back its changed files", "hint": "Enter" },
  { "keys": ["u"], "context": "installed", "action": "update", "label": "update this addon", "hint": "u", "nothing": "Nothing to update here" },
  { "keys": ["p"], "context": "installed", "action": "pin", "label": "pin or unpin it", "hint": "p", "nothing": "Nothing to pin here" },
  { "keys": ["x"], "context": "installed", "action": "remove", "label": "remove it, after a confirmation", "hint": "x", "nothing": "Nothing to remove here" },
  { "keys": ["r"], "context": "installed", "action": "restore", "label": "restore its settings from a backup", "hint": "r", "nothing": "No settings to restore here" },
  { "keys": ["/"], "context": "installed", "action": "filter", "label": "filter the list by name, source or state", "hint": "/", "nothing": "Nothing to filter here" },
  { "keys": ["S"], "context": "installed", "action": "sort", "label": "sort by name, by recently updated, or by source", "hint": "S", "nothing": "Nothing to sort here" },
  { "keys": ["Enter", "Space"], "context": "issues", "action": "enter", "label": "preview a repair, run the fix, or show why; on a Lua error, update its addon or show the stack", "hint": "Enter" },
  { "keys": ["u"], "context": "issues", "action": "update", "label": "update the addon a Lua error comes from", "hint": "u", "nothing": "Nothing to update here" },
  { "keys": ["r"], "context": "issues", "action": "restore", "label": "restore the erroring addon's settings from a backup", "hint": "r", "nothing": "No settings to restore here" },
  { "keys": ["y"], "context": "issues", "action": "copy", "label": "copy the command a row shows, to run in a terminal", "hint": "y", "nothing": "Nothing to copy here" },
  { "keys": ["Enter", "Space"], "context": "backups", "action": "enter", "label": "restore from this backup: choose what", "hint": "Enter" },
  { "keys": ["r"], "context": "backups", "action": "restore", "label": "restore from this backup: choose what", "hint": "r", "nothing": "No settings to restore here" },
  { "keys": ["x"], "context": "backups", "action": "remove", "label": "delete this backup, after a confirmation", "hint": "x", "nothing": "Nothing to remove here" },
  { "keys": ["Enter", "Space"], "context": "search", "action": "enter", "label": "install the result", "hint": "Enter" },
  { "keys": ["i"], "context": "search", "action": "install", "label": "install the result", "hint": "i", "nothing": "Nothing to install here" },
  { "keys": ["Enter"], "context": "field", "action": "search", "label": "search", "hint": "Enter" },
  { "keys": ["Esc"], "context": "field", "action": "leave", "label": "leave the box (its text stays)", "hint": "Esc" },
  { "keys": ["Down", "Up"], "context": "filter", "action": "move", "label": "move the cursor in the list (the box keeps the keys)", "hint": "Down Up" },
  { "keys": ["Enter"], "context": "filter", "action": "first", "label": "go to the first match, and leave the box", "hint": "Enter" },
  { "keys": ["Esc"], "context": "filter", "action": "leave", "label": "leave the box (the filter stays: Esc again clears it)", "hint": "Esc" },
  { "keys": ["j", "k", "Down", "Up"], "context": "card", "action": "move", "label": "move between the card's buttons", "hint": "j k" },
  { "keys": ["Enter", "Space"], "context": "card", "action": "press", "label": "press the button", "hint": "Enter" },
  { "keys": ["Esc", "n", "N"], "context": "sheet", "action": "cancel", "label": "cancel, or go back a step", "hint": "Esc n" },
  { "keys": ["j", "k", "Down", "Up"], "context": "sheet", "action": "line", "label": "scroll a plan sheet, or move between a chooser's choices", "hint": "j k" },
  { "keys": ["h", "l", "Left", "Right", "Tab", "Shift+Tab"], "context": "sheet", "action": "switch", "label": "move between Cancel and the confirm button", "hint": "h l" },
  { "keys": ["y", "Y"], "context": "sheet", "action": "confirm", "label": "confirm a plan sheet", "hint": "y", "confirms": true },
  { "keys": ["Enter", "Space"], "context": "sheet", "action": "press", "label": "the button or choice under the cursor (a plan sheet starts on Cancel)", "hint": "Enter", "confirms": true },
  { "keys": ["?", "Esc"], "context": "help", "action": "close", "label": "close the reference", "hint": "? Esc" },
  { "keys": ["j", "k", "Down", "Up"], "context": "help", "action": "scroll", "label": "scroll", "hint": "j k" }
]
// END KEYMAP

// Qt's key codes for the keys KEYMAP names (stable across Qt 6; this file reads no QML
// global), and the modifier bits keyName reads.
var QT_KEY_NAMES = {
  "16777216": "Esc", "16777217": "Tab", "16777218": "Shift+Tab", "16777220": "Enter", "16777221": "Enter",
  "16777234": "Left", "16777235": "Up", "16777236": "Right", "16777237": "Down",
  "16777238": "PgUp", "16777239": "PgDn", "32": "Space"
}
var QT_SHIFT = 0x02000000
var QT_CONTROL = 0x04000000

// A key event's name in KEYMAP's terms: "Enter", "Shift+Tab", "Ctrl+Z", or the one
// character it types; "" for anything else.
function keyName(code, text, modifiers) {
  var mods = typeof modifiers === "number" ? modifiers : 0
  var named = QT_KEY_NAMES[String(code)]
  if (named === "Tab" && (mods & QT_SHIFT)) return "Shift+Tab"
  if (named) return named
  if ((mods & QT_CONTROL) && ((code >= 0x41 && code <= 0x5a) || (code >= 0x30 && code <= 0x39) || code === 0x3d || code === 0x2d))
    return "Ctrl+" + String.fromCharCode(code)
  if (typeof text === "string" && text.length === 1 && text >= " " && text !== "\u007f") return text
  return ""
}

// The direction a key steps in, for the rows that hold both directions (j and k, h and
// l, g and G, t and T, PgUp and PgDn): 1 forward or down, -1 back or up, else 0.
var KEY_STEPS = {
  "j": 1, "Down": 1, "l": 1, "Right": 1, "G": 1, "PgDn": 1, "t": 1, "Tab": 1, "Ctrl+=": 1,
  "k": -1, "Up": -1, "h": -1, "Left": -1, "g": -1, "PgUp": -1, "T": -1, "Shift+Tab": -1, "Ctrl+-": -1
}

function keyStep(name) {
  return KEY_STEPS.hasOwnProperty(name) ? KEY_STEPS[name] : 0
}

// The KEYMAP contexts a key is looked up in, from where the keyboard is: a view's name,
// "page" (a page on the navigation stack: the frame's keys, and the page's own rows when
// pages bring some, .23), "card", "field", "sheet" or "help".
// The text box each view has, whose keys the key reference lists with the view's.
var VIEW_BOXES = { "search": "field", "installed": "filter" }

function keyScopes(context) {
  if (context === "card") return ["panel", "card"]
  if (VIEWS.indexOf(context) >= 0 || context === "page") return ["panel", "view", context]
  return [context]
}

function keyRows(context) {
  return KEYMAP.filter(function(r) { return r.context === context })
}

// The row the key `name` runs in `context`, or null.
function keyRow(name, context) {
  var scopes = keyScopes(context)
  for (var s = 0; s < scopes.length; s++) {
    var rows = keyRows(scopes[s])
    for (var i = 0; i < rows.length; i++) if (rows[i].keys.indexOf(name) >= 0) return rows[i]
  }
  return null
}

// What the key `name` does in `context` (a view's name, or "card"): `{action, step}`,
// with `view` for a digit; `{action: null, note}` for a key that acts only in other
// views (design doc §3.3: a key that does nothing here says why); or null.
function panelKey(name, context) {
  if (typeof name !== "string" || name === "") return null
  var where = typeof context === "string" && context !== "" ? context : VIEWS[0]
  var row = keyRow(name, where)
  if (row) {
    var found = { "action": row.action, "step": keyStep(name) }
    if (row.action === "view") found.view = VIEWS[row.keys.indexOf(name)]
    return found
  }
  if (VIEWS.indexOf(where) < 0) return null
  for (var i = 0; i < KEYMAP.length; i++) {
    var other = KEYMAP[i]
    if (VIEWS.indexOf(other.context) >= 0 && other.keys.indexOf(name) >= 0 && other.nothing)
      return { "action": null, "note": other.nothing }
  }
  return null
}

// What a dialog does with the key `name` (KEYMAP's sheet rows): "cancel", "line",
// "switch", "confirm", "press", or "inert" (every other key is the dialog's too, and
// does nothing). A confirming key that is auto-repeat is inert: Yes needs a fresh press,
// so a key held from before the dialog opened never confirms it (M3-D46).
function dialogKey(name, autoRepeat) {
  var row = keyRow(name, "sheet")
  if (!row) return "inert"
  if (row.confirms && autoRepeat) return "inert"
  return row.action
}

// What the key reference does with the key `name`: "close", "scroll" or "inert".
function helpKey(name) {
  var row = keyRow(name, "help")
  return row ? row.action : "inert"
}

// The first row running `action`, in `context`'s scopes when given.
function actionRow(action, context) {
  var rows = typeof context === "string" && context !== ""
    ? [].concat.apply([], keyScopes(context).map(keyRows)) : KEYMAP
  for (var i = 0; i < rows.length; i++) if (rows[i].action === action) return rows[i]
  return null
}

// How a tooltip writes the key of `action`: "c", "Ctrl+Z"; "" when no key runs it.
function keyHint(action, context) {
  var row = actionRow(action, context)
  return row ? row.hint : ""
}

// `text` with the key of `action` after it: "Check now (c)".
function withKey(text, action, context) {
  var hint = keyHint(action, context)
  return hint ? text + " (" + hint + ")" : text
}

// A button's tooltip: what its key does, and the key ("Check for updates now (c)").
function keyTip(action, context) {
  var row = actionRow(action, context)
  if (!row) return ""
  return row.label.charAt(0).toUpperCase() + row.label.slice(1) + " (" + row.hint + ")"
}

function contextTitle(id) {
  for (var i = 0; i < KEY_CONTEXTS.length; i++) if (KEY_CONTEXTS[i].id === id) return KEY_CONTEXTS[i].title
  return ""
}

// The key reference for `context` (a view's name, or "card"): its own rows first, then
// the frame's, then a dialog's, each `{title, rows: [{hint, label}]}`.
function helpSections(context) {
  var ids = keyScopes(context).slice().reverse()
  // A view's box (Search's search box, Addons' filter box) right after its own keys.
  if (VIEW_BOXES.hasOwnProperty(context)) ids.splice(1, 0, VIEW_BOXES[context])
  ids = ids.concat(["sheet"])
  return ids.map(function(id) {
    return {
      "title": contextTitle(id),
      "rows": keyRows(id).map(function(r) { return { "hint": r.hint, "label": r.label } })
    }
  }).filter(function(s) { return s.rows.length > 0 })
}

// The view `step` places from `view`, wrapping around.
function stepView(view, step) {
  var at = VIEWS.indexOf(view)
  if (at < 0) at = 0
  var n = VIEWS.length
  return VIEWS[(((at + step) % n) + n) % n]
}

// The clients a scope covers: the one `filter` names, or every client under All.
function scopeTargets(targets, filter) {
  var list = Array.isArray(targets) ? targets : []
  return filter ? list.filter(function(t) { return t.id === filter }) : list
}

// The clients on this computer: every client `status` lists but a missing one (its
// directory gone, or its launcher uninstalled it). "6 clients" counts these, and `t`
// steps through these (design doc §2.2; omad-widget-user-first-1n2p.43's observation).
function liveTargets(targets) {
  return (Array.isArray(targets) ? targets : []).filter(function(t) { return !!t && t.missing !== true })
}

// The scope a panel opens on (design doc §2.2): the client that runs, when exactly one
// does; else `last`, the scope used last in this shell session, while it names a client
// still on this computer; else All (""). A missing client is never a default scope.
function openingScope(targets, last) {
  var live = liveTargets(targets)
  var running = live.filter(function(t) { return t.running === true })
  if (running.length === 1) return running[0].id
  var kept = targetById(live, str(last))
  return kept ? kept.id : ""
}

// The scope after `status` answered again: `filter` while `targets` still lists its
// client (a missing one included: it was chosen, and says what happened to it), else
// All, for a client that was forgotten or renamed.
function keptScope(targets, filter) {
  return filter && targetById(targets, filter) ? filter : ""
}

// A client's group under All (design doc §2.2, §2.3): its player name, then whether it
// runs and the group's count, as a section header draws them ("RETAIL" with "running ·
// 3 updates").
function clientGroup(statusTargets, id, count) {
  var t = targetById(statusTargets, id)
  var parts = []
  if (t && t.running) parts.push(copy("client.running"))
  else if (t && t.runningUnknown) parts.push(copy("client.running_unknown"))
  if (count) parts.push(count)
  return { "title": sheetClient(statusTargets, id, ""), "count": parts.join(" · ") }
}

// A missing client's "Forget {client}…" (design doc §2.2, §6.5.2): the plain `targets rm
// <id>` its own problem offers as a runnable command, the forget flow's plan sheet
// before it runs; null when no problem offers it (a purge is never runnable here, F7
// scan 12).
function forgetAction(target) {
  if (!target || target.missing !== true || !Array.isArray(target.issues)) return null
  for (var i = 0; i < target.issues.length; i++) {
    var r = target.issues[i] ? target.issues[i].remediation : null
    if (!r || !r.runnable || !Array.isArray(r.argv) || verdictPath(r.argv) !== "targets rm") continue
    if (r.argv.length === 4 && r.argv[3] === target.id) return r
  }
  return null
}

// What needs a player in a scope, counted (design doc §2.1): new Lua errors, problems
// (`urgent` the P0 and P1 ones), updates; the running clients, by name, and whether
// the process table could not tell. Under All the machine's own problems count too.
function scopeFacts(targets, filter, machineIssues) {
  var scoped = scopeTargets(targets, filter)
  var facts = { "errors": 0, "problems": 0, "urgent": 0, "updates": 0, "running": [], "unknown": false }
  var issues = filter || !Array.isArray(machineIssues) ? [] : machineIssues.slice()
  for (var i = 0; i < scoped.length; i++) {
    var t = scoped[i]
    if (t.errors) facts.errors += t.errors["new"]
    if (t.updates) facts.updates += t.updates.available
    if (Array.isArray(t.issues)) issues = issues.concat(t.issues)
    if (t.running) facts.running.push(t.displayName || t.id)
    if (t.runningUnknown) facts.unknown = true
  }
  facts.problems = issues.length
  facts.urgent = issues.filter(function(issue) { return issue.urgent === true }).length
  return facts
}

// The hero's title: the scope (§2.1), "All clients" or the client's player name.
function heroTitle(targets, filter) {
  if (!filter) return copy("client.all")
  var t = targetById(targets, filter)
  return t ? (t.displayName || t.id) : filter
}

// The hero's meta line, urgent first, quiet parts left out, in capitals (§2.1, §5.2):
// "2 NEW ERRORS · 1 PROBLEM · 3 UPDATES · RETAIL RUNNING", "ALL GOOD"; "" before the
// first answer.
function heroMeta(summary, targets, filter, machineIssues) {
  if (!summary) return ""
  var facts = scopeFacts(targets, filter, machineIssues)
  var parts = []
  if (facts.errors > 0) parts.push(plural(facts.errors, "new error"))
  if (facts.urgent > 0) parts.push(plural(facts.problems, "problem"))
  if (facts.updates > 0) parts.push(plural(facts.updates, "update"))
  if (facts.urgent === 0 && facts.problems > 0) parts.push(plural(facts.problems, "problem"))
  if (parts.length === 0) parts.push("all good")
  if (facts.running.length > 0) parts.push(filter ? "running" : joinNames(facts.running) + " running")
  else if (facts.unknown) parts.push("can't tell if the game is running")
  return parts.join(" · ").toUpperCase()
}

// The hero glyph's state (§2.1): "urgent" for new Lua errors or a P0 or P1 problem,
// "busy" while a change runs, else "quiet".
function heroState(summary, targets, filter, machineIssues, busy) {
  if (summary) {
    var facts = scopeFacts(targets, filter, machineIssues)
    if (facts.errors > 0 || facts.urgent > 0) return "urgent"
  }
  return busy ? "busy" : "quiet"
}

// The hero's detail (§2.1): the client's version and family under one client
// ("12.1.5.70391 · mainline"), else how many clients there are.
function heroDetail(summary, targets, filter) {
  if (!summary) return ""
  if (!filter) return plural(liveTargets(targets).length, "client")
  var t = targetById(targets, filter)
  if (!t) return ""
  if (t.missing) return "not on this computer any more"
  return [t.version || "", t.family].filter(function(p) { return !!p }).join(" · ")
}

// The freshness footer (§2.1, §6.4): "6 clients · checked 25 min ago · Retail
// running"; under one client "checked 25 min ago · running". The check is the scope's
// oldest; "never checked" when none was. A missing client is not counted.
function footerText(summary, targets, filter, nowMs) {
  if (!summary) return ""
  var scoped = scopeTargets(targets, filter)
  var parts = filter ? [] : [plural(liveTargets(scoped).length, "client")]
  var oldest = null
  for (var i = 0; i < scoped.length; i++) {
    var checked = scoped[i].updates ? scoped[i].updates.checkedAt : null
    if (checked && (!oldest || Date.parse(checked) < Date.parse(oldest))) oldest = checked
  }
  if (scoped.length > 0) parts.push(oldest ? ageText(oldest, nowMs) : "never checked")
  var facts = scopeFacts(targets, filter, [])
  if (facts.running.length > 0) parts.push(filter ? "running" : joinNames(facts.running) + " running")
  return parts.join(" · ")
}

// The scope Dropdown's options (§2.1, §2.2): "All clients · 3 updates · 2 new errors"
// first, then each client by its player name with its updates and whether it runs, in
// `status`'s order; a missing client comes last, as "{name} · not found" (decided: last,
// so the clients on this computer stay together at the top; the kit's Dropdown draws
// every option alike, so the words say it, not a dimmed row).
function scopeOptions(targets) {
  var list = Array.isArray(targets) ? targets : []
  var counts = function(facts) {
    var parts = []
    if (facts.updates > 0) parts.push(plural(facts.updates, "update"))
    if (facts.errors > 0) parts.push(plural(facts.errors, "new error"))
    return parts
  }
  var options = [{ "value": "", "label": [copy("client.all")].concat(counts(scopeFacts(list, "", []))).join(" · ") }]
  var gone = []
  for (var i = 0; i < list.length; i++) {
    var t = list[i]
    var parts = [t.displayName || t.id]
    if (t.missing) {
      gone.push({ "value": t.id, "label": parts.concat([copy("client.not_found")]).join(" · ") })
      continue
    }
    parts = parts.concat(counts(scopeFacts(list, t.id, [])))
    if (t.running) parts.push(copy("client.running"))
    else if (t.runningUnknown) parts.push(copy("client.running_unknown"))
    options.push({ "value": t.id, "label": parts.join(" · ") })
  }
  return options.concat(gone)
}

// Each tab's count in the scope (§2.1), `{count, urgent}` (a zero count is not shown):
// Updates its updates; Issues its problems and, until the Errors view (.27) takes
// them, its new Lua errors, urgent for a P0 or P1 problem or a new error; Backups the
// clients `status` asks to back up (`backup_create`, no checkpoint in 7 days).
function viewCounts(targets, filter, commands, machineIssues) {
  var facts = scopeFacts(targets, filter, machineIssues)
  var scoped = scopeTargets(targets, filter).map(function(t) { return t.id })
  var backups = 0
  var list = Array.isArray(commands) ? commands : []
  for (var i = 0; i < list.length; i++) {
    var argv = Array.isArray(list[i].argv) ? list[i].argv : []
    var at = argv.indexOf("-t")
    if (list[i].action === "backup_create" && at >= 0 && scoped.indexOf(argv[at + 1]) >= 0) backups++
  }
  var none = { "count": 0, "urgent": false }
  return {
    "updates": { "count": facts.updates, "urgent": false },
    "installed": none,
    "search": none,
    "issues": { "count": facts.problems + facts.errors, "urgent": facts.urgent > 0 || facts.errors > 0 },
    "backups": { "count": backups, "urgent": false }
  }
}

function targetById(targets, id) {
  if (!id || !Array.isArray(targets)) return null
  for (var i = 0; i < targets.length; i++) if (targets[i].id === id) return targets[i]
  return null
}

// `t`: All (""), then each target in order, then All again. A filter whose target is
// gone starts over at All.
function nextFilter(targets, current) {
  return stepFilter(targets, current, 1)
}

// `t` (`step` 1) and `T` (-1): All ("") and each client on this computer, in a ring. A
// missing client is left out (decided: on the development machine four uninstalled
// clients stood between the two that are there; the Dropdown still reaches them), and a
// filter on one, or on a client that is gone, starts over at All.
function stepFilter(targets, current, step) {
  var ring = [""].concat(liveTargets(targets).map(function(t) { return t.id }))
  var at = ring.indexOf(current || "")
  if (at < 0) return ""
  var n = ring.length
  return ring[(((at + (step < 0 ? -1 : 1)) % n) + n) % n]
}

// `o`: the selected target's AddOns folder for the desktop's opener (M3-D2: a literal
// tool, an argv); null without a selected target or an absolute client directory.
function addonsDirArgv(target) {
  if (!target || !target.clientDir || target.clientDir.charAt(0) !== "/") return null
  return ["xdg-open", target.clientDir.replace(/\/+$/, "") + "/Interface/AddOns"]
}

// `o`'s client (design doc §2.2): the scoped one, or under All the client of the row
// under the cursor (`rowTarget`, "" when there is none); null otherwise, so `o` never
// opens a guessed client's folder, and never a missing client's.
function openTarget(targets, filter, rowTarget) {
  var t = targetById(targets, filter || str(rowTarget))
  return t && t.missing !== true ? t : null
}

// `U`: every client's updates, or the selected one's, previewed (M4-D28). Given the
// Updates view's `rows` (updateRows'), `shown` is what the view showed of them, the
// update rows in the filter, which the plan runner compares with the plan. The label
// names the client by its player name (`targets`, `status`'s; its id without them).
function updateRequest(filter, rows, targets) {
  var request = filter ? { "template": "update_target", "slots": { "target": filter }, "label": "Update " + sheetClient(targets, filter, "") }
    : { "template": "update_all", "slots": {}, "label": "Update all" }
  if (Array.isArray(rows)) {
    request.shown = rows.filter(function(r) { return !!r && r.kind === "update" && (!filter || r.target === filter) }).map(shownOf)
  }
  return request
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

// ------------------------------------------------------------------ colours (§5.1)

// A colour as {r, g, b} in 0..1: QML's color values, {r, g, b} objects and "#rrggbb"
// (or "#aarrggbb", alpha ignored) strings all read; anything else is null.
function rgbOf(c) {
  if (c && typeof c === "object" && typeof c.r === "number" && typeof c.g === "number" && typeof c.b === "number")
    return { "r": c.r, "g": c.g, "b": c.b }
  var text = String(c === undefined || c === null ? "" : c)
  var m = text.match(/^#(?:[0-9a-fA-F]{2})?([0-9a-fA-F]{2})([0-9a-fA-F]{2})([0-9a-fA-F]{2})$/)
  if (!m) return null
  return { "r": parseInt(m[1], 16) / 255, "g": parseInt(m[2], 16) / 255, "b": parseInt(m[3], 16) / 255 }
}

function luminance(rgb) {
  var lin = function(v) { return v <= 0.03928 ? v / 12.92 : Math.pow((v + 0.055) / 1.055, 2.4) }
  return 0.2126 * lin(rgb.r) + 0.7152 * lin(rgb.g) + 0.0722 * lin(rgb.b)
}

// The WCAG contrast ratio of two colours (1 to 21), or 1 when either does not read.
function contrastRatio(a, b) {
  var x = rgbOf(a)
  var y = rgbOf(b)
  if (!x || !y) return 1
  var lx = luminance(x)
  var ly = luminance(y)
  return (Math.max(lx, ly) + 0.05) / (Math.min(lx, ly) + 0.05)
}

// `fg` laid over `bg` at `alpha`, opaque, as "#rrggbb".
function mixColor(fg, bg, alpha) {
  var f = rgbOf(fg)
  var b = rgbOf(bg)
  if (!f || !b) return "#000000"
  var hex = function(v) { return ("0" + Math.round(Math.max(0, Math.min(1, v)) * 255).toString(16)).slice(-2) }
  return "#" + hex(f.r * alpha + b.r * (1 - alpha)) + hex(f.g * alpha + b.g * (1 - alpha)) + hex(f.b * alpha + b.b * (1 - alpha))
}

var DIM_ALPHA_FLOOR = 0.55
var DIM_MIN_CONTRAST = 4.5

// The alpha `dimColor` lays the foreground at: the smallest from 0.55 up, in steps of
// 0.01, at which it reaches 4.5:1 on `bg`; 1 when even the foreground does not.
function dimAlpha(fg, bg) {
  for (var step = Math.round(DIM_ALPHA_FLOOR * 100); step <= 100; step++) {
    if (contrastRatio(mixColor(fg, bg, step / 100), bg) >= DIM_MIN_CONTRAST) return step / 100
  }
  return 1
}

// Secondary text (§5.1): the foreground over the popup background, dimmed as far as
// 4.5:1 allows. Unlike Qt.darker it dims on a light theme too, where darker would
// stand out more than the foreground (UX-44).
function dimColor(fg, bg) {
  return mixColor(fg, bg, dimAlpha(fg, bg))
}

// `list` sorted by `compare`, items it calls equal kept in their input order. Qt's V4
// engine does not sort stably (bun's does), so a comparator that ties would shuffle the
// CLI's own order: every sort of rows goes through here.
function stableSort(list, compare) {
  var decorated = list.map(function(item, index) { return { "item": item, "index": index } })
  decorated.sort(function(a, b) { return compare(a.item, b.item) || a.index - b.index })
  return decorated.map(function(d) { return d.item })
}

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
        "group": "", "groupCount": "", "targetKey": t.key, "channel": a.channel, "latestSource": a.latestSource,
        "latestReleaseId": a.latestReleaseId, "latestAsset": a.latestAsset
      })
    }
    // Within a kind, the order `list -v` gave.
    rows = stableSort(rows, function(x, y) { return UPDATE_ORDER.indexOf(x.kind) - UPDATE_ORDER.indexOf(y.kind) })
    // Under All, the client's header above its first row: its player name, whether it
    // runs, and its updates (design doc §2.3).
    if (!filter && rows.length > 0) {
      var updates = updateCount(rows)
      var group = clientGroup(statusTargets, t.id, updates > 0 ? plural(updates, "update") : "")
      rows[0].group = group.title
      rows[0].groupCount = group.count
    }
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
// The oldest update check of the clients in `filter` (every client under All), or null
// when none was checked.
function oldestCheck(statusTargets, filter) {
  var oldest = null
  var targets = Array.isArray(statusTargets) ? statusTargets : []
  for (var i = 0; i < targets.length; i++) {
    if (filter && targets[i].id !== filter) continue
    var checked = targets[i].updates ? targets[i].updates.checkedAt : null
    if (checked && (!oldest || Date.parse(checked) < Date.parse(oldest))) oldest = checked
  }
  return oldest
}

// -------------------------------------------------------------- the views' states (1n2p.19)
//
// What a view says around its rows (design doc §7's matrix): `firstLoad` before its data
// first arrives, `failed` when that read failed, `empty` and `filteredEmpty` with a
// sentence and the next action, else `rows`; and over rows that stay on screen, a quiet
// `refreshing` while a read runs and notices: `failed` (a re-read failed, the last good
// data shown), `offline` (a source did not answer), `partial` (a read cut short) and
// `stale` (a check older than its interval). Decided from the service's fields alone:
// its data, the reads running (`reading`), the last outcome per resource, the last
// check, the status's targets; never a message's text.

// The resource each view reads.
var VIEW_RESOURCES = { "updates": "list", "installed": "list", "search": "search", "issues": "errors", "backups": "backups" }
// The codes that mean a source did not answer: on the last check's warnings (the check
// then showed what it remembered) or its failed items.
var SOURCE_TROUBLE = ["network", "http_status", "rate_limited", "offline", "stale_check"]
// A state's glyph (design doc §5.4): loading spins, a failure alerts, an empty view shows
// its own (all good is a check).
var STATE_GLYPHS = { "firstLoad": 0xF04E6, "failed": 0xF0028, "filteredEmpty": 0xF0236 }
var EMPTY_GLYPHS = { "updates": 0xF05E0, "installed": 0xF0431, "search": 0xF0349, "issues": 0xF05E0, "backups": 0xF003C }

// The last `outdated` run (Check now, or the background check), as the Updates view
// qualifies its rows: when it ran, and whether a source did not answer, from the codes
// of its warnings, its failed items and its error.
function reduceCheck(env, outcome) {
  var out = { "at": "", "trouble": false, "failed": 0, "outcome": str(outcome) }
  if (!envelopeOk(env)) {
    out.trouble = outcome !== "ok"
    return out
  }
  var codes = Array.isArray(env.warnings) ? env.warnings.map(function(w) { return w && typeof w === "object" ? str(w.code) : "" }) : []
  if (env.ok && env.data && typeof env.data === "object") {
    out.at = str(env.data.checked_at)
    var targets = Array.isArray(env.data.targets) ? env.data.targets : []
    for (var i = 0; i < targets.length; i++) {
      var items = targets[i] && Array.isArray(targets[i].items) ? targets[i].items : []
      for (var j = 0; j < items.length; j++) if (items[j] && items[j].status === "failed") out.failed++
    }
  } else if (env.error && typeof env.error === "object") {
    codes.push(str(env.error.code))
  }
  out.trouble = out.failed > 0 || codes.some(function(c) { return SOURCE_TROUBLE.indexOf(c) !== -1 })
  return out
}

// The resources the lanes read now or will next (`running`, the local and network lanes'
// requests; `queues`, theirs), for viewState's `refreshing`.
function readingResources(running, queues) {
  var out = []
  var add = function(r) { if (r && typeof r.resource === "string" && out.indexOf(r.resource) === -1) out.push(r.resource) }
  var q = queues && typeof queues === "object" ? queues : {}
  var requests = (Array.isArray(running) ? running : [])
    .concat(Array.isArray(q.local) ? q.local : [], Array.isArray(q.network) ? q.network : [])
  requests.forEach(add)
  return out
}

function stateAction(name) {
  return { "name": name, "text": copy("state.action." + name) }
}

function stateNotice(kind, sentence, action) {
  return { "kind": kind, "sentence": sentence, "action": action || null }
}

// "18:02", local time, from an RFC 3339 instant; "" for one that does not parse.
function clockText(iso) {
  var at = new Date(str(iso))
  if (isNaN(at.getTime())) return ""
  var two = function(n) { return n < 10 ? "0" + n : String(n) }
  return two(at.getHours()) + ":" + two(at.getMinutes())
}

// Whether the service holds the view's data: the search's only for the query and client
// it was asked with.
function viewHas(view, s, query, filter) {
  if (view === "updates" || view === "installed") return !!s.lists
  if (view === "issues") return !!s.errors
  if (view === "backups") return !!s.backups
  if (view === "search") {
    var selection = s.search && s.search.selection && typeof s.search.selection === "object" ? s.search.selection : null
    return !!selection && selection.query === query && selection.target === filter
  }
  return false
}

// When the view's data was read, as an age ("25 min ago"), or "earlier".
function readAge(view, s, nowMs) {
  var at = view === "issues" ? num(s.errorsAt) : view === "backups" ? num(s.backupsAt) : view === "search" ? 0 : num(s.listsAt)
  return at > 0 ? sinceText(new Date(at).toISOString(), nowMs) : copy("state.age.unknown")
}

// The search's skipped sources as notices, one per reason: "GitHub and Tukui couldn't be
// searched: offline."
function skippedNotices(skipped) {
  var list = Array.isArray(skipped) ? skipped : []
  var reasons = []
  var byReason = {}
  for (var i = 0; i < list.length; i++) {
    var entry = list[i] && typeof list[i] === "object" ? list[i] : {}
    var reason = ["offline", "rate_limited", "disabled"].indexOf(entry.reason) !== -1 ? entry.reason : "other"
    if (!byReason.hasOwnProperty(reason)) {
      byReason[reason] = { "sources": [], "resetAt": "" }
      reasons.push(reason)
    }
    var source = str(entry.source)
    var name = COPY.hasOwnProperty("source." + source) ? copy("source." + source) : source
    if (name && byReason[reason].sources.indexOf(name) === -1) byReason[reason].sources.push(name)
    if (!byReason[reason].resetAt && entry.reset_at) byReason[reason].resetAt = str(entry.reset_at)
  }
  return reasons.map(function(reason) {
    var group = byReason[reason]
    var sources = joinNames(group.sources)
    var time = clockText(group.resetAt)
    var id = reason === "rate_limited" && !time ? "state.notice.skipped.rate_limited.untimed" : "state.notice.skipped." + reason
    return stateNotice(reason === "offline" ? "offline" : "partial", copy(id, { "sources": sources, "time": time }))
  })
}

// What `view` shows around its rows (design doc §7). `input` is `{service, filter, rows,
// allRows, query, nowMs}`: the service (or any object with its fields), the client
// filter, how many rows the view shows and how many it would under All, the search's
// query. The answer is `{view, state, glyph, sentence, detail, action, refreshing,
// notices}`, `action` and each notice's `{name, text}` or null.
function viewState(view, input) {
  var i = input && typeof input === "object" ? input : {}
  var s = i.service && typeof i.service === "object" ? i.service : {}
  var filter = str(i.filter)
  var query = str(i.query)
  var nowMs = typeof i.nowMs === "number" ? i.nowMs : Date.now()
  var targets = Array.isArray(s.targets) ? s.targets : []
  var resource = VIEW_RESOURCES.hasOwnProperty(view) ? VIEW_RESOURCES[view] : ""
  var reading = Array.isArray(s.reading) && s.reading.indexOf(resource) !== -1
  var outcome = s.outcomes && typeof s.outcomes === "object" ? s.outcomes[resource] : undefined
  var failed = typeof outcome === "string" && outcome !== "ok" && !reading
  var client = filter ? sheetClient(targets, filter, "") : ""
  // Rows on screen: the view's data, or rows from elsewhere (Issues lists `status`'s
  // problems before Lua errors are read).
  var have = viewHas(view, s, query, filter) || num(i.rows) > 0
  var out = { "view": view, "state": "rows", "glyph": 0, "sentence": "", "detail": "", "action": null,
    "refreshing": have && (reading || (view === "updates" && s.checking === true)), "notices": [] }
  var set = function(state, sentence, action) {
    out.state = state
    out.glyph = STATE_GLYPHS.hasOwnProperty(state) ? STATE_GLYPHS[state] : (EMPTY_GLYPHS[view] || 0)
    out.sentence = sentence
    out.action = action || null
  }
  // A missing client's scope (design doc §2.2, §6.5.2): one card in every view saying
  // what happened, with Rescan clients and, when its own problem offers the plain
  // `targets rm`, Forget; what a view still has of it (Issues lists that problem) stays
  // under the card, and no view says it is empty or up to date.
  var chosen = filter ? targetById(targets, filter) : null
  if (chosen && chosen.missing === true) {
    var actions = [stateAction("rescan")]
    if (forgetAction(chosen)) actions.push({ "name": "forget", "text": copy("state.action.forget", { "client": client }) })
    out.state = "missing"
    out.refreshing = false
    out.notices.push({ "kind": "missing", "sentence": copy("state.missing", { "client": client }),
      "body": copy("state.missing.detail"), "action": null, "actions": actions })
    return out
  }
  if (view === "search" && query === "") {
    set("empty", copy("state.empty.search"))
    return out
  }
  if (!have) {
    if (failed) set("failed", copy("state.failed." + view), stateAction("retry"))
    else set("firstLoad", copy("state.loading." + view))
    return out
  }
  if (failed) {
    out.notices.push(stateNotice("failed", copy("state.notice.failed", { "what": copy("state.what." + view), "age": readAge(view, s, nowMs) }),
      stateAction("retry")))
  }
  var scoped = scopeTargets(targets, filter)
  // A stale check over rows is a notice; an empty view says its age already, and takes
  // Check now as its action instead.
  var checkStale = false
  if (view === "updates" && !out.refreshing) {
    var check = s.lastCheck && typeof s.lastCheck === "object" ? s.lastCheck : null
    var oldest = oldestCheck(targets, filter)
    checkStale = !!oldest && scoped.some(function(t) { return !!t.updates && t.updates.stale === true })
    if (check && check.trouble) {
      out.notices.push(stateNotice("offline", num(check.failed) > 0 ? copy("state.notice.check_failed", { "n": num(check.failed) })
        : copy("state.notice.check_unreachable"), stateAction("check")))
    } else if (checkStale && num(i.rows) > 0) {
      out.notices.push(stateNotice("stale", copy("state.notice.check_stale", { "age": sinceText(oldest, nowMs) }), stateAction("check")))
    }
  }
  if (view === "issues" && !out.refreshing) {
    var errorTargets = s.errors && Array.isArray(s.errors.targets) ? s.errors.targets : []
    if (errorTargets.some(function(t) { return t && t.partial === true && (!filter || t.id === filter) })) {
      out.notices.push(stateNotice("partial", copy("state.notice.errors_partial"), stateAction("read_errors")))
    }
  }
  if (view === "backups" && !out.refreshing && scoped.some(function(t) { return !!t.backups && t.backups.stale === true })) {
    out.notices.push(stateNotice("stale", copy("state.notice.backups_stale"), stateAction("refresh_backups")))
  }
  if (view === "search" && s.search) out.notices = out.notices.concat(skippedNotices(s.search.skipped))
  if (num(i.rows) > 0) return out
  var filtered = filter !== "" && num(i.allRows) > 0
  if (view === "updates") {
    if (!filter && targets.length === 0) set("empty", "")
    else if (!oldestCheck(targets, filter)) set("empty", copy("state.empty.updates.unchecked"), stateAction("check"))
    else set(filtered ? "filteredEmpty" : "empty", copy(filter ? "state.empty.updates.client" : "state.empty.updates",
      { "client": client, "age": sinceText(oldestCheck(targets, filter), nowMs) }), checkStale ? stateAction("check") : null)
  } else if (view === "installed") {
    set(filtered ? "filteredEmpty" : "empty", copy(filter ? "state.empty.installed.client" : "state.empty.installed", { "client": client }),
      stateAction("search"))
  } else if (view === "search") {
    set("empty", copy("state.empty.search.query", { "query": query }))
  } else if (view === "issues") {
    set(filtered ? "filteredEmpty" : "empty", copy(filter ? "state.empty.issues.client" : "state.empty.issues", { "client": client }))
  } else if (view === "backups") {
    set(filtered ? "filteredEmpty" : "empty", copy(filter ? "state.empty.backups.client" : "state.empty.backups", { "client": client }),
      stateAction("backup"))
  }
  return out
}

// The Addons view's orders (`S` cycles them, 1n2p.22): by name, recently updated (the
// newest install first), by source. Problems and waiting updates come first in each.
var INSTALLED_SORTS = ["name", "recent", "source"]

// The sort after `sort`, in a ring.
function nextSort(sort) {
  var at = INSTALLED_SORTS.indexOf(str(sort))
  return INSTALLED_SORTS[(at + 1) % INSTALLED_SORTS.length]
}

// A source kind as a player word (design doc §6.3).
var SOURCE_WORDS = { "gh": "GitHub", "wowi": "WoWInterface", "tukui": "Tukui", "wowace": "WowAce", "git": "git", "file": "a file" }

// Whether `query` matches: each of its words is a subsequence, ignoring case, of one of
// `fields` ("plt" matches Plater, "chg disk" a row whose files changed on disk). An
// empty query matches everything.
function filterMatches(query, fields) {
  var terms = str(query).toLowerCase().split(/\s+/).filter(function(w) { return w !== "" })
  var texts = (Array.isArray(fields) ? fields : []).map(function(f) { return str(f).toLowerCase() })
  var subsequence = function(term, text) {
    var at = 0
    for (var i = 0; i < text.length && at < term.length; i++) if (text.charAt(i) === term.charAt(at)) at++
    return at === term.length
  }
  return terms.every(function(term) { return texts.some(function(text) { return subsequence(term, text) }) })
}

// One addon's row in the Addons view (design doc §2.3, §5.3): its glyph and band, its
// version (installed → the waiting one), chips for its state at a glance, the most
// urgent first, a meta line of its source and what the chips cannot say (how many
// files changed, what needs it), and the actions the cursored row offers.
function installedRow(t, a, client) {
  var problem = a.drift === "modified" || a.drift === "missing"
  var markers = []
  if (a.dependency) markers.push("dep")
  if (a.orphan) markers.push("orphan")
  if (a.pinned) markers.push("pinned")
  if (problem) markers.push(a.drift)
  var chips = []
  var meta = [SOURCE_WORDS[a.source] || copy("installed.source.unknown")]
  if (a.drift === "modified") {
    chips.push({ "text": copy("installed.chip.modified"), "urgent": true })
    meta.push(a.driftFiles > 0 ? copy("installed.meta.modified_files", { "n": a.driftFiles }) : copy("installed.meta.modified"))
  } else if (a.drift === "missing") {
    chips.push({ "text": copy("installed.chip.missing"), "urgent": true })
    meta.push(copy("installed.meta.missing"))
  } else if (a.drift === "unindexed") {
    chips.push({ "text": copy("installed.chip.unindexed"), "urgent": false })
  }
  if (a.updateAvailable) chips.push({ "text": copy("installed.chip.update"), "urgent": false })
  if (a.pinned) chips.push({ "text": copy("installed.chip.pinned"), "urgent": false })
  if (a.orphan) {
    chips.push({ "text": copy("installed.chip.orphan"), "urgent": false })
    meta.push(copy("installed.meta.orphan"))
  } else if (a.dependency) {
    chips.push({ "text": copy("installed.chip.dependency"), "urgent": false })
    if (a.requiredBy.length) meta.push(copy("installed.meta.needed_by", { "addons": joinNames(a.requiredBy) }))
  }
  if (a.anyFlavor) chips.push({ "text": copy("installed.chip.any_flavor"), "urgent": false })
  var primary = problem ? "reinstall" : (a.updateAvailable ? "update" : "")
  // The cursored row's buttons: a problem's Repair first (design doc §2.3).
  var actions = []
  if (problem) actions.push({ "name": "enter", "glyph": String.fromCodePoint(0xF0DC9), "tooltip": withKey(copy("installed.action.repair"), "enter", "installed") })
  if (a.updateAvailable) actions.push({ "name": "update", "glyph": String.fromCodePoint(0xF0CE1), "tooltip": withKey(copy("installed.action.update"), "update", "installed") })
  actions.push({ "name": "pin", "glyph": String.fromCodePoint(a.pinned ? 0xF0404 : 0xF0403),
    "tooltip": withKey(copy(a.pinned ? "installed.action.unpin" : "installed.action.pin"), "pin", "installed") })
  actions.push({ "name": "restore", "glyph": String.fromCodePoint(0xF006F), "tooltip": withKey(copy("installed.action.restore"), "restore", "installed") })
  actions.push({ "name": "remove", "glyph": String.fromCodePoint(0xF01B4), "tooltip": withKey(copy("installed.action.remove"), "remove", "installed") })
  return {
    "key": t.id + "/" + a.name, "target": t.id, "client": client, "name": a.name, "version": a.version, "source": a.source,
    "sourceWord": meta[0], "markers": markers, "chips": chips, "meta": meta.join(" · "), "problem": problem,
    "band": problem ? "urgent" : (a.updateAvailable ? "accent" : ""),
    "glyph": a.drift === "modified" ? 0xF0DC9 : a.drift === "missing" ? 0xF0258 : a.updateAvailable ? 0xF0CE1
      : a.pinned ? 0xF0403 : (a.dependency || a.orphan) ? 0xF0431 : 0,
    "pinned": a.pinned, "updateAvailable": a.updateAvailable === true, "primary": primary, "actions": actions,
    "depth": 0, "group": "", "groupCount": "", "installedAt": a.installedAt, "requiredBy": a.requiredBy,
    "dependency": a.dependency, "targetKey": t.key, "latest": a.latest, "channel": a.channel,
    "latestSource": a.latestSource, "latestReleaseId": a.latestReleaseId, "latestAsset": a.latestAsset
  }
}

// The Addons view's rows, in the filter (1n2p.22, design doc §2.3): each client's
// managed addons, problems first (files changed, folder gone), then the ones with an
// update waiting, then the rest, each part in `view.sort`'s order (by name, the
// newest install first, or by source); a dependency with no problem of its own sits
// right under the first addon here that needs it (`depth` 1), once. `view.query`
// keeps the rows it matches (filterMatches over the name, the source and the state
// words), flat where a dependency's requester did not match. Under All, each client's
// first row carries its header (`group`, `groupCount`), named from `statusTargets`
// (`status`'s clients; the id without them). Enter on a row repairs a problem, else
// updates, else does nothing (`primary`), until the addon page (.23) opens instead.
function installedRows(lists, filter, statusTargets, view) {
  var options = view && typeof view === "object" ? view : {}
  var sort = INSTALLED_SORTS.indexOf(str(options.sort)) >= 0 ? str(options.sort) : "name"
  var query = str(options.query).trim()
  var byName = function(x, y) {
    var a = x.name.toLowerCase(), b = y.name.toLowerCase()
    return a < b ? -1 : a > b ? 1 : 0
  }
  var order = function(x, y) {
    var rank = function(r) { return r.problem ? 0 : (r.updateAvailable ? 1 : 2) }
    var ranked = rank(x) - rank(y)
    if (ranked) return ranked
    if (sort === "recent" && x.installedAt !== y.installedAt) return x.installedAt > y.installedAt ? -1 : 1
    if (sort === "source" && x.sourceWord !== y.sourceWord) return x.sourceWord < y.sourceWord ? -1 : 1
    return byName(x, y)
  }
  var out = []
  var targets = listTargets(lists, filter)
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    var client = sheetClient(statusTargets, t.id, "")
    var rows = stableSort(t.addons.map(function(a) { return installedRow(t, a, client) }), order)
    // A dependency without a problem nests under the first addon here that needs it.
    var names = rows.map(function(r) { return r.name })
    var parentOf = function(r) {
      if (!r.dependency || r.problem) return ""
      for (var k = 0; k < r.requiredBy.length; k++) {
        var p = r.requiredBy[k]
        if (p !== r.name && names.indexOf(p) >= 0) return p
      }
      return ""
    }
    var top = rows.filter(function(r) { return parentOf(r) === "" })
    var nested = []
    for (var j = 0; j < top.length; j++) {
      nested.push(top[j])
      for (var c = 0; c < rows.length; c++) {
        if (parentOf(rows[c]) !== top[j].name) continue
        rows[c].depth = 1
        nested.push(rows[c])
      }
    }
    // A child whose parent is itself nested (a chain) stays where it sorted, flat.
    for (var m = 0; m < rows.length; m++) if (nested.indexOf(rows[m]) < 0) nested.push(rows[m])
    var shown = query === "" ? nested : nested.filter(function(r) {
      return filterMatches(query, [r.name, r.sourceWord, r.meta].concat(r.chips.map(function(ch) { return ch.text })))
    })
    for (var s = 0; s < shown.length; s++) {
      if (shown[s].depth === 1 && shown.filter(function(r) { return r.name === parentOf(shown[s]) }).length === 0) shown[s].depth = 0
    }
    if (!filter && shown.length > 0) {
      var group = clientGroup(statusTargets, t.id, plural(shown.length, "addon"))
      shown[0].group = group.title
      shown[0].groupCount = group.count
    }
    out = out.concat(shown)
  }
  return out
}

// How many addons the Addons view shows of how many in the filter: the filter box's
// "2 of 42", urgent at none.
function installedCount(lists, filter, query) {
  var total = 0
  var targets = listTargets(lists, filter)
  for (var i = 0; i < targets.length; i++) total += targets[i].addons.length
  var shown = str(query).trim() === "" ? total : installedRows(lists, filter, [], { "query": query }).length
  return { "shown": shown, "total": total, "text": copy("installed.count", { "shown": shown, "total": total }), "none": total > 0 && shown === 0 }
}

// The folders omaddons does not manage, one row per client in the filter (design doc
// §2.3, UX-13): "3 folders omaddons doesn't manage", never a command line; their Take
// over… leads to the take-over flow (first run, .32).
function unmanagedRows(lists, filter, statusTargets) {
  return listTargets(lists, filter)
    .filter(function(t) { return t.unmanaged > 0 })
    .map(function(t) {
      return { "target": t.id, "client": sheetClient(statusTargets, t.id, ""), "count": t.unmanaged,
        "text": filter ? copy("installed.unmanaged", { "n": t.unmanaged })
          : copy("installed.unmanaged.client", { "n": t.unmanaged, "client": sheetClient(statusTargets, t.id, "") }) }
    })
}

// A row's action, as a template run: Enter's, and `p`'s pin or unpin.
function rowAction(row, which) {
  var slots = { "target": row.target, "addon": row.name }
  if (which === "pin") return row.pinned ? { "template": "unpin", "slots": slots, "label": "Unpin " + row.name }
    : { "template": "pin", "slots": slots, "label": "Pin " + row.name }
  if (which === "update") return { "template": "update_addon", "slots": slots, "label": "Update " + row.name, "shown": [shownOf(row)] }
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
    problems.push(Object.assign({
      "kind": "problem", "key": "p//" + n, "target": "", "client": "", "severity": mi.severity,
      "urgent": mi.urgent, "title": mi.title, "autoFixable": mi.autoFixable === true
    }, problemRow(mi, "")))
  }
  var targets = Array.isArray(statusTargets) ? statusTargets : []
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    if (filter && t.id !== filter) continue
    for (var j = 0; j < t.issues.length; j++) {
      var issue = t.issues[j]
      problems.push(Object.assign({
        "kind": "problem", "key": "p/" + t.id + "/" + j, "target": t.id, "client": t.displayName || t.id,
        "severity": issue.severity, "urgent": issue.urgent, "title": issue.title, "autoFixable": issue.autoFixable === true
      }, problemRow(issue, t.id)))
    }
  }
  // Within a severity, the order `status` ranked them in (the machine's first).
  problems = stableSort(problems, function(a, b) { return SEVERITY_ORDER.indexOf(a.severity) - SEVERITY_ORDER.indexOf(b.severity) })
  if (problems.length) sections.push({ "title": "Problems", "lines": [], "rows": problems })

  var errTargets = errors && Array.isArray(errors.targets) ? errors.targets : []
  for (var k = 0; k < errTargets.length; k++) {
    var e = errTargets[k]
    if (filter && e.id !== filter) continue
    var rows = []
    var lines = []
    var client = sheetClient(targets, e.id, "")
    if (e.capture === "none") {
      rows.push({
        "kind": "capture", "key": "c/" + e.id, "target": e.id, "client": client,
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
    sections.push({ "title": copy("issues.errors_title", { "client": client }), "target": e.id, "lines": lines, "rows": rows })
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
  targets = stableSort(targets, function(a, b) { return a.id < b.id ? -1 : a.id > b.id ? 1 : 0 })
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
    // Titled with the client's player name, whether it runs (design doc §2.2).
    var group = clientGroup(known, t.id, "")
    sections.push({ "title": group.title, "count": group.count, "target": t.id, "lines": lines, "rows": rows })
  }
  return sections
}

// `b`: the filtered client, or every client (no confirmation: creation is undoable and
// expected, M3-D9). The label names the client as `targets` (`status`'s) does.
function backupRequest(filter, targets) {
  if (filter) return { "template": "backup_target", "slots": { "target": filter }, "label": "Back up " + sheetClient(targets, filter, "") }
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
// The Undo a result offers. A repair's is the bound Undo last (`undo --run <run>`, its
// dry run's stored plan, then `apply`), never the fix envelope's own `doctor undo` hint,
// and only once a `status` read after the repair names it as the last change
// (`statusSeen`, set with `repair` by the view that ran it).
function resultUndo(result, lastChange) {
  if (!result) return null
  if (result.repair === true) {
    var undo = lastChange && !lastChange.unknown && lastChange.kind === "repair" ? lastChange.undo : null
    if (result.statusSeen !== true || !undo || !undo.runnable || !Array.isArray(undo.argv)) return null
    return { "action": "undo_run", "command": undo.command, "argv": undo.argv, "runnable": true }
  }
  if (!Array.isArray(result.commands)) return null
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

// -------------------------------------------------------------- the plan runner (G1)

// A storable command's preview stores a plan (M4-D22, M4-D28): the widget compares its
// typed approval with what the view showed, then applies the plan as offered, asks
// first, refreshes, or runs nothing and says why. These functions decide; the service
// only carries out the decision. None reads a warning's text.

// The paths whose dry runs store a plan (M4-D22), held equal to app::COMMANDS'
// stores_plan by tests/omarchy_widget.rs. A previewed action on one of them confirms
// through its plan.
// BEGIN PLAN_PATHS
var PLAN_PATHS = ["install", "update", "pin", "unpin", "adopt", "sync", "copy", "remove", "undo", "backup restore", "backup prune"]
// END PLAN_PATHS
var PLAN_ID_PATTERN = /^[0-9A-HJKMNP-TV-Z]{26}$/
// The fields of an approval's selection item, as the widget keeps them.
var APPROVAL_FIELDS = ["target_key", "target_id", "addon", "source", "channel", "release_id", "asset", "from_version", "to_version", "op"]
// The fields a shown item and a plan item must both carry, and agree on, to match.
var MATCH_FIELDS = ["target_key", "addon", "source", "channel", "release_id", "asset", "from_version", "to_version"]

function storesPlan(path) {
  return PLAN_PATHS.indexOf(path) !== -1
}

// A non-empty string, else null.
function nonEmpty(value) {
  return typeof value === "string" && value !== "" ? value : null
}

// Lower case for ASCII letters only, as the CLI matches addon names.
function asciiLower(text) {
  return String(text).replace(/[A-Z]/g, function(c) { return c.toLowerCase() })
}

// A dry run's typed approval summary (`data.approval`), fail-closed: null unless it has
// a boolean `routine`, `reasons` of strings and `selection` of objects. A summary that
// says routine while naming a reason is not routine.
function approvalOf(raw) {
  if (!raw || typeof raw !== "object" || Array.isArray(raw) || typeof raw.routine !== "boolean") return null
  if (!Array.isArray(raw.reasons) || !Array.isArray(raw.selection)) return null
  for (var i = 0; i < raw.reasons.length; i++) if (typeof raw.reasons[i] !== "string") return null
  var items = []
  for (var j = 0; j < raw.selection.length; j++) {
    var item = raw.selection[j]
    if (!item || typeof item !== "object" || Array.isArray(item)) return null
    var kept = {}
    for (var k = 0; k < APPROVAL_FIELDS.length; k++) kept[APPROVAL_FIELDS[k]] = nonEmpty(item[APPROVAL_FIELDS[k]])
    items.push(kept)
  }
  var scope = raw.scope && typeof raw.scope === "object" && !Array.isArray(raw.scope) ? raw.scope : {}
  return { "routine": raw.routine === true && raw.reasons.length === 0, "reasons": raw.reasons.slice(), "selection": items, "scope": scope }
}

// The plan a preview stored, or null: `data.plan_id` a ULID, exactly one offer in
// `commands[]` to run it whose argv is exactly that plan's, and an approval that reads.
// `argv` is the only one the plan may run (classifyCliArgv).
function planFrom(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || typeof env.data !== "object") return null
  var id = env.data.plan_id
  if (typeof id !== "string" || !PLAN_ID_PATTERN.test(id)) return null
  var offers = Array.isArray(env.commands) ? env.commands.filter(function(c) { return !!c && c.action === "apply" }) : []
  if (offers.length !== 1) return null
  var argv = offers[0].argv
  if (!Array.isArray(argv) || argv.length !== 3 || argv[0] !== "omaddons" || argv[1] !== "apply" || argv[2] !== id) return null
  var approval = approvalOf(env.data.approval)
  if (!approval) return null
  return { "planId": id, "argv": argv.slice(), "destructive": offers[0].destructive === true, "expiresAt": str(env.data.plan_expires_at), "approval": approval }
}

// What an Updates or Installed row showed, in the approval's field names.
function shownOf(row) {
  var r = row && typeof row === "object" ? row : {}
  return {
    "target_key": nonEmpty(r.targetKey), "target_id": nonEmpty(r.target), "addon": nonEmpty(r.name),
    "source": nonEmpty(r.latestSource), "channel": nonEmpty(r.channel), "release_id": nonEmpty(r.latestReleaseId),
    "asset": nonEmpty(r.latestAsset), "from_version": nonEmpty(r.version), "to_version": nonEmpty(r.latest)
  }
}

// What the view showed of `addon` on `target` (its Installed row, the name ignoring
// ASCII case), else an item naming only the two.
function shownForAddon(lists, target, addon) {
  var wanted = asciiLower(str(addon))
  var rows = target ? installedRows(lists, target) : []
  for (var i = 0; i < rows.length; i++) if (asciiLower(str(rows[i].name)) === wanted) return [shownOf(rows[i])]
  return [shownOf({ "target": target, "name": addon })]
}

// The name a scope list's entry gives: the entry itself, or its `addon` or `name`.
function entryName(entry) {
  if (typeof entry === "string") return entry
  return entry && typeof entry === "object" ? str(entry.addon) || str(entry.name) : ""
}

// The plan items the user asked for: the selection without the dependencies it adds,
// which the dialog names as an effect instead (M4-D28).
function requestedItems(approval) {
  var selection = approval && Array.isArray(approval.selection) ? approval.selection : []
  var scope = approval && approval.scope && typeof approval.scope === "object" ? approval.scope : {}
  var added = (Array.isArray(scope.dependencies_added) ? scope.dependencies_added : []).map(function(d) { return asciiLower(entryName(d)) })
  return selection.filter(function(item) {
    return !(item && typeof item.addon === "string" && added.indexOf(asciiLower(item.addon)) !== -1)
  })
}

// Where an item pairs: its client key and addon (ignoring ASCII case), or null when it
// lacks either.
function pairKey(item) {
  if (!item || typeof item !== "object" || !nonEmpty(item.target_key) || !nonEmpty(item.addon)) return null
  return item.target_key + "\n" + asciiLower(item.addon)
}

function sameField(field, a, b) {
  return field === "addon" ? asciiLower(a) === asciiLower(b) : a === b
}

// Whether the plan's items are exactly what the view showed: each shown item paired
// with the plan item of the same client key and addon, one for one, every MATCH_FIELDS
// entry present on both sides and equal. Missing evidence is never equality.
function selectionMatches(shown, selection) {
  if (!Array.isArray(shown) || !Array.isArray(selection) || shown.length === 0 || shown.length !== selection.length) return false
  var keys = function(list) {
    var out = []
    for (var i = 0; i < list.length; i++) {
      var key = pairKey(list[i])
      if (key === null || out.indexOf(key) !== -1) return null
      out.push(key)
    }
    return out
  }
  var shownKeys = keys(shown)
  var planKeys = keys(selection)
  if (!shownKeys || !planKeys) return false
  for (var i = 0; i < shown.length; i++) {
    var partner = selection[planKeys.indexOf(shownKeys[i])]
    if (!partner) return false
    for (var f = 0; f < MATCH_FIELDS.length; f++) {
      var a = nonEmpty(shown[i][MATCH_FIELDS[f]])
      var b = nonEmpty(partner[MATCH_FIELDS[f]])
      if (a === null || b === null || !sameField(MATCH_FIELDS[f], a, b)) return false
    }
  }
  return true
}

// -------------------------------------------------------------- the plan sheet (1n2p.17)
//
// Every confirmation is a plan sheet (design doc §6.7): a title that names the change,
// the "Changed since you looked" banner, every item (no cap: the sheet scrolls), one line
// per effect, and Cancel beside the verb. These functions build it from the preview's
// envelope; PlanSheet.qml draws it, and dialogText writes it as text for IPC's state().
// A sheet is `{title, stale, items: [{op, addon, client, clientKey, from, to, name,
// detail, text, header}], effects: [{kind, severity, glyph, text}], confirmLabel,
// cancelLabel, destructive, glyph}`.

// The widget's words (design doc §6.2): ids to templates with named {slots}, and a
// {one, other} pair chosen by the `n` slot. copy() is the one formatter. The plan
// sheet's words start the table; the other views move theirs here as they are rebuilt.
var COPY = {
  "sheet.cancel": "Cancel",
  "sheet.date.unknown": "an earlier time",
  "sheet.title.generic": "Go ahead with this change?",
  "sheet.confirm.generic": "Continue",
  "sheet.title.failed": "omaddons couldn't say what this would do",

  "sheet.title.update.one": "Update {addon} on {client}?",
  "sheet.title.update.client": "Update {n} addons on {client}?",
  "sheet.title.update.clients": "Update {n} addons on {m} clients?",
  "sheet.confirm.update.one": "Update {addon}",
  "sheet.confirm.update.many": "Update {n}",
  "sheet.title.install.one": "Install {addon} on {client}?",
  "sheet.title.install.client": "Install {n} addons on {client}?",
  "sheet.title.install.clients": "Install {n} addons on {m} clients?",
  "sheet.confirm.install.one": "Install {addon}",
  "sheet.confirm.install.many": "Install {n}",
  "sheet.title.reinstall.one": "Repair {addon} on {client}?",
  "sheet.title.reinstall.client": "Repair {n} addons on {client}?",
  "sheet.title.reinstall.clients": "Repair {n} addons on {m} clients?",
  "sheet.confirm.reinstall.one": "Repair {addon}",
  "sheet.confirm.reinstall.many": "Repair {n}",
  "sheet.title.remove.one": "Remove {addon} from {client}?",
  "sheet.title.remove.client": "Remove {n} addons from {client}?",
  "sheet.title.remove.clients": "Remove {n} addons from {m} clients?",
  "sheet.confirm.remove.one": "Remove {addon}",
  "sheet.confirm.remove.many": "Remove {n}",
  "sheet.title.adopt.one": "Take over {addon} on {client}?",
  "sheet.title.adopt.client": "Take over {n} addons on {client}?",
  "sheet.title.adopt.clients": "Take over {n} addons on {m} clients?",
  "sheet.confirm.adopt.one": "Take over {addon}",
  "sheet.confirm.adopt.many": "Take over {n}",
  "sheet.title.pin.one": "Pin {addon} on {client}?",
  "sheet.title.pin.client": "Pin {n} addons on {client}?",
  "sheet.title.pin.clients": "Pin {n} addons on {m} clients?",
  "sheet.confirm.pin.one": "Pin {addon}",
  "sheet.confirm.pin.many": "Pin {n}",
  "sheet.title.unpin.one": "Unpin {addon} on {client}?",
  "sheet.title.unpin.client": "Unpin {n} addons on {client}?",
  "sheet.title.unpin.clients": "Unpin {n} addons on {m} clients?",
  "sheet.confirm.unpin.one": "Unpin {addon}",
  "sheet.confirm.unpin.many": "Unpin {n}",
  "sheet.title.change.one": "Change {addon} on {client}?",
  "sheet.title.change.client": "Make {n} changes on {client}?",
  "sheet.title.change.clients": "Make {n} changes on {m} clients?",
  "sheet.confirm.change.one": "Change {addon}",
  "sheet.confirm.change.many": "Change {n}",

  "sheet.item.install": "{version} (new)",
  "sheet.item.remove": { "one": "{version} (1 folder)", "other": "{version} ({n} folders)" },
  "sheet.item.edit": "changes its {fields}",

  "sheet.title.undo": "Undo the {change} on {clients} from {date}?",
  "sheet.title.undo.nowhere": "Undo the {change} from {date}?",
  "sheet.confirm.undo": "Undo",
  "sheet.confirm.redo": "Redo",
  "sheet.change.install": "install",
  "sheet.change.update": "update",
  "sheet.change.remove": "removal",
  "sheet.change.undo": "undo",
  "sheet.change.restore": "restore",
  "sheet.change.repair": "repair",
  "sheet.change.other": "change",
  "sheet.item.undo.restored": "{folder} back as it was",
  "sheet.item.undo.removed": "{folder} removed",

  "sheet.title.restore": "Restore all of {owner} settings from {date}?",
  "sheet.confirm.restore": "Restore all settings",
  "sheet.title.restore_addon": "Restore {owner} settings on {client} from {date}?",
  "sheet.confirm.restore_addon": "Restore {owner} settings",
  "sheet.item.restore": "{files} back, {differed} differ now, {created} added since (moved aside)",
  "sheet.item.restore_addon": "{replaced} replaced, {restored} back, {created} added since (moved aside)",

  "sheet.title.prune.one": "Delete a backup on {client}?",
  "sheet.title.prune.client": "Delete {n} backups on {client}?",
  "sheet.title.prune.clients": "Delete {n} backups on {m} clients?",
  "sheet.confirm.prune.one": "Delete",
  "sheet.confirm.prune.many": "Delete {n}",
  "sheet.item.prune": "{kind}, {size}",
  "backup.kind.manual": "made by you",
  "backup.kind.auto": "automatic",
  "backup.kind.before_restore": "before a restore",
  "backup.kind.imported": "imported",
  "backup.kind.recovered": "rescued from an interrupted restore",

  "sheet.title.forget": "Forget {client}?",
  "sheet.confirm.forget": "Forget {client}",

  "sheet.title.repair": "Repair: {problem}?",
  "sheet.title.repair.unnamed": "Repair this problem?",
  "sheet.confirm.repair": "Repair",

  "sheet.effect.dependency_added": "Also installs {name}, which {addon} needs.",
  "sheet.effect.dependency_added.any": "Also installs {name}, a new dependency.",
  "sheet.effect.dependency_added.unnamed": "Also installs a new dependency.",
  "sheet.effect.folders_removed": "Removes the folder {folder}. Undo brings it back.",
  "sheet.effect.folders_removed.unnamed": "Removes folders. Undo brings them back.",
  "sheet.effect.local_changes_replaced": {
    "one": "Replaces 1 file you changed in {folders}. Undo brings it back.",
    "other": "Replaces {n} files you changed in {folders}. Undo brings them back."
  },
  "sheet.effect.local_changes_replaced.nowhere": {
    "one": "Replaces 1 file you changed. Undo brings it back.",
    "other": "Replaces {n} files you changed. Undo brings them back."
  },
  "sheet.effect.local_changes_replaced.unnamed": "Replaces files you changed. Undo brings them back.",
  "sheet.effect.source_changed": "{addon} will come from {source} now.",
  "sheet.effect.source_changed.unknown": "{addon} will come from another source now.",
  "sheet.effect.source_changed.unnamed": "An addon here will come from another source now.",
  "sheet.effect.channel_changed": "{addon} switches to {channel} releases.",
  "sheet.effect.channel_changed.unnamed": "An addon here switches to another release channel.",
  "sheet.effect.while_running.reload": { "one": "{clients} is running: then /reload in game.", "other": "{clients} are running: then /reload in game." },
  "sheet.effect.while_running.restart": { "one": "{clients} is running: then restart the game.", "other": "{clients} are running: then restart the game." },
  "sheet.effect.while_running.unnamed": "The game is running: then /reload in game.",
  "sheet.effect.reinstall": "Puts back {owner} files exactly as released.",
  "sheet.effect.reinstall.unnamed": "Puts back the files exactly as released.",
  "sheet.effect.force": "Takes over folders another addon owns.",
  "sheet.effect.downgrade": "Goes back to an older version ({from} → {to}).",
  "sheet.effect.downgrade.unnamed": "Goes back to an older version.",
  "sheet.effect.unknown": "Also: {words}.",
  "sheet.effect.still_required": "Other addons still need {addon} and won't load without it.",
  "sheet.effect.still_required.unnamed": "Other addons still need what this removes and won't load without it.",
  "sheet.effect.reload": "Then /reload in game.",
  "sheet.effect.restart": "Then restart the game.",
  "sheet.effect.changed_since": {
    "one": "Also undoes every play session since the restore: 1 file the game changed (undo data keeps it).",
    "other": "Also undoes every play session since the restore: {n} files the game changed (undo data keeps them)."
  },
  "sheet.effect.changed_since.uncounted": "Also undoes every play session since the restore (the files it changed can't be counted).",
  "sheet.effect.restore_scope": "Also restores keybindings, macros, chat and every other addon's settings.",
  "sheet.effect.kept_backup": "Your current settings are kept as a backup first.",
  "sheet.effect.before_restore_skipped": "Today's settings won't be backed up first (this drive can't share files); undo still brings them back.",
  "sheet.effect.verified": "Checked against the backup's checksums.",
  "sheet.effect.settings_version": "Settings written by {addon} {captured}; installed now: {installed}.",
  "sheet.effect.not_in_backup": "{folder} isn't in this backup and stays as it is.",
  "sheet.effect.busy": "{id} is in use and is kept.",
  "sheet.effect.forget_untouched": "Nothing in its folder is touched.",
  "sheet.effect.forget_record": "omaddons keeps its record of {addons}.",
  "sheet.effect.delegated": "Also finishes the interrupted change {tx} ({direction}).",
  "sheet.effect.keeps_copy": "Keeps a copy of {size} first, for undo.",
  "sheet.effect.undo_last": "Undo last reverts it.",

  "state.loading.updates": "Reading your addons…",
  "state.loading.installed": "Reading your addons…",
  "state.loading.search": "Searching…",
  "state.loading.issues": "Checking…",
  "state.loading.backups": "Reading backups…",
  "state.failed.updates": "Couldn't read your addons.",
  "state.failed.installed": "Couldn't read your addons.",
  "state.failed.search": "The search didn't finish.",
  "state.failed.issues": "Couldn't read the problems and Lua errors.",
  "state.failed.backups": "Couldn't read your backups.",
  "state.empty.updates": "All up to date. Checked {age}.",
  "state.empty.updates.client": "{client} is up to date. Checked {age}.",
  "state.empty.updates.unchecked": "Not checked for updates yet.",
  "state.empty.installed": "No addons yet. Find some in Search.",
  "state.empty.installed.client": "No addons on {client} yet. Find some in Search.",
  "state.empty.search": "Type a name, or paste a link from GitHub, WoWInterface, Tukui or WowAce.",
  "state.empty.search.query": "Nothing found for “{query}”.",
  "state.empty.issues": "Nothing needs fixing.",
  "state.empty.issues.client": "Nothing needs fixing on {client}.",
  "state.empty.backups": "No backups yet.",
  "state.empty.backups.client": "No backups of {client} yet.",
  "state.notice.failed": "Couldn't read {what} again; showing what was read {age}.",
  "state.notice.check_unreachable": "Some sources didn't answer the last check; what they said before is shown.",
  "state.notice.check_failed": {
    "one": "1 addon couldn't be checked: its source didn't answer.",
    "other": "{n} addons couldn't be checked: their sources didn't answer."
  },
  "state.notice.check_stale": "Last checked {age}.",
  "state.notice.errors_partial": "Some errors couldn't be read yet: the game was writing. Read again after you quit.",
  "state.notice.backups_stale": "Backup details are out of date.",
  "state.notice.skipped.offline": "{sources} couldn't be searched: offline.",
  "state.notice.skipped.rate_limited": "{sources} couldn't be searched: it asked to wait until {time}.",
  "state.notice.skipped.rate_limited.untimed": "{sources} couldn't be searched: it asked to wait.",
  "state.notice.skipped.disabled": "{sources} couldn't be searched: turned off.",
  "state.notice.skipped.other": "{sources} couldn't be searched.",
  "state.what.updates": "your addons",
  "state.what.installed": "your addons",
  "state.what.search": "the search",
  "state.what.issues": "the problems",
  "state.what.backups": "your backups",
  "state.age.unknown": "earlier",
  "state.action.retry": "Try again",
  "state.action.check": "Check now",
  "state.action.search": "Search",
  "state.action.backup": "Back up now",
  "state.action.read_errors": "Read again",
  "state.action.refresh_backups": "Refresh",
  "state.refreshing": "Updating…",
  "state.missing": "{client} isn't on this computer any more",
  "state.missing.detail": "Its folder is gone, or the game was uninstalled.",
  "state.action.rescan": "Rescan clients",
  "state.action.forget": "Forget {client}…",

  // The Addons view (1n2p.22, design doc §2.3, §6.3).
  "installed.source.unknown": "an unknown source",
  "installed.chip.modified": "changed on disk",
  "installed.chip.missing": "folder gone",
  "installed.chip.unindexed": "can't check files",
  "installed.chip.update": "update ready",
  "installed.chip.pinned": "pinned",
  "installed.chip.orphan": "not needed",
  "installed.chip.dependency": "dependency",
  "installed.chip.any_flavor": "not made for this client",
  "installed.meta.modified_files": { "one": "1 file changed on disk", "other": "{n} files changed on disk" },
  "installed.meta.modified": "files changed on disk",
  "installed.meta.missing": "its folder is gone",
  "installed.meta.orphan": "nothing needs it any more",
  "installed.meta.needed_by": "needed by {addons}",
  "installed.action.repair": "Repair",
  "installed.action.update": "Update",
  "installed.action.pin": "Pin",
  "installed.action.unpin": "Unpin",
  "installed.action.restore": "Restore its settings",
  "installed.action.remove": "Remove",
  "installed.count": "{shown} of {total}",
  "installed.filter": "Filter by name, source or state",
  "installed.sort.name": "Sorted by name",
  "installed.sort.recent": "Sorted by recently updated",
  "installed.sort.source": "Sorted by source",
  "installed.unmanaged": { "one": "1 folder omaddons doesn't manage", "other": "{n} folders omaddons doesn't manage" },
  "installed.unmanaged.client": { "one": "1 folder on {client} omaddons doesn't manage", "other": "{n} folders on {client} omaddons doesn't manage" },
  "installed.take_over": "Take over…",

  // The clients (design doc §2.1, §2.2): the scope's words, and a group's header.
  "client.all": "All clients",
  "client.running": "running",
  "client.running_unknown": "can't tell if running",
  "client.not_found": "not found",
  "client.choose": "Choose a client first: t, or a row of one",
  "issues.errors_title": "Lua errors on {client}",

  "source.catalog": "omaddons' catalog",
  "source.github": "GitHub",
  "source.wowi": "WoWInterface",
  "source.tukui": "Tukui",
  "source.wowace": "WowAce",
  "source.git": "a git repository",
  "source.file": "a file",
  "channel.stable": "stable",
  "channel.beta": "beta",
  "channel.alpha": "alpha",

  "stale.banner": "Changed since you looked: {what}. Nothing ran; here is what would happen now.",
  "stale.banner.there": "Changed since you looked: {what}. Nothing ran there; here is what would happen now.",
  "stale.expired": "that was more than a day ago",
  "stale.gone": "what you saw was cleared",
  "stale.binary_changed": "omaddons was updated since",
  "stale.input_missing": "the file it installs from is gone",
  "stale.already_applied": "it already ran",
  "stale.landed": "another change landed on {clients}",
  "stale.changed.targets": "another change landed on a client",
  "stale.changed.selection": "a newer version came out, or the change is different now",
  "stale.changed.observed": "files it would change were changed",
  "stale.changed.command": "the command is different",
  "stale.changed.unknown": "something it depends on",
  "stale.repair.targets": "the clients it touches",
  "stale.repair.selection": "what it would do",
  "stale.repair.observed": "the files this repair checks",
  "stale.repair.command": "the command",
  "stale.repair.unknown": "the repair"
}

// COPY[id] with its {slots} filled; a pair by `slots.n` (1 is "one"). A missing slot is
// empty, and an unknown id shows as itself, so a typo is seen, never thrown.
function copy(id, slots) {
  if (!COPY.hasOwnProperty(id)) return String(id)
  var s = slots && typeof slots === "object" ? slots : {}
  var entry = COPY[id]
  var template = typeof entry === "string" ? entry : (s.n === 1 ? entry.one : entry.other)
  return String(template).replace(/\{([a-z_]+)\}/g, function(whole, name) {
    var value = s.hasOwnProperty(name) ? s[name] : null
    return value === null || value === undefined ? "" : String(value)
  })
}

// "WeakAuras'" and "Retail's" (design doc §2.4's "WeakAuras' settings").
function possessive(name) {
  var s = str(name)
  if (s === "") return ""
  return /s$/i.test(s) ? s + "'" : s + "'s"
}

// The glyph of each effect kind (design doc §5.4), and the kinds that cost the player
// something they have now: those show first, in the urgent colour.
var SHEET_GLYPHS = {
  "dependency_added": 0xF0431, "folders_removed": 0xF0258, "local_changes_replaced": 0xF0DC9,
  "source_changed": 0xF059F, "channel_changed": 0xF0093, "while_running": 0xF040A, "reinstall": 0xF006F,
  "force": 0xF0026, "downgrade": 0xF02DA, "still_required": 0xF0026, "changed_since": 0xF02DA,
  "restore_scope": 0xF006F, "kept_backup": 0xF003C, "verified": 0xF05E0, "undo_last": 0xF054C,
  "delegated": 0xF04E6, "note": 0xF02FD
}
var SHEET_URGENT = ["folders_removed", "local_changes_replaced", "force", "still_required", "changed_since", "restore_scope"]
// The destructive verb's glyph, by what the sheet changes (design doc §5.4).
var SHEET_VERB_GLYPHS = { "remove": 0xF01B4, "prune": 0xF01B4, "forget": 0xF01B4, "undo": 0xF054C, "restore": 0xF006F }

function sheetEffect(kind, text) {
  return { "kind": kind, "severity": SHEET_URGENT.indexOf(kind) !== -1 ? "urgent" : "normal",
    "glyph": SHEET_GLYPHS.hasOwnProperty(kind) ? SHEET_GLYPHS[kind] : SHEET_GLYPHS.note, "text": text }
}

// A client by the name a player knows it by (design doc §6.3): the `targets` entry with
// this key, else this id; else the id itself.
function sheetClient(targets, id, key) {
  var list = Array.isArray(targets) ? targets : []
  var name = function(t) { return str(t.displayName) || str(t.id) }
  for (var i = 0; key && i < list.length; i++) if (list[i] && list[i].key === key) return name(list[i])
  for (var j = 0; id && j < list.length; j++) if (list[j] && list[j].id === id) return name(list[j])
  return str(id)
}

// A sheet item: its name (which elides) and its detail (versions, which never do).
function sheetItem(fields) {
  var f = fields && typeof fields === "object" ? fields : {}
  var item = {
    "op": str(f.op), "addon": str(f.addon), "client": str(f.client), "clientKey": str(f.clientKey) || str(f.client),
    "from": str(f.from), "to": str(f.to), "name": str(f.name), "detail": str(f.detail), "header": ""
  }
  item.text = item.detail ? item.name + " " + item.detail : item.name
  return item
}

// A source as a player reads it.
function sourceWords(source) {
  var s = str(source)
  if (s === "github" || s === "gh") return copy("source.github")
  if (s === "wowi" || s === "tukui" || s === "wowace" || s === "git") return copy("source." + s)
  return copy("source.file")
}

// One approval selection item as a sheet item (design doc §6.7's item lines).
// `folders` counts each removed addon's folders (by lower-cased name), from a remove's
// own items.
function selectionItem(item, targets, folders) {
  var it = item && typeof item === "object" ? item : {}
  var op = str(it.op)
  var from = str(it.from_version)
  var to = str(it.to_version)
  var detail = to || from
  if (op === "update") detail = from && to && from !== to ? from + " → " + to : (to || from)
  else if (op === "install") detail = copy("sheet.item.install", { "version": to }).replace(/^ /, "")
  else if (op === "remove") {
    var count = folders && folders.hasOwnProperty(asciiLower(str(it.addon))) ? folders[asciiLower(str(it.addon))] : null
    detail = count === null ? from : copy("sheet.item.remove", { "n": count, "version": from }).replace(/^ /, "")
  } else if (op === "edit" && Array.isArray(it.fields) && it.fields.length) {
    detail = copy("sheet.item.edit", { "fields": joinNames(it.fields.map(function(f) { return str(f).replace(/_/g, " ") })) })
  }
  return sheetItem({ "op": op, "addon": it.addon, "client": sheetClient(targets, str(it.target_id), str(it.target_key)),
    "clientKey": str(it.target_key) || str(it.target_id), "from": from, "to": to, "name": it.addon, "detail": detail })
}

// The distinct clients a sheet's items name, in order.
function sheetClients(items) {
  var keys = []
  var names = []
  for (var i = 0; i < items.length; i++) {
    if (keys.indexOf(items[i].clientKey) === -1) {
      keys.push(items[i].clientKey)
      names.push(items[i].client)
    }
  }
  return names
}

// "Update Foo on Retail?", "Update 3 addons on Retail?", "Update 3 addons on 2 clients?",
// and the verb "Update Foo" or "Update 3", for one of the counted kinds.
function countedTitle(kind, items) {
  var clients = sheetClients(items)
  if (items.length === 0) return copy("sheet.title.generic")
  if (items.length === 1 && items[0].addon) return copy("sheet.title." + kind + ".one", { "addon": items[0].addon, "client": items[0].client })
  if (clients.length <= 1) return copy("sheet.title." + kind + ".client", { "n": items.length, "client": clients[0] || "" })
  return copy("sheet.title." + kind + ".clients", { "n": items.length, "m": clients.length })
}

function countedConfirm(kind, items) {
  if (items.length === 1 && items[0].addon) return copy("sheet.confirm." + kind + ".one", { "addon": items[0].addon })
  return copy("sheet.confirm." + kind + ".many", { "n": items.length })
}

// A dependency's version, from its scope entry or the selection's install of it.
function dependencyVersion(entry, selection) {
  if (entry && typeof entry === "object" && (str(entry.version) || str(entry.to_version))) return str(entry.version) || str(entry.to_version)
  var wanted = asciiLower(entryName(entry))
  for (var i = 0; i < selection.length; i++) {
    var item = selection[i]
    if (item && item.op === "install" && asciiLower(str(item.addon)) === wanted) return str(item.to_version)
  }
  return ""
}

// The clients a change lands on that run now (the `targets` entries' `running`).
function runningClients(items, targets) {
  var list = Array.isArray(targets) ? targets : []
  var out = []
  for (var i = 0; i < items.length; i++) {
    for (var j = 0; j < list.length; j++) {
      var t = list[j]
      var same = t && (t.key === items[i].clientKey || t.id === items[i].clientKey)
      if (same && t.running === true && out.indexOf(items[i].client) === -1) out.push(items[i].client)
    }
  }
  return out
}

// The `reload_required` of a preview's data, over every client it reports.
function reloadOfData(data) {
  return reloadOf(Array.isArray(data.targets) ? data.targets : [data])
}

// One effect per approval reason (design doc §6.7's effect lines), from the typed
// approval and the sheet's items; `destructive` has none (the verb says it).
function reasonEffects(approval, items, targets, data) {
  var out = []
  var a = approval && typeof approval === "object" ? approval : { "reasons": [], "selection": [], "scope": {} }
  var scope = a.scope && typeof a.scope === "object" ? a.scope : {}
  var selection = Array.isArray(a.selection) ? a.selection : []
  var list = function(key) { return Array.isArray(scope[key]) ? scope[key] : [] }
  var single = items.length === 1 && items[0].addon ? items[0] : null
  var itemNamed = function(name) {
    for (var i = 0; i < selection.length; i++) if (selection[i] && asciiLower(str(selection[i].addon)) === asciiLower(name)) return selection[i]
    return null
  }
  var reasons = Array.isArray(a.reasons) ? a.reasons : []
  for (var r = 0; r < reasons.length; r++) {
    var reason = reasons[r]
    if (reason === "destructive") continue
    if (reason === "dependency_added") {
      var deps = list("dependencies_added").filter(function(d) { return !!entryName(d) })
      deps.forEach(function(d) {
        var version = dependencyVersion(d, selection)
        var name = entryName(d) + (version ? " " + version : "")
        out.push(sheetEffect(reason, single ? copy("sheet.effect.dependency_added", { "name": name, "addon": single.addon })
          : copy("sheet.effect.dependency_added.any", { "name": name })))
      })
      if (!deps.length) out.push(sheetEffect(reason, copy("sheet.effect.dependency_added.unnamed")))
    } else if (reason === "folders_removed") {
      var removed = list("folders_removed").map(function(f) { return typeof f === "string" ? f : f && typeof f === "object" ? str(f.folder) : "" })
        .filter(function(f) { return f !== "" })
      removed.forEach(function(f) { out.push(sheetEffect(reason, copy("sheet.effect.folders_removed", { "folder": f }))) })
      if (!removed.length) out.push(sheetEffect(reason, copy("sheet.effect.folders_removed.unnamed")))
    } else if (reason === "local_changes_replaced") {
      var local = scope.local_changes_replaced && typeof scope.local_changes_replaced === "object" ? scope.local_changes_replaced : {}
      var count = num(local.count)
      var where = stringList(local.folders)
      out.push(sheetEffect(reason, count <= 0 ? copy("sheet.effect.local_changes_replaced.unnamed")
        : where.length ? copy("sheet.effect.local_changes_replaced", { "n": count, "folders": joinNames(where) })
        : copy("sheet.effect.local_changes_replaced.nowhere", { "n": count })))
    } else if (reason === "source_changed") {
      var moved = list("source_changed").map(entryName).filter(function(n) { return n !== "" })
      moved.forEach(function(name) {
        var it = itemNamed(name)
        out.push(sheetEffect(reason, it && nonEmpty(it.source) ? copy("sheet.effect.source_changed", { "addon": name, "source": sourceWords(it.source) })
          : copy("sheet.effect.source_changed.unknown", { "addon": name })))
      })
      if (!moved.length) out.push(sheetEffect(reason, copy("sheet.effect.source_changed.unnamed")))
    } else if (reason === "channel_changed") {
      var channel = single ? itemNamed(single.addon) : null
      out.push(sheetEffect(reason, channel && nonEmpty(channel.channel)
        ? copy("sheet.effect.channel_changed", { "addon": single.addon, "channel": copy("channel." + channel.channel) })
        : copy("sheet.effect.channel_changed.unnamed")))
    } else if (reason === "while_running") {
      var running = runningClients(items, targets)
      var restart = reloadOfData(data) === "restart"
      out.push(sheetEffect(reason, running.length
        ? copy(restart ? "sheet.effect.while_running.restart" : "sheet.effect.while_running.reload", { "n": running.length, "clients": joinNames(running) })
        : (restart ? copy("sheet.effect.restart") : copy("sheet.effect.while_running.unnamed"))))
    } else if (reason === "reinstall") {
      out.push(sheetEffect(reason, single ? copy("sheet.effect.reinstall", { "owner": possessive(single.addon) }) : copy("sheet.effect.reinstall.unnamed")))
    } else if (reason === "force") {
      out.push(sheetEffect(reason, copy("sheet.effect.force")))
    } else if (reason === "downgrade") {
      out.push(sheetEffect(reason, single && single.from && single.to ? copy("sheet.effect.downgrade", { "from": single.from, "to": single.to })
        : copy("sheet.effect.downgrade.unnamed")))
    } else {
      out.push(sheetEffect("note", copy("sheet.effect.unknown", { "words": str(reason).replace(/_/g, " ") })))
    }
  }
  return out
}

// What every preview may add, whatever its command: others that still need what it
// removes (by the warning's code, never its text), and the game to reload or restart
// (unless a `while_running` line says it already).
function commonEffects(env, items, effects) {
  var out = []
  var data = env.data && typeof env.data === "object" ? env.data : {}
  var codes = Array.isArray(env.warnings) ? env.warnings.map(function(w) { return w && w.code }) : []
  if (codes.indexOf("still_required") !== -1) {
    var removed = items.filter(function(i) { return i.op === "remove" && i.addon })
    out.push(sheetEffect("still_required", removed.length === 1 ? copy("sheet.effect.still_required", { "addon": removed[0].addon })
      : copy("sheet.effect.still_required.unnamed")))
  }
  var said = effects.some(function(e) { return e.kind === "while_running" })
  var reload = reloadOfData(data)
  if (!said && reload === "reload") out.push(sheetEffect("note", copy("sheet.effect.reload")))
  if (!said && reload === "restart") out.push(sheetEffect("note", copy("sheet.effect.restart")))
  return out
}

// The items, titled and labelled by kind, for a preview whose approval lists the
// change: install, update, remove, adopt, pin, unpin, sync and copy. The dependencies it
// adds are effects, never items (M4-D28).
function selectionSheet(path, env, approval, targets) {
  var data = env.data && typeof env.data === "object" ? env.data : {}
  var folders = {}
  var removedItems = Array.isArray(data.items) ? data.items : []
  for (var i = 0; i < removedItems.length; i++) {
    var r = removedItems[i]
    if (r && !r.error && Array.isArray(r.folders)) folders[asciiLower(str(r.name || r.request))] = r.folders.length
  }
  var items = requestedItems(approval).map(function(it) { return selectionItem(it, targets, folders) })
  var reinstall = approval.reasons.indexOf("reinstall") !== -1
  var kinds = { "update": "update", "install": "install", "remove": "remove", "adopt": "adopt", "pin": "pin", "unpin": "unpin" }
  var kind = reinstall ? "reinstall" : (kinds.hasOwnProperty(path) ? kinds[path] : "change")
  var effects = reasonEffects(approval, items, targets, data)
  return { "title": countedTitle(kind, items), "items": items, "effects": effects.concat(commonEffects(env, items, effects)),
    "confirmLabel": countedConfirm(kind, items), "glyph": kind === "remove" ? SHEET_VERB_GLYPHS.remove : 0 }
}

// An undo's sheet (or a repair's undo, E7): what it reverts, where and from when, and
// each folder or operation it puts back.
function undoSheet(env, context, targets) {
  var data = env.data && typeof env.data === "object" ? env.data : {}
  var reverted = data.reverted && typeof data.reverted === "object" ? data.reverted : {}
  var repair = data.kind === "repair"
  var ids = []
  var txs = Array.isArray(data.transactions) ? data.transactions : []
  for (var t = 0; !repair && t < txs.length; t++) if (txs[t] && txs[t].target && txs[t].target.id) ids.push(txs[t].target.id)
  if (!ids.length) ids = revertedIds(reverted)
  var names = []
  ids.forEach(function(id) { var n = sheetClient(targets, id, ""); if (names.indexOf(n) === -1) names.push(n) })
  var kind = str(reverted.kind)
  var change = COPY.hasOwnProperty("sheet.change." + kind) ? copy("sheet.change." + kind) : copy("sheet.change.other")
  var date = dateText(reverted.at) || copy("sheet.date.unknown")
  var items = []
  if (repair) {
    var ops = Array.isArray(data.repairs) ? data.repairs : []
    for (var o = 0; o < ops.length; o++) items.push(sheetItem({ "op": "undo", "name": repairActionLine(ops[o], context, targets) }))
  } else {
    var folders = data.folders && typeof data.folders === "object" ? data.folders : {}
    stringList(folders.restored).forEach(function(f) { items.push(sheetItem({ "op": "undo", "name": copy("sheet.item.undo.restored", { "folder": f }) })) })
    stringList(folders.removed).forEach(function(f) { items.push(sheetItem({ "op": "undo", "name": copy("sheet.item.undo.removed", { "folder": f }) })) })
  }
  var effects = []
  // The undo of a whole restore reverts every play since it too: `changed_since` counts
  // the files the game changed (kept in the undo's snapshot), `null` when it cannot.
  if (typeof data.changed_since === "number" && data.changed_since > 0) {
    effects.push(sheetEffect("changed_since", copy("sheet.effect.changed_since", { "n": data.changed_since })))
  } else if (data.changed_since === null && kind === "restore") {
    effects.push(sheetEffect("changed_since", copy("sheet.effect.changed_since.uncounted")))
  }
  return {
    "title": names.length ? copy("sheet.title.undo", { "change": change, "clients": joinNames(names), "date": date })
      : copy("sheet.title.undo.nowhere", { "change": change, "date": date }),
    "items": items, "effects": effects,
    "confirmLabel": copy(kind === "undo" ? "sheet.confirm.redo" : "sheet.confirm.undo"), "glyph": SHEET_VERB_GLYPHS.undo
  }
}

// A restore's sheet (D7, D8): what it puts back, from when; a whole one says what else
// it changes and whether today's settings are kept first, one addon's which version
// wrote the settings and what the backup does not hold.
function restoreSheet(env, targets) {
  var data = env.data && typeof env.data === "object" ? env.data : {}
  var backup = data.backup && typeof data.backup === "object" ? data.backup : {}
  var target = data.target && typeof data.target === "object" ? data.target : {}
  var client = sheetClient(targets, str(target.id), str(target.key))
  var date = dateText(backup.captured_at) || copy("sheet.date.unknown")
  var codes = Array.isArray(env.warnings) ? env.warnings.map(function(w) { return w && w.code }) : []
  var items = []
  var effects = []
  var sheet = {}
  if (data.mode === "addons") {
    var addons = Array.isArray(data.addons) ? data.addons : []
    var owner = possessive(joinNames(addons.map(function(a) { return str(a.addon) }).filter(function(n) { return n !== "" })))
    sheet.title = copy("sheet.title.restore_addon", { "owner": owner, "client": client, "date": date })
    sheet.confirmLabel = copy("sheet.confirm.restore_addon", { "owner": owner })
    addons.forEach(function(a) {
      var f = a.files && typeof a.files === "object" ? a.files : {}
      items.push(sheetItem({ "op": "restore", "addon": a.addon, "client": client, "name": a.addon,
        "detail": copy("sheet.item.restore_addon", { "replaced": plural(num(f.replaced), "file"), "restored": num(f.restored),
          "created": num(f.created_since) }) }))
      var captured = str(a.captured_version)
      var installed = str(a.installed_version)
      if (captured && installed && captured !== installed) {
        effects.push(sheetEffect("note", copy("sheet.effect.settings_version", { "addon": str(a.addon), "captured": captured, "installed": installed })))
      }
      stringList(a.new_folders).forEach(function(folder) { effects.push(sheetEffect("note", copy("sheet.effect.not_in_backup", { "folder": folder }))) })
    })
  } else {
    var files = data.files && typeof data.files === "object" ? data.files : {}
    sheet.title = copy("sheet.title.restore", { "owner": possessive(client), "date": date })
    sheet.confirmLabel = copy("sheet.confirm.restore")
    items.push(sheetItem({ "op": "restore", "client": client, "name": copy("sheet.item.restore",
      { "files": plural(num(files.restored), "file"), "differed": num(files.differed), "created": num(files.created_since) }) }))
    effects.push(sheetEffect("restore_scope", copy("sheet.effect.restore_scope")))
    effects.push(codes.indexOf("before_restore_skipped") !== -1 ? sheetEffect("note", copy("sheet.effect.before_restore_skipped"))
      : sheetEffect("kept_backup", copy("sheet.effect.kept_backup")))
  }
  if (backup.integrity === "verified") effects.push(sheetEffect("verified", copy("sheet.effect.verified")))
  sheet.items = items
  sheet.effects = effects
  sheet.glyph = SHEET_VERB_GLYPHS.restore
  return sheet
}

// A prune's sheet: the backups it deletes, each by its kind and size, and those in use
// it keeps.
function pruneSheet(env, targets) {
  var data = env.data && typeof env.data === "object" ? env.data : {}
  var list = Array.isArray(data.targets) ? data.targets : []
  var items = []
  var effects = []
  for (var i = 0; i < list.length; i++) {
    var t = list[i] && typeof list[i] === "object" ? list[i] : {}
    var target = t.target && typeof t.target === "object" ? t.target : {}
    var client = sheetClient(targets, str(target.id), str(target.key))
    var pruned = Array.isArray(t.pruned) ? t.pruned : []
    for (var j = 0; j < pruned.length; j++) {
      var kind = str(pruned[j].kind)
      items.push(sheetItem({ "op": "prune", "client": client, "clientKey": str(target.key) || str(target.id),
        "name": copy("sheet.item.prune", { "kind": COPY.hasOwnProperty("backup.kind." + kind) ? copy("backup.kind." + kind) : kind,
          "size": bytesText(num(pruned[j].bytes)) }) }))
    }
    var busy = Array.isArray(t.busy_backups) ? t.busy_backups : []
    for (var k = 0; k < busy.length; k++) effects.push(sheetEffect("note", copy("sheet.effect.busy", { "id": str(busy[k].id || busy[k]) })))
  }
  var clients = sheetClients(items)
  return {
    "title": items.length === 1 ? copy("sheet.title.prune.one", { "client": clients[0] })
      : clients.length <= 1 ? copy("sheet.title.prune.client", { "n": items.length, "client": clients[0] || "" })
      : copy("sheet.title.prune.clients", { "n": items.length, "m": clients.length }),
    "items": items, "effects": effects,
    "confirmLabel": items.length === 1 ? copy("sheet.confirm.prune.one") : copy("sheet.confirm.prune.many", { "n": items.length }),
    "glyph": SHEET_VERB_GLYPHS.prune
  }
}

// Forgetting a client (`targets rm`, M3's binding confirmation): it touches nothing in
// the client's folder, and says so.
function forgetSheet(env, targets) {
  var data = env.data && typeof env.data === "object" ? env.data : {}
  var client = sheetClient(targets, str(data.id), "")
  var effects = [sheetEffect("note", copy("sheet.effect.forget_untouched"))]
  var managed = num(data.managed_addons)
  if (managed > 0) effects.push(sheetEffect("note", copy("sheet.effect.forget_record", { "addons": plural(managed, "managed addon") })))
  return { "title": copy("sheet.title.forget", { "client": client }), "items": [], "effects": effects,
    "confirmLabel": copy("sheet.confirm.forget", { "client": client }), "destructive": true, "glyph": SHEET_VERB_GLYPHS.forget }
}

// The sheet as PlanSheet.qml reads it: every key present, the items grouped by client
// when they span several (`header` on each group's first item), and the effects that
// cost something first, in their order otherwise.
function finishSheet(sheet) {
  var s = sheet && typeof sheet === "object" ? sheet : {}
  var items = Array.isArray(s.items) ? s.items : []
  var grouped = sheetClients(items).length > 1
  var last = null
  for (var i = 0; i < items.length; i++) {
    items[i].header = grouped && items[i].clientKey !== last ? items[i].client : ""
    last = items[i].clientKey
  }
  var effects = Array.isArray(s.effects) ? s.effects : []
  var urgent = effects.filter(function(e) { return e.severity === "urgent" })
  return {
    "title": str(s.title) || copy("sheet.title.generic"), "stale": str(s.stale), "items": items,
    "effects": urgent.concat(effects.filter(function(e) { return e.severity !== "urgent" })),
    "confirmLabel": str(s.confirmLabel) || copy("sheet.confirm.generic"), "cancelLabel": copy("sheet.cancel"),
    "destructive": s.destructive === true, "glyph": typeof s.glyph === "number" ? s.glyph : 0
  }
}

// The sheet of a preview that is no stored plan's (`targets rm`), or of a command's own
// data (undo, restore, prune). `context` and `targets` shorten a repair undo's paths and
// name the clients.
function confirmSheet(name, env, context, targets) {
  if (!envelopeOk(env) || !env.ok) return finishSheet({ "title": copy("sheet.title.failed") })
  var sheet = null
  if (env.command === "undo") sheet = undoSheet(env, context, targets)
  else if (env.command === "backup restore") sheet = restoreSheet(env, targets)
  else if (env.command === "backup prune") sheet = pruneSheet(env, targets)
  else if (env.command === "targets rm") sheet = forgetSheet(env, targets)
  else sheet = { "items": [sheetItem({ "name": resultLine(env, context, targets) })], "effects": [] }
  sheet.effects = sheet.effects.concat(commonEffects(env, sheet.items, sheet.effects))
  return finishSheet(sheet)
}

// A stored plan's sheet: the command's own items, the effects its approval names, and
// the urgent verb when the offer says the plan is destructive (never the path table).
function previewSheet(path, env, approval, plan, context, targets) {
  var sheet = null
  if (path === "undo" || path === "backup restore" || path === "backup prune") {
    sheet = path === "undo" ? undoSheet(env, context, targets)
      : path === "backup restore" ? restoreSheet(env, targets) : pruneSheet(env, targets)
    var data = env.data && typeof env.data === "object" ? env.data : {}
    var effects = reasonEffects(approval, sheet.items, targets, data).concat(sheet.effects)
    sheet.effects = effects.concat(commonEffects(env, sheet.items, effects))
  } else {
    sheet = selectionSheet(path, env, approval, targets)
  }
  sheet.destructive = (!!plan && plan.destructive === true) || approval.reasons.indexOf("destructive") !== -1
  return finishSheet(sheet)
}

// A sheet with the banner of a preview run again after its plan was refused (M4-D24),
// unless the sheet already says what changed.
function withStale(sheet, stale) {
  if (sheet && typeof sheet === "object" && !sheet.stale && typeof stale === "string" && stale !== "") sheet.stale = stale
  return sheet
}

// One plan item as a line: "Update Foo v1 → v2 on retail"; a missing part is left out.
function planItemLine(item) {
  var it = item && typeof item === "object" ? item : {}
  var op = str(it.op)
  var addon = str(it.addon)
  var where = str(it.target_id)
  var from = str(it.from_version)
  var to = str(it.to_version)
  var words = []
  if (op === "update") words = ["Update", addon, from, to ? "→ " + to : "", where ? "on " + where : ""]
  else if (op === "install") words = ["Install", addon, to, where ? "on " + where : ""]
  else if (op === "remove") words = ["Remove", addon, from, where ? "from " + where : ""]
  else if (op === "reinstall") words = ["Reinstall", addon, to || from, where ? "on " + where : ""]
  else words = [op ? op.charAt(0).toUpperCase() + op.slice(1) : "Change", addon, where ? "on " + where : ""]
  return words.filter(function(w) { return w !== "" }).join(" ")
}

// Whether a plan item is the shown item's partner: the same addon (ignoring ASCII case)
// on the same client, by key when both carry one, else by id.
function partners(shownItem, planItem) {
  if (!shownItem || !planItem || !nonEmpty(shownItem.addon) || !nonEmpty(planItem.addon)) return false
  if (asciiLower(shownItem.addon) !== asciiLower(planItem.addon)) return false
  if (nonEmpty(shownItem.target_key) && nonEmpty(planItem.target_key)) return shownItem.target_key === planItem.target_key
  return nonEmpty(shownItem.target_id) !== null && shownItem.target_id === planItem.target_id
}

function fieldWords(fields) {
  return joinNames(fields.map(function(f) { return f.replace(/_/g, " ") }))
}

// What differs between what the view showed and the plan's items, as phrases: per shown
// item in order, then the plan's items the view did not show.
function changeLines(shown, selection) {
  var rows = Array.isArray(shown) ? shown : []
  var items = Array.isArray(selection) ? selection : []
  var used = []
  var out = []
  for (var i = 0; i < rows.length; i++) {
    var row = rows[i] && typeof rows[i] === "object" ? rows[i] : {}
    var name = nonEmpty(row.addon) || "an addon"
    var at = -1
    for (var j = 0; j < items.length && at === -1; j++) if (used.indexOf(j) === -1 && partners(row, items[j])) at = j
    if (at === -1) {
      out.push(name + (nonEmpty(row.target_id) ? " on " + row.target_id : "") + " is no longer part of it")
      continue
    }
    used.push(at)
    var item = items[at]
    var both = function(field) { return nonEmpty(row[field]) !== null && nonEmpty(item[field]) !== null }
    var differs = function(field) { return both(field) && !sameField(field, row[field], item[field]) }
    if (differs("to_version")) {
      out.push(name + " " + item.to_version + " is now the newest")
    } else {
      var version = nonEmpty(item.to_version) ? " " + item.to_version : ""
      if (differs("release_id")) out.push(name + version + " is another release now")
      if (differs("asset")) out.push(name + version + " comes in another file now (" + item.asset + ")")
    }
    if (differs("source")) out.push(name + " comes from " + item.source + " now")
    if (differs("channel")) out.push(name + " follows " + item.channel + " now")
    if (differs("from_version")) {
      var where = nonEmpty(row.target_id) || nonEmpty(item.target_id)
      out.push(name + (where ? " on " + where : "") + " is at " + item.from_version + " now")
    }
    var noRow = MATCH_FIELDS.filter(function(f) { return nonEmpty(row[f]) === null })
    var noPlan = MATCH_FIELDS.filter(function(f) { return nonEmpty(item[f]) === null })
    if (noRow.length) out.push("the view did not show " + name + "'s " + fieldWords(noRow))
    if (noPlan.length) out.push("the preview did not say " + name + "'s " + fieldWords(noPlan))
  }
  for (var k = 0; k < items.length; k++) {
    if (used.indexOf(k) === -1) out.push("Also: " + planItemLine(items[k]) + " (not in the view)")
  }
  return out
}

// The dialog's heading for a plan that differs from the view: its first three changes.
function changeHeading(lines) {
  var all = Array.isArray(lines) ? lines : []
  if (all.length === 0) return "Changed since you looked"
  return "Changed since you looked: " + all.slice(0, 3).join("; ") + (all.length > 3 ? "; and " + (all.length - 3) + " more" : "")
}

// The fix of the first warning with `code`, as text ("" for none).
function warningFix(env, code) {
  var list = env && Array.isArray(env.warnings) ? env.warnings : []
  for (var i = 0; i < list.length; i++) if (list[i] && list[i].code === code) return str(list[i].fix)
  return ""
}

// What to do with a preview's envelope (M4-D28). `spec` is `{mode: "match" | "confirm",
// shown, again, path, context, targets}`: `match` compares the plan with what the view
// `shown`, `again` marks a preview run after a refused plan, `path` is the previewed
// command's, and `context` and `targets` name clients and shorten paths. The decision
// is `{kind, plan, heading, sheet, line, fixText}`, its kind one of "failed", "refuse"
// (nothing ran, `line` says why), "refresh" (nothing to do), "dialog" (ask, with the
// plan `sheet`) and "direct" (apply `plan` with no sheet: a routine plan that is exactly
// what the view showed).
function previewDecision(spec, env) {
  var s = spec && typeof spec === "object" ? spec : {}
  var decision = { "kind": "failed", "plan": null, "heading": "", "sheet": null, "line": "", "fixText": "" }
  if (!envelopeOk(env) || !env.ok) return decision
  var data = env.data && typeof env.data === "object" ? env.data : {}
  var approval = approvalOf(data.approval)
  decision.kind = "refuse"
  if (!approval) {
    decision.line = "The preview did not say what it would change; nothing ran"
    return decision
  }
  if (data.plan_id === null && approval.selection.length === 0) {
    decision.kind = "refresh"
    decision.line = s.mode === "match" ? "Already up to date; the view is refreshed" : "Nothing to do; the view is refreshed"
    return decision
  }
  if (data.plan_id === null) {
    decision.line = "The preview could not be kept as a plan; nothing ran"
    decision.fixText = warningFix(env, "plan_not_stored")
    return decision
  }
  var plan = planFrom(env)
  if (!plan) {
    decision.line = "The preview offered no plan to apply; nothing ran"
    return decision
  }
  decision.kind = "dialog"
  decision.plan = plan
  // Anything but a match is confirmed in the sheet: never applied directly.
  if (s.mode !== "match") {
    decision.sheet = previewSheet(s.path, env, approval, plan, s.context, s.targets)
    return decision
  }
  var shown = Array.isArray(s.shown) ? s.shown : []
  var compared = requestedItems(approval)
  var equal = selectionMatches(shown, compared)
  if (equal && approval.routine && !plan.destructive && s.again !== true) {
    decision.kind = "direct"
    return decision
  }
  decision.sheet = previewSheet(s.path, env, approval, plan, s.context, s.targets)
  if (!equal && shown.length > 0) {
    decision.heading = changeHeading(changeLines(shown, compared))
    decision.sheet.stale = decision.heading
  }
  return decision
}

// A plan sheet (or a decision's) as text, one line each: the title, the banner, every
// item (each client's name before its group) and every effect. IPC's state() carries it,
// for the checkpoint scripts and anyone reading the widget from a terminal.
function dialogText(value) {
  var sheet = value && typeof value === "object" && value.sheet && typeof value.sheet === "object" ? value.sheet : value
  if (!sheet || typeof sheet !== "object" || typeof sheet.title !== "string") return ""
  var lines = [sheet.title]
  if (sheet.stale) lines.push(sheet.stale)
  var items = Array.isArray(sheet.items) ? sheet.items : []
  for (var i = 0; i < items.length; i++) {
    if (items[i].header) lines.push(items[i].header)
    lines.push(str(items[i].text))
  }
  var effects = Array.isArray(sheet.effects) ? sheet.effects : []
  for (var j = 0; j < effects.length; j++) lines.push(str(effects[j].text))
  return lines.join("\n")
}

// An `apply` result read as the envelope of the command it ran, so resultLine,
// failedItems, itemFixes and commandResultLine say what that command did; null for
// anything else.
function applyResult(env) {
  if (!envelopeOk(env) || env.command !== "apply" || !env.data || typeof env.data !== "object") return null
  var data = env.data
  if (!data.applied || typeof data.applied !== "object" || typeof data.applied.command !== "string") return null
  if (!data.result || typeof data.result !== "object" || Array.isArray(data.result)) return null
  return {
    "ok": env.ok, "schema_version": env.schema_version, "tool_version": env.tool_version,
    "command": data.applied.command, "exit_code": env.exit_code, "partial": env.partial, "data": data.result,
    "warnings": env.warnings, "commands": env.commands, "error": env.error,
    "outcomes": Array.isArray(data.outcomes) ? data.outcomes : []
  }
}

// Whether an `apply` calls for the preview again, never `apply` again: the plan went
// stale or is gone, or a client's part went stale.
function needsPreviewAgain(env) {
  if (!envelopeOk(env) || env.command !== "apply") return false
  var code = env.error && typeof env.error === "object" ? env.error.code : ""
  if (!env.ok && (code === "plan_stale" || code === "plan_not_found")) return true
  var view = applyResult(env)
  return !!view && view.outcomes.some(function(o) { return !!o && o.status === "stale" })
}

// -------------------------------------------------------------- the doctor's repairs (G2)
//
// Every decision the widget makes about a repair, pure (M4-D44, M4-D46): a repair runs
// through the bound `doctor_fix` templates only, previewed, and its undo through
// `undo`'s stored plan. The envelopes are the doctor's (M4-D34): the report's keys at
// the top level, no `data`.

function isDoctorEnvelope(env) {
  if (!envelopeOk(env) || env.command.indexOf("doctor ") !== 0 || env.hasOwnProperty("data")) return false
  return typeof env.state === "string" || (!!env.failure_mode && typeof env.failure_mode === "object")
}

// A `doctor fix` report (E6's FixReport), read defensively; null for anything else.
// `report` is an answer to show (exits 0 to 3), `repaired` a real run that changed
// something.
function doctorResult(env) {
  if (!isDoctorEnvelope(env) || env.command !== "doctor fix") return null
  var s = env.summary && typeof env.summary === "object" ? env.summary : {}
  var dryRun = env.dry_run === true
  var exitCode = num(env.exit_code)
  var error = env.error && typeof env.error === "object" ? env.error : null
  var state = str(env.state)
  var tx = strOrNull(env.tx)
  var actionsTaken = num(s.actions_taken)
  var list = function(value) { return Array.isArray(value) ? value.filter(function(v) { return !!v && typeof v === "object" }) : [] }
  return {
    "command": env.command, "exitCode": exitCode, "state": state, "dryRun": dryRun, "tx": tx,
    "summary": {
      "findingsBefore": num(s.findings_before), "findingsAfter": num(s.findings_after),
      "actionsTaken": actionsTaken, "actionsFailed": num(s.actions_failed), "bytesToBackUp": num(s.bytes_to_back_up),
      "bytesBackedUp": num(s.bytes_backed_up), "backupsRestored": num(s.backups_restored), "totalFindings": num(s.total_findings)
    },
    "actions": list(dryRun ? env.actions_planned : env.actions),
    "delegated": list(dryRun ? env.delegated_planned : env.delegated),
    "precondition": typeof env.precondition === "string" && TOKEN_PATTERN.test(env.precondition) ? env.precondition : "",
    "findings": list(env.findings),
    "undoable": Array.isArray(env.undo_argv),
    "error": error,
    "report": !error && [0, 1, 2, 3].indexOf(exitCode) !== -1,
    // A run with repairs verified and other findings left is DONE_FINDINGS: it changed
    // something too.
    "repaired": !dryRun && !!tx && (state === "DONE_OK" || state === "DONE_PARTIAL" || state === "DONE_FINDINGS")
      && actionsTaken > 0
  }
}

// A path as a person reads it: below omaddons' own folders `<config>`, `<state>` and
// `<cache>`, below a client `<id>:`, below HOME `~`, the longest prefix on a component
// boundary winning; a path below a `WTF/` is never shown (it names the account).
function displayPath(path, context, targets) {
  var p = str(path)
  if (/(^|\/)WTF\/./.test(p)) return "(a path under WTF/, not shown)"
  if (!context || typeof context !== "object") return p
  var roots = []
  var add = function(prefix, shown, client) {
    if (typeof prefix !== "string" || prefix.charAt(0) !== "/") return
    var trimmed = prefix.replace(/\/+$/, "")
    if (trimmed) roots.push({ "prefix": trimmed, "shown": shown, "client": client })
  }
  if (context.XDG_CONFIG_HOME) add(context.XDG_CONFIG_HOME + "/omaddons", "<config>", false)
  if (context.XDG_STATE_HOME) add(context.XDG_STATE_HOME + "/omaddons", "<state>", false)
  if (context.XDG_CACHE_HOME) add(context.XDG_CACHE_HOME + "/omaddons", "<cache>", false)
  var list = Array.isArray(targets) ? targets : []
  for (var i = 0; i < list.length; i++) if (list[i] && list[i].clientDir) add(list[i].clientDir, str(list[i].id) + ":", true)
  add(context.HOME, "~", false)
  var best = null
  for (var j = 0; j < roots.length; j++) {
    var r = roots[j]
    if ((p === r.prefix || p.indexOf(r.prefix + "/") === 0) && (!best || r.prefix.length > best.prefix.length)) best = r
  }
  if (!best) return p
  var rest = p.slice(best.prefix.length)
  return best.client ? best.shown + rest.replace(/^\/+/, "") : best.shown + rest
}

// Whether a permission mode lets the group or others read (`0o044`).
function exposesMode(mode) {
  return typeof mode === "string" && /^[0-7]{3,4}$/.test(mode) && (parseInt(mode, 8) & 0o044) !== 0
}

// One repair operation as a dialog or result line: a planned action (E6's
// ActionPlanned, or ActionDone), or an undo's inverse (E7's UndoRepairOp: `octal_mode`
// and `to_rel`).
function repairActionLine(action, context, targets) {
  var a = action && typeof action === "object" ? action : {}
  var op = str(a.op)
  var path = displayPath(a.path, context, targets)
  var pre = a.pre && typeof a.pre === "object" ? a.pre : {}
  if (op === "chmod") {
    var mode = str(a.mode || a.octal_mode)
    var was = str(pre.mode)
    return "chmod " + mode + " " + path + (was ? " (was " + was + ")" : "")
      + (exposesMode(mode) && !exposesMode(was) ? " (readable by other users again)" : "")
  }
  if (op === "write_file") {
    if (typeof a.backup === "string" || pre.kind === "file") {
      return "Rewrite " + path + " (" + (typeof a.bytes === "number" ? bytesText(a.bytes) + "; " : "")
        + "the old file is kept for undo)"
    }
    return "Create " + path + (typeof a.bytes === "number" && a.bytes > 0 ? " (" + bytesText(a.bytes) + ")" : "")
  }
  if (op === "rename_noreplace") {
    var to = displayPath(a.to || a.to_rel, context, targets)
    if (a.activation === true) return "Reactivate the interrupted change's journal (" + to + "); the next change finishes it"
    return "Move " + path + " to " + to
  }
  return op.replace(/_/g, " ") + " " + path
}

// What a repair's preview decides (G1's decision shape, with its plan sheet): "dialog"
// with the token to bind and an item per change, "refresh" when nothing is left to
// repair, "refuse" when the preview is not exactly this repair or gives no token,
// "failed" for an error or another envelope. The sheet's title is the problem's, from
// the request's label ("Repair: <title>", repairRequest's).
function doctorPreviewDecision(spec, env, context, targets) {
  var slots = spec && spec.slots && typeof spec.slots === "object" ? spec.slots : {}
  var decision = { "kind": "failed", "token": "", "heading": "", "sheet": null, "line": "", "fixText": "" }
  var result = doctorResult(env)
  if (!result || result.error) return decision
  if (result.actions.length === 0 && result.delegated.length === 0) {
    decision.kind = "refresh"
    decision.line = "Nothing to repair now; the view is refreshed"
    return decision
  }
  decision.kind = "refuse"
  for (var i = 0; i < result.actions.length; i++) {
    if (str(result.actions[i].fm) !== str(slots.fm)) {
      decision.line = "The preview planned more than this repair; nothing ran"
      return decision
    }
  }
  if (!result.precondition) {
    decision.line = "The preview gave no token to bind; nothing ran"
    return decision
  }
  decision.kind = "dialog"
  decision.token = result.precondition
  var items = result.actions.map(function(a) { return sheetItem({ "op": "repair", "name": repairActionLine(a, context, targets) }) })
  var effects = result.delegated.map(function(d) {
    return sheetEffect("delegated", copy("sheet.effect.delegated", { "tx": str(d.tx), "direction": str(d.direction).replace(/_/g, " ") }))
  })
  if (result.summary.bytesToBackUp > 0) effects.push(sheetEffect("kept_backup", copy("sheet.effect.keeps_copy", { "size": bytesText(result.summary.bytesToBackUp) })))
  effects.push(sheetEffect("undo_last", copy("sheet.effect.undo_last")))
  var label = str(spec && spec.label)
  decision.sheet = finishSheet({
    "title": label.indexOf("Repair: ") === 0 && label.length > 8 ? copy("sheet.title.repair", { "problem": label.slice(8) })
      : copy("sheet.title.repair.unnamed"),
    "items": items, "effects": effects, "confirmLabel": copy("sheet.confirm.repair")
  })
  return decision
}

// A repair's result line: its error, what it repaired, or why nothing was.
function doctorResultLine(env, context, targets) {
  var r = doctorResult(env)
  if (!r) return ""
  if (r.error) return str(r.error.message) || "The repair was refused"
  var after = r.summary.findingsAfter
  var remains = plural(after, "finding") + (after === 1 ? " remains" : " remain")
  if (r.state === "DONE_FAILED") return "The repair failed and was rolled back; nothing changed"
  if (r.state === "DONE_PARTIAL") return "Partly repaired (" + plural(r.summary.actionsTaken, "change") + "); " + remains
  if (r.summary.actionsTaken > 0 && r.actions.length > 0) {
    var n = r.actions.length
    return "Repaired: " + repairActionLine(r.actions[0], context, targets) + (n > 1 ? " (and " + (n - 1) + " more)" : "")
  }
  return "Nothing was repaired" + (after > 0 ? "; " + remains + " (see Issues)" : "")
}

// One evidence value as text, or null to leave it out.
function evidenceText(value, context, targets) {
  if (typeof value === "string") return value.charAt(0) === "/" ? displayPath(value, context, targets) : value
  if (typeof value === "boolean") return value ? "yes" : "no"
  if (typeof value === "number" && isFinite(value)) return String(value)
  if (Array.isArray(value) && value.length > 0 && value.every(function(v) { return typeof v === "string" })) {
    var shown = value.slice(0, 5).map(function(v) { return v.charAt(0) === "/" ? displayPath(v, context, targets) : v })
    return shown.join(", ") + (value.length > 5 ? " and " + (value.length - 5) + " more" : "")
  }
  return null
}

// A `doctor explain` answer (E3's ExplainReport) as lines: what it is, why it matters,
// whether a repair exists, what to do by hand, and each instance now with its evidence
// and the command for a terminal; [] for anything else.
function explainLines(env, context, targets) {
  if (!isDoctorEnvelope(env) || env.command !== "doctor explain" || !env.failure_mode || typeof env.failure_mode !== "object") return []
  var fm = env.failure_mode
  var lines = []
  if (fm.title) lines.push(str(fm.title))
  if (fm.description) lines.push(str(fm.description))
  if (fm.why) lines.push("Why it matters: " + str(fm.why))
  var fixer = fm.fixer && typeof fm.fixer === "object" ? fm.fixer : null
  if (!fixer) lines.push("No automatic repair")
  else if (fixer.needs_force === true) lines.push("Its repair needs your deliberate consent: read this, then run the command below in a terminal")
  else if (fixer.enabled === false) lines.push("Its repair is off by default: run the command below in a terminal")
  var manual = fm.manual && typeof fm.manual === "object" ? fm.manual : null
  if (manual && manual.instruction) lines.push("By hand: " + str(manual.instruction) + (manual.why ? " (" + str(manual.why) + ")" : ""))
  var instances = Array.isArray(env.instances) ? env.instances.filter(function(i) { return !!i && typeof i === "object" }) : []
  for (var i = 0; i < instances.length && i < WIDGET_EXPLAIN_INSTANCES_MAX; i++) {
    var inst = instances[i]
    lines.push(str(inst.label) || str(inst.instance))
    var evidence = inst.evidence && typeof inst.evidence === "object" && !Array.isArray(inst.evidence) ? inst.evidence : {}
    var shown = 0
    var keys = Object.keys(evidence).sort()
    for (var k = 0; k < keys.length && shown < WIDGET_EXPLAIN_EVIDENCE_MAX; k++) {
      var text = evidenceText(evidence[keys[k]], context, targets)
      if (text === null) continue
      lines.push("  " + keys[k].replace(/_/g, " ") + ": " + text)
      shown++
    }
    var remediation = inst.remediation && typeof inst.remediation === "object" ? inst.remediation : {}
    if (remediation.command) lines.push("  In a terminal: " + str(remediation.command))
  }
  if (instances.length > WIDGET_EXPLAIN_INSTANCES_MAX) lines.push("(and " + (instances.length - WIDGET_EXPLAIN_INSTANCES_MAX) + " more)")
  if (instances.length === 0) lines.push("No instance found now; it may be resolved")
  if (env.incomplete && typeof env.incomplete === "object" && env.incomplete.limit) {
    lines.push("The check was cut short (" + str(env.incomplete.limit).replace(/_/g, " ") + "); this may be incomplete")
  }
  return lines
}

// The explanation's lines for its own row only (view-bound, as rawLines).
function explainFor(explanation, row) {
  if (!explanation || !explanation.selection || !row || explanation.selection.key !== row.key) return null
  return Array.isArray(explanation.lines) ? explanation.lines : null
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
    IRREVERSIBLE_FLAGS: IRREVERSIBLE_FLAGS,
    IRREVERSIBLE_NOTE: IRREVERSIBLE_NOTE,
    CONTEXT_KEYS: CONTEXT_KEYS,
    pathOf: pathOf,
    templateShape: templateShape,
    buildArgv: buildArgv,
    classifyCliArgv: classifyCliArgv,
    cliArgv: cliArgv,
    cliAction: cliAction,
    copyText: copyText,
    copyArgv: copyArgv,
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
    staleText: staleText,
    fixText: fixText,
    VIEWS: VIEWS,
    VIEW_TITLES: VIEW_TITLES,
    KEY_CONTEXTS: KEY_CONTEXTS,
    KEYMAP: KEYMAP,
    keyName: keyName,
    keyStep: keyStep,
    keyScopes: keyScopes,
    keyRow: keyRow,
    panelKey: panelKey,
    dialogKey: dialogKey,
    helpKey: helpKey,
    keyHint: keyHint,
    withKey: withKey,
    keyTip: keyTip,
    helpSections: helpSections,
    stepView: stepView,
    VIEW_GLYPHS: VIEW_GLYPHS,
    scopeTargets: scopeTargets,
    liveTargets: liveTargets,
    openingScope: openingScope,
    keptScope: keptScope,
    clientGroup: clientGroup,
    forgetAction: forgetAction,
    scopeFacts: scopeFacts,
    heroTitle: heroTitle,
    heroMeta: heroMeta,
    heroState: heroState,
    heroDetail: heroDetail,
    footerText: footerText,
    scopeOptions: scopeOptions,
    viewCounts: viewCounts,
    targetById: targetById,
    nextFilter: nextFilter,
    stepFilter: stepFilter,
    addonsDirArgv: addonsDirArgv,
    openTarget: openTarget,
    updateRequest: updateRequest,
    undoAction: undoAction,
    progressFraction: progressFraction,
    resultUndo: resultUndo,
    sourceKind: sourceKind,
    rgbOf: rgbOf,
    contrastRatio: contrastRatio,
    mixColor: mixColor,
    dimAlpha: dimAlpha,
    dimColor: dimColor,
    DIM_ALPHA_FLOOR: DIM_ALPHA_FLOOR,
    DIM_MIN_CONTRAST: DIM_MIN_CONTRAST,
    stableSort: stableSort,
    updateRows: updateRows,
    updateCount: updateCount,
    oldestCheck: oldestCheck,
    reduceCheck: reduceCheck,
    readingResources: readingResources,
    viewState: viewState,
    VIEW_RESOURCES: VIEW_RESOURCES,
    SOURCE_TROUBLE: SOURCE_TROUBLE,
    installedRows: installedRows,
    unmanagedRows: unmanagedRows,
    installedCount: installedCount,
    filterMatches: filterMatches,
    nextSort: nextSort,
    INSTALLED_SORTS: INSTALLED_SORTS,
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
    resultToastArgv: resultToastArgv,
    PLAN_PATHS: PLAN_PATHS,
    PLAN_ID_PATTERN: PLAN_ID_PATTERN,
    MATCH_FIELDS: MATCH_FIELDS,
    storesPlan: storesPlan,
    approvalOf: approvalOf,
    planFrom: planFrom,
    shownOf: shownOf,
    shownForAddon: shownForAddon,
    requestedItems: requestedItems,
    selectionMatches: selectionMatches,
    COPY: COPY,
    copy: copy,
    possessive: possessive,
    SHEET_GLYPHS: SHEET_GLYPHS,
    SHEET_URGENT: SHEET_URGENT,
    sheetClient: sheetClient,
    selectionItem: selectionItem,
    reasonEffects: reasonEffects,
    confirmSheet: confirmSheet,
    previewSheet: previewSheet,
    withStale: withStale,
    planItemLine: planItemLine,
    changeLines: changeLines,
    changeHeading: changeHeading,
    previewDecision: previewDecision,
    dialogText: dialogText,
    applyResult: applyResult,
    needsPreviewAgain: needsPreviewAgain,
    FM_ID_PATTERN: FM_ID_PATTERN,
    CONSENT_FLAGS: CONSENT_FLAGS,
    WIDGET_EXPLAIN_INSTANCES_MAX: WIDGET_EXPLAIN_INSTANCES_MAX,
    WIDGET_EXPLAIN_EVIDENCE_MAX: WIDGET_EXPLAIN_EVIDENCE_MAX,
    holdsConsent: holdsConsent,
    repairRequest: repairRequest,
    isDoctorEnvelope: isDoctorEnvelope,
    doctorResult: doctorResult,
    displayPath: displayPath,
    repairActionLine: repairActionLine,
    doctorPreviewDecision: doctorPreviewDecision,
    doctorResultLine: doctorResultLine,
    explainLines: explainLines,
    explainFor: explainFor,
    explainArgv: explainArgv,
    lastChangeOf: lastChangeOf
  }
}
