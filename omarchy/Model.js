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
  // Toasts while a game runs (1n2p.33): Off holds them for one digest after the session.
  "notifyWhilePlaying": "Off",
  "refreshIntervalSec": 300,
  // The first run's keybinding tip (1n2p.32): On until the player has seen it.
  "keyHint": "On"
}

// What each setting accepts; anything else falls back to its default.
var SETTING_RULES = {
  "binary": { "type": "path" },
  "panelWidth": { "type": "integer", "min": 400, "max": 800, "step": 20 },
  "autoCheck": { "type": "switch" },
  "notifyUpdates": { "type": "switch" },
  "notifyErrors": { "type": "switch" },
  "notifyWhilePlaying": { "type": "switch" },
  "refreshIntervalSec": { "type": "integer", "min": 30, "max": 3600, "step": 30 },
  "keyHint": { "type": "switch" }
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

// ------------------------------------------------------------- the settings sheet (1n2p.31)

// The panel's own settings, in the sheet's order (design doc §2.7). `keyHint` is the
// first run's tip, which seeing it turns off: never a setting a player picks.
var PANEL_SETTING_KEYS = ["panelWidth", "refreshIntervalSec", "autoCheck", "notifyErrors", "notifyUpdates", "notifyWhilePlaying", "binary"]

// "30 s", "5 min", "1 min 30 s".
function secondsText(n) {
  var s = num(n)
  if (s < 60) return copy("settings.value.seconds", { "n": s })
  var minutes = Math.floor(s / 60)
  var rest = s % 60
  return rest === 0 ? copy("settings.value.minutes", { "n": minutes }) : copy("settings.value.minutes_seconds", { "n": minutes, "s": rest })
}

// A panel setting's value in words: "On", "Off", "540 px", "5 min", the binary's path.
function settingText(key, value) {
  var rule = SETTING_RULES[key]
  if (!rule) return str(value)
  if (rule.type === "switch") return copy(value === "On" ? "settings.value.on" : "settings.value.off")
  if (key === "panelWidth") return copy("settings.value.px", { "n": value })
  if (key === "refreshIntervalSec") return secondsText(value)
  return value === "" ? copy("settings.value.binary_stamped") : str(value)
}

// The Panel section's rows (design doc §2.7): each setting's label, what it does, its
// value (`setting`'s, so never an invalid one) in words, and its kind: "switch",
// "integer" (stepped) or "path" (typed).
function panelSettingRows(settings) {
  return PANEL_SETTING_KEYS.map(function(key) {
    var value = setting(settings, key)
    return { "section": "panel", "key": key, "kind": SETTING_RULES[key].type, "label": copy("settings.panel." + key),
      "about": copy("settings.panel." + key + ".about"), "value": value, "text": settingText(key, value) }
  })
}

// The value a step gives a panel setting: a switch flips; a number moves by its step
// within its bounds, past the end back to the start when `wrap` (Enter, a click), never
// past them otherwise (h and l). A path is typed, never stepped.
function steppedSetting(settings, key, step, wrap) {
  var rule = SETTING_RULES[key]
  var value = setting(settings, key)
  if (!rule) return value
  if (rule.type === "switch") return value === "On" ? "Off" : "On"
  if (rule.type !== "integer") return value
  var next = value + (step < 0 ? -rule.step : rule.step)
  if (next > rule.max) return wrap ? rule.min : rule.max
  if (next < rule.min) return wrap ? rule.max : rule.min
  return next
}

// A typed binary path: "" (the stamped binary) or an absolute path, trimmed; null for
// anything else, which `setting` would turn back into the default anyway.
function typedPath(text) {
  var s = str(text).trim()
  return s === "" || s.charAt(0) === "/" ? s : null
}

// omaddons' own settings in the sheet (design doc §2.7): each key, the template that sets
// it, and the values offered, in the order Enter steps through them. `config.toml`
// stays the one source: the panel reads it with `config` and keeps no copy.
var OMADDONS_SETTINGS = [
  { "key": "default_channel", "template": "config_channel", "values": ["stable", "beta", "alpha"] },
  { "key": "running_policy", "template": "config_running", "values": ["safe", "refuse", "allow"] },
  { "key": "auto_backup", "template": "config_backups", "values": ["daily", "daily-copy", "off"] },
  { "key": "check_interval_hours", "template": "config_interval", "values": [1, 2, 3, 4, 6, 8, 12, 24, 48] }
]

// `config --json`: each setting's value and whether it is the default.
function reduceConfig(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || !Array.isArray(env.data.settings)) return null
  var settings = {}
  env.data.settings.forEach(function(s) {
    if (s && typeof s.name === "string") settings[s.name] = { "value": s.value, "source": str(s.source) }
  })
  return { "settings": settings }
}

// An omaddons setting's value in words ("Stable", "Safe ones", "6 h").
function configText(key, value) {
  if (key === "check_interval_hours") return copy("settings.value.hours", { "n": value })
  var id = "settings.omaddons." + key + "." + str(value)
  return COPY.hasOwnProperty(id) ? copy(id) : str(value)
}

// The omaddons section's rows, from `config` (reduced): each key's label, what it does,
// its value in words and the values Enter and h/l step through. A value the sheet does
// not offer (set by hand) shows as it is; a step from it goes to the first offered.
function omaddonsSettingRows(config) {
  var settings = config && config.settings ? config.settings : {}
  return OMADDONS_SETTINGS.map(function(s) {
    var held = settings.hasOwnProperty(s.key) ? settings[s.key] : null
    var value = held ? held.value : null
    return { "section": "omaddons", "key": s.key, "kind": "choice", "label": copy("settings.omaddons." + s.key),
      "about": copy("settings.omaddons." + s.key + ".about"), "value": value, "text": value === null ? "" : configText(s.key, value),
      "template": s.template, "values": s.values }
  })
}

// The change a step on a choice row runs: the next offered value (Enter and l on, h
// back, past the end round again when `wrap`), applied directly with the result shown;
// null when there is nothing to change. An omaddons setting sets its `{value}`; a
// client's game sets it for that client (`{target}`, its key); the default client is
// `targets default` with the chosen client's id.
function configRequest(row, step, wrap) {
  if (!row || !Array.isArray(row.values) || row.values.length === 0) return null
  var at = -1
  for (var i = 0; i < row.values.length; i++) if (String(row.values[i]) === String(row.value)) at = i
  var next = at === -1 ? 0 : at + (step < 0 ? -1 : 1)
  if (next >= row.values.length) next = wrap ? 0 : row.values.length - 1
  if (next < 0) next = wrap ? row.values.length - 1 : 0
  if (next === at) return null
  var value = String(row.values[next])
  var words = row.texts && row.texts.hasOwnProperty(value) ? row.texts[value] : configText(row.key, row.values[next])
  var slots = row.template === "targets_default" ? { "target": value }
    : (row.target ? { "target": row.target, "value": value } : { "value": value })
  return { "template": row.template, "slots": slots, "label": copy("settings.omaddons.change", { "setting": row.label, "value": words }) }
}

// The games a client's family can be set to (`targets.<key>.family`), for a client whose
// version omaddons cannot read (`family` "unknown"), as the CLI takes them.
var CLIENT_FAMILIES = ["mainline", "vanilla", "tbc", "wrath", "cata", "mists", "titan", "forever",
  "vanilla-legacy", "tbc-legacy", "wrath-legacy", "cata-legacy", "mists-legacy"]

// The Clients section's rows (design doc §2.7), from `status`'s clients: each client
// present by its name (Enter types a new one; empty restores the built-in one), its game
// where omaddons can't tell it, which client is the default (with more than one), each
// client no longer installed (Enter forgets it, through the plan sheet), Scan for
// clients, and Add a client by its folder.
function clientSettingRows(statusTargets) {
  var list = Array.isArray(statusTargets) ? statusTargets : []
  var live = list.filter(function(t) { return t && t.missing !== true })
  var rows = []
  var nameOf = function(t) { return sheetClient(list, t.id, "") }
  live.forEach(function(t) {
    var client = nameOf(t)
    rows.push({ "section": "clients", "key": "name:" + t.id, "kind": "name", "target": str(t.key) || str(t.id), "id": t.id,
      "label": client, "value": client, "text": t.isDefault ? copy("settings.clients.default_mark") : "",
      "about": copy("settings.clients.name.about", { "id": t.id }) })
    if (t.family === "unknown") {
      var texts = {}
      CLIENT_FAMILIES.forEach(function(f) { texts[f] = copy("settings.family." + f) })
      rows.push({ "section": "clients", "key": "family:" + t.id, "kind": "choice", "template": "config_client_family",
        "target": str(t.key) || str(t.id), "label": copy("settings.clients.family", { "owner": possessive(client) }),
        "about": copy("settings.clients.family.about"), "value": null, "text": copy("settings.clients.family.unknown"),
        "values": CLIENT_FAMILIES, "texts": texts })
    }
  })
  if (live.length > 1) {
    var chosen = live.filter(function(t) { return t.isDefault })[0] || null
    var names = {}
    live.forEach(function(t) { names[t.id] = nameOf(t) })
    rows.push({ "section": "clients", "key": "default", "kind": "choice", "template": "targets_default",
      "label": copy("settings.clients.default"), "about": copy("settings.clients.default.about"),
      "value": chosen ? chosen.id : null, "text": chosen ? names[chosen.id] : copy("settings.clients.default.none"),
      "values": live.map(function(t) { return t.id }), "texts": names })
  }
  list.filter(function(t) { return t && t.missing === true }).forEach(function(t) {
    var offered = forgetAction(t) !== null
    rows.push({ "section": "clients", "key": "forget:" + t.id, "kind": "action", "action": offered ? "forget" : "", "id": t.id,
      "client": nameOf(t), "label": copy("settings.clients.missing", { "client": nameOf(t) }), "text": offered ? copy("settings.clients.forget") : "",
      "about": copy(offered ? "settings.clients.forget.about" : "settings.clients.forget.none") })
  })
  rows.push({ "section": "clients", "key": "rescan", "kind": "action", "action": "rescan", "label": copy("settings.clients.rescan"),
    "text": "", "about": copy("settings.clients.rescan.about") })
  rows.push({ "section": "clients", "key": "add", "kind": "action", "action": "add", "label": copy("settings.clients.add"),
    "text": "", "about": copy("settings.clients.add.about") })
  return rows
}

// What a client's typed name runs: `config set targets.<key>.name`; the label names the
// client as it was, never the new text (a label is one short line).
function clientNameRequest(row, text) {
  return { "template": "config_client_name", "slots": { "target": row.target, "name": str(text).trim() },
    "label": copy("settings.clients.name.label", { "client": row.label }) }
}

// The settings sheet's sections (design doc §2.7): each `{key, title, rows, note}`, every
// row with its index for the sheet's one cursor; `note` says what a section waits for.
// `input`: {settings} (the panel's, as the shell applied them), {config} (`config`,
// reduced; null until read), {configFailed} (the read's failure, or "") and {targets}
// (`status`'s clients).
function settingsSections(input) {
  var i = input || {}
  var sections = [{ "key": "panel", "title": copy("settings.section.panel"), "rows": panelSettingRows(i.settings), "note": "" }]
  sections.push({ "key": "omaddons", "title": copy("settings.section.omaddons"), "rows": i.config ? omaddonsSettingRows(i.config) : [],
    "note": i.config ? "" : (i.configFailed ? copy("settings.omaddons.failed") : copy("settings.omaddons.reading")) })
  sections.push({ "key": "clients", "title": copy("settings.section.clients"), "rows": clientSettingRows(i.targets), "note": "" })
  var index = 0
  sections.forEach(function(s) { s.rows.forEach(function(r) { r.index = index++ }) })
  return sections
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

// The settings sheet's (1n2p.31): `config` reads omaddons' settings whole, and each
// `config_*` sets one key named in its own argv, so no other key is reachable from the
// panel; its value is a `{value}` CONFIG_VALUE_PATTERN admits. A client's own settings
// (`config_client_*`) name the client by its key inside theirs (KEYED_SETTINGS), and its
// name is the one free text a change takes (`{name}`, one line); `targets_default`
// makes a client the default.
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
  "addon_info":     { "argv": ["info", "--changelog", "--", "{ref}"],               "mutates": false, "lane": "network" },
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
  "catalog":        { "argv": ["search", "--catalog"],                              "mutates": false, "lane": "local" },
  "catalog_target": { "argv": ["search", "--catalog", "-t", "{target}"],            "mutates": false, "lane": "local" },
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
  "doctor_fix_target": { "argv": ["doctor", "fix", "-t", "{target}", "--only={fm}"], "mutates": true, "preview": "bind" },
  "targets_add":       { "argv": ["targets", "add", "--", "{path}"],               "mutates": true },
  "adopt_target":      { "argv": ["adopt", "--apply", "-t", "{target}"],           "mutates": true, "preview": "confirm" },
  "history":           { "argv": ["history", "-v", "--limit", "100"],              "mutates": false, "lane": "local", "actor": false },
  "undo_run":          { "argv": ["undo", "--run={run}"],                          "mutates": true, "preview": "confirm" },
  "undo_run_target":   { "argv": ["undo", "-t", "{target}", "--run={run}"],        "mutates": true, "preview": "confirm" },
  "config":            { "argv": ["config"],                                       "mutates": false, "lane": "local" },
  "config_channel":    { "argv": ["config", "set", "default_channel", "--", "{value}"], "mutates": true },
  "config_running":    { "argv": ["config", "set", "running_policy", "--", "{value}"], "mutates": true },
  "config_backups":    { "argv": ["config", "set", "auto_backup", "--", "{value}"], "mutates": true },
  "config_interval":   { "argv": ["config", "set", "check_interval_hours", "--", "{value}"], "mutates": true },
  "config_client_name": { "argv": ["config", "set", "targets.{target}.name", "--", "{name}"], "mutates": true },
  "config_client_family": { "argv": ["config", "set", "targets.{target}.family", "--", "{value}"], "mutates": true },
  "targets_default":   { "argv": ["targets", "default", "--", "{target}"],         "mutates": true }
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
var SLOTS = { "target": true, "addon": true, "ref": true, "backup": true, "query": true, "fm": true, "path": true, "run": true, "value": true,
  "name": true }
// A client's own settings in `config set` (the settings sheet's Clients, 1n2p.31): the
// only elements a slot sits inside, the client named by its key (a ULID: no dot to
// split the key path). Each is `config set`'s key, right after `set`.
var KEYED_SETTINGS = ["targets.{target}.name", "targets.{target}.family"]
var KEY_SEGMENT_PATTERN = /^[A-Za-z0-9_-]+$/
// A client's player name (`targets.<key>.name`): one line of at most 200 characters, as
// the CLI keeps it; empty restores the built-in name.
var CLIENT_NAME_PATTERN = /^[^\u0000-\u001f\u007f]*$/
var CLIENT_NAME_MAX_CHARS = 200
// A setting's value for `config set` (the settings sheet, 1n2p.31): one of the words or
// numbers the sheet offers, never free text: lower case, digits and dashes, at most 16
// characters. Each template names its own key; the CLI judges the value against it.
var CONFIG_VALUE_PATTERN = /^[a-z0-9][a-z0-9-]{0,15}$/
// A run's id as `history` writes it, whole: a ULID in Crockford's upper case. History's
// undo names the run it read (1n2p.30), never a prefix the CLI could match twice.
var RUN_PATTERN = /^[0-9A-HJKMNP-TV-Z]{26}$/
// A client folder the player types (`targets add`, 1n2p.32): absolute, one line, no
// control character, at most PATH_MAX_BYTES; the CLI judges what it holds.
var PATH_PATTERN = /^\/[^\u0000-\u001f\u007f]*$/
var PATH_MAX_BYTES = 4096
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
    // A client's own setting: its key names the client, the one slot inside an element.
    var keyed = KEYED_SETTINGS.indexOf(part) !== -1 && path === "config set" && i === 2
    if (!SLOTS.hasOwnProperty(slot) || (close !== part.length - 1 && !keyed)) {
      throw refuse("bad_template", name + " has a malformed slot in " + part)
    }
    if (keyed) {
      slots.push(slot)
      continue
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
  // A change always records who made it; only a read may name no actor (`actor: false`).
  if (template.actor === false && template.mutates) throw refuse("bad_template", name + " names no actor but mutates")
  return { path: path, slots: slots }
}

function checkSlot(name, slot, value, template, part) {
  // A client's name may be empty: that restores its built-in name.
  if (typeof value !== "string" || (value === "" && slot !== "name")) throw refuse("bad_value", name + ": {" + slot + "} is empty")
  if (value.indexOf("\u0000") !== -1) throw refuse("bad_value", name + ": {" + slot + "} holds a NUL")
  if (slot === "target" && !TARGET_PATTERN.test(value)) {
    throw refuse("bad_target", name + ": " + JSON.stringify(value) + " is no target id or key")
  }
  // Inside a setting's key, the client is one key segment.
  if (slot === "target" && part !== undefined && part !== "{target}" && !KEY_SEGMENT_PATTERN.test(value)) {
    throw refuse("bad_target", name + ": " + JSON.stringify(value) + " is no key segment")
  }
  if (slot === "name" && (!CLIENT_NAME_PATTERN.test(value) || value.length > CLIENT_NAME_MAX_CHARS)) {
    throw refuse("bad_name", name + ": a client's name is one line of at most " + CLIENT_NAME_MAX_CHARS + " characters")
  }
  if (slot === "fm" && !FM_ID_PATTERN.test(value)) {
    throw refuse("bad_fm", name + ": " + JSON.stringify(value) + " is no failure-mode id")
  }
  if (slot === "path" && (!PATH_PATTERN.test(value) || utf8Length(value) > PATH_MAX_BYTES)) {
    throw refuse("bad_path", name + ": " + JSON.stringify(value) + " is no absolute path")
  }
  if (slot === "run" && !RUN_PATTERN.test(value)) {
    throw refuse("bad_run", name + ": " + JSON.stringify(value) + " is no run id")
  }
  if (slot === "value" && !CONFIG_VALUE_PATTERN.test(value)) {
    throw refuse("bad_setting", name + ": " + JSON.stringify(value) + " is no setting value the panel offers")
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

// The flags every run starts with, right after the binary (M3-D2). A read whose
// template says `actor: false` names no actor: `history` reads an explicit `--actor` as
// a filter (README `history`), and History lists every change, not only the panel's.
function standardFlags(opts, progress, preview, ifMatch, anonymous) {
  var argv = [binaryOf(opts), "--json"]
  if (anonymous !== true) argv.push("--actor", "widget")
  argv.push("--reason", reasonOf(opts))
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
    var close = part.indexOf("}", open)
    var slot = part.slice(open + 1, close)
    if (!given.hasOwnProperty(slot)) throw refuse("unfilled_slot", name + ": {" + slot + "}")
    checkSlot(name, slot, given[slot], template, part)
    // Slicing, never String.replace, whose `$&` patterns would rewrite the value; a
    // client's setting keeps its key's tail (`targets.<key>.name`).
    filled.push(part.slice(0, open) + given[slot] + part.slice(close + 1))
  }
  var row = CLI_PATHS[shape.path]
  var preview = o.preview === true
  // `match` and `confirm` confirm through their plan's apply; a `bind` template's real
  // run is its own argv bound to its preview's token, so it needs that token instead.
  var planned = template.preview === "match" || template.preview === "confirm"
  if (planned && !preview) throw refuse("preview_required", name + " runs only as its preview; its Yes runs the plan's apply")
  var ifMatch = ifMatchFor(name, row, template.preview === "confirm", template.bind === true || template.preview === "bind", o, preview)
  // Every change, and every read that asks the sources: the activity bar counts them, and
  // their progress keeps a network read's inactivity deadline alive (1n2p.18).
  var progress = template.mutates === true || template.lane === "network"
  return { argv: standardFlags(o, progress, preview, ifMatch, template.actor === false).concat(filled), env: contextOf(o.context) }
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
  // As buildArgv: every change, and the reads that ask the sources (the network lane's).
  var progress = verdict.mutates || verdict.path === "outdated" || verdict.path === "search"
    || (verdict.path === "info" && rest.indexOf("--changelog") !== -1)
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
// queues}`. User mutations go before background ones (none preempts a running one).
// On the local lane a user's `status` jumps the queue, then the reads someone waits on,
// then the background's (an open's quiet reads and its scan, 1n2p.38).
function nextRequest(queues, lanesBusy) {
  var out = copyQueues(queues)
  var busy = lanesBusy || {}
  var start = { "mutation": null, "local": null, "network": null }
  for (var lane in start) {
    var list = out[lane]
    if (busy[lane] || list.length === 0) continue
    var at = 0
    if (lane === "mutation") at = Math.max(0, indexWhere(list, function(r) { return r.priority === "user" }))
    if (lane === "local") {
      at = indexWhere(list, function(r) { return r.resource === "status" && r.priority !== "background" })
      if (at === -1) at = Math.max(0, indexWhere(list, function(r) { return r.priority !== "background" }))
    }
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
    "irreversible": verdict !== null && verdict.reason === "irreversible",
    // An omaddons command the widget never runs (a purge, a forced repair, `doctor gc`, a
    // repair outside its bound flow): shown under "Copy command" for a terminal. A fix
    // with no argv is prose, and an argv that is no omaddons command line is neither.
    // A plan's apply offered outside its preview is neither: it is no command for anyone.
    "terminal": verdict !== null && !verdict.ok && ["malformed", "not_omaddons", "apply_outside_plan"].indexOf(verdict.reason) === -1,
    // No argv at all: `command` is advice in words.
    "prose": !Array.isArray(argv)
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

// A problem row's fix, explanation, text and hint (§3 of the repair bead, 1n2p.28): a
// repair previews through its bound template; a `doctor explain` remediation is the
// row's explanation, never its action; any other runnable remediation runs as issueFix
// says (a reinstall or an update through its template, so it previews first); a command
// the widget never runs is text to copy, and a fix with no argv a sentence.
function problemRow(issue, targetId, targets) {
  var r = issue.remediation || {}
  var repair = repairRequest(issue, targetId)
  var explainsOnly = !!r.runnable && verdictPath(r.argv) === "doctor explain"
  var issueExplain = issue.explain && issue.explain.runnable && verdictPath(issue.explain.argv) === "doctor explain"
    ? issue.explain : null
  var explain = explainsOnly ? r : issueExplain
  var way = repair ? { "fix": repair, "action": null, "go": null, "label": copy("issues.fix.repair") }
    : (explainsOnly ? null : issueFix(r, targets))
  return fixFields(way, explain, r, issue.forced === true, !!repair)
}

// The fields every Issues row shares (1n2p.28): what its fix runs (`fix`, a template;
// `action`, the CLI's own argv through the classifier; `go`, a view or one of the
// frame's own actions; at most one is set), the fix's button name (`fixLabel`), the
// doctor's explanation (`explain`), the command it shows instead of running (`text`,
// with `note` saying why it is no button), a fix that is prose (`prose`), its named
// buttons in order (`buttons`: `{name, text, tooltip}`, `name` one of fix, explain,
// copy), what Enter does (`primary`: "fix", "explain" or "") and Enter's hint.
function fixFields(way, explain, r, forced, repair) {
  var terminal = !way && !!r && r.terminal === true
  var out = {
    "fix": way ? way.fix : null,
    "action": way ? way.action : null,
    "go": way ? way.go : null,
    "fixLabel": way ? way.label : "",
    "explain": explain || null,
    "forced": forced === true,
    "repair": repair === true,
    "text": terminal ? str(r.command) : "",
    // Why a command is no button (design doc §6.5.4): what a purge deletes, that a forced
    // repair is consented to in a terminal after reading why, or that the panel never
    // runs it.
    "note": !terminal ? "" : r.irreversible ? IRREVERSIBLE_NOTE
      : forced === true ? copy(explain ? "issues.terminal.forced" : "issues.terminal.forced.unexplained")
      : copy("issues.terminal.other"),
    "prose": !way && !!r && r.prose === true ? str(r.command) : "",
    "buttons": [],
    "primary": way ? "fix" : (explain ? "explain" : "")
  }
  var key = keyHint("enter", "issues")
  out.hint = out.primary === "" ? "" : copy(out.repair ? "issues.hint.repair" : out.primary === "fix" ? "issues.hint.fix" : "issues.hint.explain", { "key": key })
  if (way) out.buttons.push({ "name": "fix", "text": way.label, "tooltip": out.hint })
  if (out.explain) out.buttons.push({ "name": "explain", "text": copy("issues.explain"), "tooltip": keyTip("explain", "issues") })
  if (out.text) out.buttons.push({ "name": "copy", "text": copy("issues.copy"), "tooltip": keyTip("copy", "issues") })
  return out
}

// An omaddons argv's parts after its command path (`argv[0]` is "omaddons"): the path
// the classifier gives it ("" when it refuses it), the `-t` value, the other flags and
// the operands, in order.
function argvParts(argv) {
  var out = { "path": verdictPath(argv), "target": "", "flags": [], "operands": [] }
  if (!out.path) return out
  for (var i = 1 + out.path.split(" ").length; i < argv.length; i++) {
    var arg = argv[i]
    if (arg === "--") {
      out.operands = out.operands.concat(argv.slice(i + 1))
      break
    }
    if (arg === "-t" || arg === "--target") {
      out.target = str(argv[++i])
    } else if (arg.charAt(0) === "-") {
      out.flags.push(arg)
    } else {
      out.operands.push(arg)
    }
  }
  return out
}

// How the panel runs a command the CLI offered (a problem's remediation, a status
// recommendation, a warning's fix), from its argv's shape (1n2p.28): a template where one
// fits it exactly (a reinstall or an update previews first, as the Addons and Updates
// views' do; a backup runs as `b` does); the frame's own action or view where the panel
// has one (Check now, a refresh, reinstalling the panel, the Errors, Backups or Addons
// view); else the argv itself through the classifier. `{fix, action, go, label}`, one of
// the three set and `label` its button's name; null for a command the widget never runs.
function issueFix(action, targets) {
  if (!action || action.runnable !== true || !Array.isArray(action.argv)) return null
  var p = argvParts(action.argv)
  var client = p.target ? sheetClient(targets, p.target, "") : ""
  var owner = possessive(buttonName(client))
  var plain = p.flags.length === 0 && p.operands.length === 0
  var way = function(kind, value, label) {
    var out = { "fix": null, "action": null, "go": null, "label": label }
    out[kind] = value
    return out
  }
  // The run's label (its recorded --reason) names the addon in full; the button, at most
  // BUTTON_NAME_MAX_CHARS of it, so it fits the narrowest panel.
  var template = function(name, slots, id, words) {
    var full = words || {}
    var short = {}
    for (var key in full) short[key] = buttonName(full[key])
    // One line of at most LABEL_MAX_CHARS, as reasonOf takes it (a client's own name may
    // run to 200 characters).
    var label = copy(id, full).replace(/[\r\n]+/g, " ")
    if (label.length > LABEL_MAX_CHARS) label = label.slice(0, LABEL_MAX_CHARS - 1) + "…"
    return way("fix", { "template": name, "slots": slots, "label": label }, copy(id, short))
  }
  if (p.path === "install" && p.target && p.operands.length === 1 && p.flags.length === 1 && p.flags[0] === "--reinstall") {
    return template("reinstall", { "target": p.target, "addon": p.operands[0] }, "issues.fix.reinstall", { "addon": p.operands[0] })
  }
  if (p.path === "update" && p.flags.length === 0) {
    if (p.target && p.operands.length === 1) return template("update_addon", { "target": p.target, "addon": p.operands[0] }, "issues.fix.update_addon", { "addon": p.operands[0] })
    if (p.target && p.operands.length === 0) return template("update_target", { "target": p.target }, "issues.fix.update_client", { "client": client })
    if (plain) return template("update_all", {}, "issues.fix.update_all")
  }
  if (p.path === "backup create" && plain) {
    return p.target ? template("backup_target", { "target": p.target }, "issues.fix.backup_client", { "client": client })
      : template("backup_all", {}, "issues.fix.backup_all")
  }
  if (p.path === "outdated" && plain && !p.target) return way("go", { "kind": "check", "view": "", "target": "" }, copy("issues.fix.check"))
  if (p.path === "status" && !p.target && p.operands.length === 0) return way("go", { "kind": "refresh", "view": "", "target": "" }, copy("issues.fix.refresh"))
  if (p.path === "omarchy install" && plain && !p.target) return way("go", { "kind": "widget", "view": "", "target": "" }, copy("issues.fix.widget"))
  if (p.path === "errors" && plain) return way("go", { "kind": "view", "view": "errors", "target": p.target }, copy("issues.fix.errors"))
  if (p.path === "backup list" && plain) return way("go", { "kind": "view", "view": "backups", "target": p.target }, copy("issues.fix.backups"))
  if (p.path === "list" && p.operands.length === 0 && p.flags.every(function(f) { return ["-v", "--verbose", "--unmanaged"].indexOf(f) !== -1 })) {
    return way("go", { "kind": "view", "view": "installed", "target": p.target },
      copy(p.target ? "issues.fix.addons" : "issues.fix.addons.all", { "owner": owner }))
  }
  // `adopt -t <id>` alone is a report; the take-over flow is the frame's (first run,
  // 1n2p.32): its preview, then the plan sheet.
  if (p.path === "adopt" && plain && p.target) {
    return way("go", { "kind": "adopt", "view": "installed", "target": p.target }, copy("issues.fix.take_over", { "owner": owner }))
  }
  if (p.path === "targets rm" && p.flags.length === 0 && p.operands.length === 1) {
    return way("action", action, copy("state.action.forget", { "client": buttonName(sheetClient(targets, p.operands[0], "")) }))
  }
  if (p.path === "remove" && p.operands.length > 0) {
    return way("action", action, copy("issues.fix.remove", { "addons": buttonName(joinNames(p.operands)) }))
  }
  return way("action", action, copy("issues.fix.other"))
}

var BUTTON_NAME_MAX_CHARS = 32

// A name as a button shows it: whole up to BUTTON_NAME_MAX_CHARS, else cut with "…".
function buttonName(name) {
  var s = str(name)
  return s.length > BUTTON_NAME_MAX_CHARS ? s.slice(0, BUTTON_NAME_MAX_CHARS - 1) + "…" : s
}

// Whether two argv are the same command line, element for element.
function sameArgv(a, b) {
  if (!Array.isArray(a) || !Array.isArray(b) || a.length !== b.length) return false
  for (var i = 0; i < a.length; i++) if (a[i] !== b[i]) return false
  return true
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
  var undo = target.undo_snapshots && typeof target.undo_snapshots === "object" ? target.undo_snapshots : null
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
    // `targets default`'s client: where an install goes without -t (the settings sheet).
    "isDefault": target["default"] === true,
    "running": target.running === true,
    // `null` when the process table could not be read (`running_unknown`).
    "runningUnknown": target.running === null,
    // Why it needs attention, typed (`journal_pending`: an interrupted change, 1n2p.34).
    "attention": stringList(target.attention),
    // Folders omaddons does not manage there (the first run's take-over, 1n2p.32).
    "unmanaged": target.folders && typeof target.folders === "object" ? num(target.folders.unmanaged) : 0,
    // From `status -v`: where `o` opens Interface/AddOns.
    "clientDir": str(target.client_dir),
    "missing": target.missing === true,
    "addons": target.addons && typeof target.addons === "object" ? num(target.addons.total) : null,
    // Unmanaged folders a catalog entry names, and dependencies nothing needs: what the
    // Issues view's suggestions count (1n2p.28).
    "adoptable": num(target.adoptable),
    "orphans": num(target.orphans),
    "updates": updates ? {
      "available": num(updates.available), "pinned": num(updates.pinned), "blocked": num(updates.blocked),
      "failed": num(updates.failed), "unchecked": num(updates.unchecked),
      "checkedAt": strOrNull(updates.checked_at), "stale": updates.stale === true
    } : null,
    "issues": issues,
    "errors": errors ? {
      // "buggrabber", or "none" with no recorder installed (the first run's card, 1n2p.32).
      "capture": str(errors.capture),
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
      // For the Backups view's card of a client `backup list` names no backup of.
      "auto": autoOf(backups.auto),
      "freeBytes": typeof backups.free_bytes === "number" ? backups.free_bytes : null,
      "bytes": typeof backups.bytes === "number" ? backups.bytes : null
    } : null,
    // The undo data kept for it (bmcx.4, the Backups card's line): null under
    // `--cached`, which reads no history; `bytes` only with `-v`.
    "undoSnapshots": undo ? {
      "count": num(undo.count), "prunable": num(undo.prunable), "oldestAt": strOrNull(undo.oldest_at),
      "bytes": typeof undo.bytes === "number" ? undo.bytes : null, "truncated": undo.truncated === true
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

// An RFC 3339 instant, as the CLI writes them ("2026-11-01T10:00:00.123456789Z"), or a
// date, in ms since the epoch; NaN for any other text, which never reaches Date.parse:
// Qt's V4 engine spends about a millisecond on a text it cannot parse (a slow fallback),
// which the Updates view paid for every row without a release date, 480 ms at 500 rows
// (1n2p.38). Every time the widget reads from text goes through here.
var INSTANT_PATTERN = /^\d{4}-\d{2}-\d{2}(?:[Tt]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:[Zz]|[+-]\d{2}:\d{2})?)?$/

function instantMs(iso) {
  var s = typeof iso === "string" ? iso : ""
  return INSTANT_PATTERN.test(s) ? Date.parse(s) : NaN
}

// "25 min ago", "3 hours ago", "2 days ago", "just now"; "" without a time.
function sinceText(iso, nowMs) {
  var at = instantMs(iso)
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

// `status --cached -v`'s envelope as the widget's model: `{failed, targets, summary,
// journal, lastChange, widget, commands, warnings, issues}`. A run that failed (an
// envelope with `ok: false`) keeps no data and is `failed`; null (no answer yet) is
// not. `journal` is `{pending, inProgress}`: interrupted changes, and whether another
// omaddons process is committing now. The bar button's model is barState's, over this.
function reduceStatus(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || typeof env.data !== "object") {
    return {
      "failed": !!env && typeof env === "object",
      "targets": [], "summary": null, "journal": null, "lastChange": null, "widget": null, "commands": [], "warnings": [], "issues": []
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
  var journal = data.journal && typeof data.journal === "object" ? data.journal : null
  return {
    "failed": false,
    "targets": targets,
    "journal": journal ? { "pending": num(journal.pending), "inProgress": journal.in_progress === true } : null,
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
      "fixText": fix && fix.runnable ? "" : str(w.fix),
      // The fix as the classifier judged it, a command it refuses included (the Issues
      // view shows such a command under Copy command, with why, 1n2p.28).
      "cli": fix
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

// ------------------------------------------------------------ the bar button (1n2p.16)
//
// What the bar button shows (design doc §2.5), pure, from `status`'s reduced model and
// the service's context. `state` is the headline, the most pressing first:
//   broken     no binary, another build, a status that failed, or no answer ever read;
//   starting   no answer yet;
//   stale      no answer for longer than the safety net behind the doorbell;
//   empty      no client on this computer;
//   attention  new Lua errors or a P0 or P1 problem;
//   updates    updates wait;
//   idle       nothing to do.
// A check or a change the player waits on spins the glyph in any state but broken, and
// a client that runs is `playing` on a fresh answer. Each state shows a glyph or a mark
// besides its colour: the spinner, the badge, dimming, and one mark under the badge,
// the most pressing of broken, stale, attention and running. decided: the running mark
// replaces nothing but yields to the attention mark (the tooltip still says which
// client runs); starting and empty share a look, dimmed with no mark, because neither
// has anything to show yet and the tooltip says which.

var BAR_STATES = ["broken", "starting", "stale", "empty", "attention", "updates", "idle"]
// How often the service recomputes the bar with nothing else changing: the stale mark
// and the tooltip's ages follow the clock.
var BAR_TICK_MS = 30000

// ------------------------------------------------------------ the open (1n2p.38)

// How old the last full `status -v` (discovery's scan) may be before a panel open asks
// for another, in the background after the open's own reads: a client installed or
// moved since shows within this. The widget's cost baseline (design doc, "Costs,
// baseline") found the scan cheap and the open's cost the widget's own reducing and
// rendering, so an open shows what `status --cached -v` says first, and rescans rarely.
var FULL_SCAN_MAX_AGE_MS = 10 * 60 * 1000

// Whether a panel open asks for a full scan after its own reads: none was made yet, the
// last is older than `maxAgeMs`, or the cached status itself asks for one (it offers
// `status` when no discovery cache exists yet).
function fullScanDue(lastAtMs, nowMs, maxAgeMs, commands) {
  if (typeof lastAtMs !== "number" || lastAtMs <= 0) return true
  if (typeof nowMs === "number" && nowMs - lastAtMs > (typeof maxAgeMs === "number" ? maxAgeMs : FULL_SCAN_MAX_AGE_MS)) return true
  return (Array.isArray(commands) ? commands : []).some(function(c) { return c && c.action === "status" })
}

// --------------------------------------------------- window events (M3-D13, 1n2p.38)

// The windows that may be a WoW client's or its launcher's, by Hyprland's class and
// title. Under Wine a window's class names its launcher, not the game (vision, F0
// 2026-10-22: umu gave the 3.3.5a client `steam_app_wow335` from its GAMEID; Omarchy
// names the Battle.net prefix's windows `steam_app_battlenet`), or the exe for a plain
// Wine window; the game's title is "World of Warcraft". So every Proton or umu window,
// every Wine window named for its exe, and anything that names WoW or Battle.net
// passes: generous on purpose, since a client start missed costs more than one
// `status --cached`, and the poll (every 30 s while a client runs) stays the net.
var WINDOW_CLASS_PATTERNS = [/^steam_app_/i, /\.exe$/i, /wow/i, /warcraft/i, /battle\.?net/i]
var WINDOW_TITLE_PATTERNS = [/warcraft/i, /battle\.?net/i]
// How many such windows' addresses the service remembers for their `closewindow`, which
// names only the address.
var WINDOW_MEMORY = 32

// What a Hyprland raw event asks of the service: `{refresh, followUp, remember,
// forget}`, or null for an event that asks nothing. An `openwindow`'s data is
// `<address>,<workspace>,<class>,<title>` (the title may hold commas): a window that may
// be WoW's refreshes and is remembered by its address. A `closewindow`'s data is the
// address alone: it refreshes when the window was remembered, or while a client runs
// (its window may have opened before the widget did), and only then is followed up,
// since a Wine game outlives its window. Any other window (a terminal, a browser, a
// stream overlay) asks nothing.
function windowEventAction(name, data, remembered, anyRunning) {
  var text = str(data)
  var known = Array.isArray(remembered) ? remembered : []
  if (name === "openwindow") {
    var parts = text.split(",")
    var address = parts[0] || ""
    var cls = parts.length > 2 ? parts[2] : ""
    var title = parts.length > 3 ? parts.slice(3).join(",") : ""
    var wow = WINDOW_CLASS_PATTERNS.some(function(p) { return p.test(cls) })
      || WINDOW_TITLE_PATTERNS.some(function(p) { return p.test(title) })
    if (!wow) return null
    return { "refresh": true, "followUp": false, "remember": address, "forget": "" }
  }
  if (name === "closewindow") {
    var closed = text.split(",")[0]
    var mine = closed !== "" && known.indexOf(closed) !== -1
    if (!mine && anyRunning !== true) return null
    return { "refresh": true, "followUp": anyRunning === true, "remember": "", "forget": mine ? closed : "" }
  }
  return null
}
// The marks' glyphs (design doc §5.4): md-alert_circle, md-clock_outline, md-alert,
// md-play.
var BAR_MARK_GLYPHS = { "broken": 0xF0028, "stale": 0xF0150, "attention": 0xF0026, "running": 0xF040A }
// The marks drawn in the urgent colour; the others take the bar's foreground.
var BAR_URGENT_MARKS = ["broken", "attention"]

// How long an answer counts as fresh: the safety net's interval (`refreshIntervalSec`)
// plus a status read's own deadline. Past it, the doorbell and the poll both missed.
function barStaleAfterMs(settings) {
  return (intSetting(settings, "refreshIntervalSec", 300, 30, 3600) + WIDGET_LOCAL_DEADLINE_SEC) * 1000
}

// What needs the player, counted over `status`'s summary: new local Lua errors, the P0
// and P1 problems (`urgent` is the two together: the badge's reason for the urgent
// colour, UX-30), every problem, and updates.
function barCounts(summary) {
  var errors = summary && summary.errors ? num(summary.errors["new"]) : 0
  var urgentIssues = summary ? num(summary.issuesUrgent) : 0
  return {
    "errors": errors,
    "urgentIssues": urgentIssues,
    "issues": summary ? num(summary.issuesTotal) : 0,
    "urgent": errors + urgentIssues,
    "updates": summary && summary.updates ? num(summary.updates.available) : 0
  }
}

// The clients a fresh answer says run.
function barRunning(targets) {
  return liveTargets(targets).filter(function(t) { return t.running === true })
}

// barState without its tooltip. `status` is reduceStatus's model (null before an
// answer); `ctx` is `{phase, working, nowMs, answeredAtMs, staleAfterMs}`: the service's
// phase ("starting", "ok", "missing", "unreachable", "incompatible"), whether
// Model.barWorking holds, the clock, when the last status was applied, and
// barStaleAfterMs. The stale bar keeps what the last answer said, dimmed.
function barFacts(status, ctx) {
  var c = ctx && typeof ctx === "object" ? ctx : {}
  var phase = str(c.phase) || "starting"
  var failed = !!status && status.failed === true
  var summary = status && !failed && status.summary ? status.summary : null
  var targets = summary && Array.isArray(status.targets) ? status.targets : []
  var counts = barCounts(summary)
  var level = liveTargets(targets).length === 0 ? "empty"
    : counts.urgent > 0 ? "attention" : counts.updates > 0 ? "updates" : "idle"
  var age = typeof c.nowMs === "number" && num(c.answeredAtMs) > 0 ? c.nowMs - c.answeredAtMs : 0
  var state = level
  if (phase === "missing" || phase === "incompatible" || failed || (phase === "unreachable" && !summary)) state = "broken"
  else if (!summary) state = "starting"
  else if (num(c.staleAfterMs) > 0 && age > c.staleAfterMs) state = "stale"
  var shown = state === "stale" ? level : state
  var playing = ["attention", "updates", "idle"].indexOf(state) !== -1 && barRunning(targets).length > 0
  var spinning = c.working === true && state !== "broken"
  var mark = ["broken", "stale", "attention"].indexOf(state) !== -1 ? state : playing ? "running" : ""
  return {
    "state": state,
    "badge": shown === "attention" ? counts.urgent : shown === "updates" ? counts.updates : 0,
    "badgeUrgent": shown === "attention",
    "urgent": shown === "attention",
    "dimmed": ["broken", "starting", "stale", "empty"].indexOf(state) !== -1,
    "spinning": spinning,
    "playing": playing,
    "mark": mark,
    "markUrgent": BAR_URGENT_MARKS.indexOf(mark) !== -1
  }
}

// What the bar shows: barFacts with its tooltip, `{state, badge, badgeUrgent, urgent,
// dimmed, spinning, playing, mark, markUrgent, tooltip[]}`.
function barState(status, ctx) {
  var bar = barFacts(status, ctx)
  bar.tooltip = barTooltip(status, ctx)
  return bar
}

// ---------------------------------------------- failures and recovery (1n2p.34)
//
// What a player sees when omaddons cannot be used, or stopped in the middle (design
// doc §6.4): one card per kind with its words and its actions, the details a bug
// report needs, the retries, and an interrupted change announced from `status` alone.

// Retries while the binary is missing or gave no answer: 5 s, 15 s, then every 60 s,
// with the panel open or closed, until it answers.
var RETRY_BACKOFF_MS = [5000, 15000, 60000]

// `value` as a JavaScript array: a list QML hands over (a `var` property's array, a
// Process's command) is array-like, not an Array; anything else is empty.
function listOf(value) {
  if (Array.isArray(value)) return value
  if (value && typeof value === "object" && typeof value.length === "number") return Array.prototype.slice.call(value)
  return []
}

// The delay before retry number `step` (0 first), the last one repeating.
function retryDelayMs(step, delays) {
  var given = listOf(delays)
  var list = given.length ? given : RETRY_BACKOFF_MS
  return list[Math.max(0, Math.min(list.length - 1, Math.floor(num(step))))]
}

// Why another build answered (M3-D42), typed: `{kind, theirs}`. "version": another
// omaddons version, or an answer this widget cannot read at all (`theirs` "" then);
// "outdated": the same version, but the widget's files are not the ones this binary
// installs.
function incompatibleOf(env, build) {
  var mine = build && build.TOOL_VERSION ? String(build.TOOL_VERSION) : ""
  if (!envelopeOk(env) || schemaMajor(env.schema_version) !== 1) return { "kind": "version", "theirs": "" }
  if (env.tool_version !== mine) return { "kind": "version", "theirs": str(env.tool_version) }
  return { "kind": "outdated", "theirs": str(env.tool_version) }
}

// `text` with the stamped home shown as "~" (Copy details, design doc §6.5): a path of
// the player's own home says nothing more, and no other environment value is read.
function homeless(text, home) {
  var s = String(text)
  var h = typeof home === "string" && home.length > 1 ? home : ""
  return h ? s.split(h).join("~") : s
}

// One argument as a shell would need it typed: quoted when it holds a space or a quote.
function shellWord(arg) {
  var s = String(arg)
  return s !== "" && /^[A-Za-z0-9_@%+=:,.\/~-]+$/.test(s) ? s : JSON.stringify(s)
}

// A failure's details for a bug report (Copy details): what ran, how it ended and the
// child's last lines, with the panel's version. Built from the request's own argv and
// codes, never from the environment: the binary is "omaddons" and the stamped home "~".
// `f` is `{label, argv, exitCode, outcome, tail}`.
function failureDetails(f, context, widgetVersion) {
  var x = f && typeof f === "object" ? f : {}
  var home = context && typeof context === "object" ? context.HOME : ""
  var argv = listOf(x.argv).map(function(a) { return String(a) })
  if (argv.length) argv[0] = "omaddons"
  var lines = [copy("details.what", { "what": str(x.label) || copy("details.unknown") })]
  if (argv.length) lines.push(copy("details.command", { "command": argv.map(shellWord).map(function(a) { return homeless(a, home) }).join(" ") }))
  var code = typeof x.exitCode === "number" && x.exitCode >= 0 ? x.exitCode : null
  lines.push(copy("details.ended", { "how": copy("details.outcome." + (str(x.outcome) || "unknown")),
    "code": code === null ? copy("details.no_exit") : copy("details.exit", { "code": code }) }))
  lines.push(copy("details.panel", { "version": str(widgetVersion) || copy("details.unknown") }))
  var tail = listOf(x.tail).slice(-12)
  if (tail.length) {
    lines.push(copy("details.output"))
    for (var i = 0; i < tail.length; i++) lines.push("  " + homeless(tail[i], home))
  }
  return lines.join("\n")
}

// What a request that ended with no envelope says in the failure card (design doc
// §6.4): its label and what happened, in words.
function failureWords(label, outcome) {
  var id = "failure." + str(outcome)
  return copy(COPY.hasOwnProperty(id) ? id : "failure.other", { "label": str(label), "outcome": str(outcome) })
}

// The card shown in place of the views when omaddons cannot be used (design doc §6.4):
// `{kind, title, sentence, notes[], actions[], details}`. `phase` is the service's;
// `info` is `{binary, home, outcome, failure, deadlineSec, mine, incompatible,
// reinstalledAtMs, nowMs}`: the binary's path, the stamped home, the last status
// read's outcome and its failure facts (failureDetails), the local deadline, this
// panel's version, incompatibleOf's answer, and when Update the panel last ran. The
// kinds: missing, malformed, oversized, timeout, stuck (no answer), version, outdated
// (another build); null when the phase has no card. Actions: retry, reinstall,
// copy_command, copy_details.
function phaseCard(phase, info) {
  var i = info && typeof info === "object" ? info : {}
  var kind = ""
  if (phase === "missing") kind = "missing"
  else if (phase === "unreachable") {
    var outcome = str(i.outcome)
    kind = ["timeout", "oversized", "stuck"].indexOf(outcome) !== -1 ? outcome : "malformed"
  } else if (phase === "incompatible") kind = i.incompatible && i.incompatible.kind === "outdated" ? "outdated" : "version"
  if (!kind) return null
  var theirs = i.incompatible ? str(i.incompatible.theirs) : ""
  var slots = { "path": homeless(str(i.binary), i.home), "seconds": num(i.deadlineSec), "mine": str(i.mine) || copy("details.unknown"),
    "theirs": theirs || copy("card.version.unreadable") }
  var card = { "kind": kind, "title": copy("card." + kind + ".title"), "sentence": copy("card." + kind + ".sentence", slots),
    "notes": [], "actions": [], "details": "" }
  if (kind === "version" || kind === "outdated") {
    card.actions = ["reinstall", "retry"]
    if (num(i.reinstalledAtMs) > 0) {
      card.notes.push(copy("card.reinstalled", { "age": sinceText(new Date(i.reinstalledAtMs).toISOString(), i.nowMs) || copy("details.unknown") }))
    }
    return card
  }
  card.notes.push(copy("card.retrying"))
  if (kind === "missing") {
    card.actions = ["retry", "copy_command"]
    return card
  }
  card.actions = ["retry", "copy_details"]
  card.details = failureDetails(i.failure, { "HOME": i.home }, i.mine)
  return card
}

// The command a missing binary's card copies: run from the binary the player uses now,
// it installs the panel for it (`omarchy install`).
function missingBinaryCommand() {
  return "omaddons omarchy install"
}

// An interrupted change, from `status` alone (journal, each client's attention): `{clients,
// sentence}`, or null when none waits or another omaddons process is committing now.
// The widget never recovers it on its own: any change recovers first, so its silent
// checkpoints wait while this holds (Service.afterStatus), and the notice's action is
// the problem's page, whose repair previews first.
function interruptedChange(status) {
  var s = status && typeof status === "object" ? status : {}
  var journal = s.journal && typeof s.journal === "object" ? s.journal : null
  if (journal && journal.inProgress === true) return null
  var names = []
  var targets = Array.isArray(s.targets) ? s.targets : []
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    if (Array.isArray(t.attention) && t.attention.indexOf("journal_pending") !== -1) names.push(t.displayName || t.id)
  }
  var pending = journal ? num(journal.pending) : 0
  if (names.length === 0 && pending === 0) return null
  var clients = names.length ? joinNames(names) : ""
  return {
    "clients": names,
    "target": names.length === 1 ? targets.filter(function(t) { return (t.displayName || t.id) === names[0] })[0].id : "",
    "title": names.length === 0 ? copy("interrupted.title.none") : copy(names.length === 1 ? "interrupted.title.one" : "interrupted.title.many", { "clients": clients }),
    "sentence": copy("interrupted.sentence")
  }
}

// --------------------------------------------------------------- first run (1n2p.32)

// With an answer that lists no client on this computer, the welcome card (design doc
// §6.4): what omaddons looked for, and two ways on with no terminal, Scan again (a
// `status` with its scan) and Add a client folder (a page with a path box, `targets
// add`). null otherwise.
function welcomeCard(summary, targets) {
  if (!summary || liveTargets(targets).length > 0) return null
  return { "kind": "welcome", "title": copy("welcome.title"), "sentence": copy("welcome.sentence"),
    "buttons": [{ "name": "rescan", "text": copy("welcome.rescan") }, { "name": "add", "text": copy("welcome.add") }] }
}

// The keybinding the tip offers (Omarchy's own `o.bind` form in ~/.config/hypr/
// bindings.lua): a key free in Omarchy's defaults, opening this widget's panel.
var KEY_HINT_KEY = "SUPER + ALT + W"

function keyHintLine(pluginId) {
  return "o.bind(" + JSON.stringify(KEY_HINT_KEY) + ", " + JSON.stringify(copy("onboard.keyhint.name")) + ", { panel = "
    + JSON.stringify(str(pluginId)) + " })"
}

// A client that has a status line for its Lua errors and records none (no BugGrabber).
function capturesNothing(t) {
  return !!t && !!t.errors && t.errors.capture === "none"
}

// The first run's cards above the views (1n2p.32, design doc §6.4), one at a time, in
// order: take over the addons already on a client that manages none; record Lua errors
// on a client without BugGrabber, when a build is known for it (the `errors` read's own
// install command); then the keybinding tip, after the panel was first opened by mouse,
// while the `keyHint` setting is On. With no client here, the views' welcome card comes
// first and none of these shows. `input` is `{summary, targets, errors (reduceErrors),
// dismissed ({kind: [client ids]}, "Not now" for the shell's lifetime), keyHint,
// openedByMouse, pluginId}`. A card is `{kind, target, title, sentence, line, action,
// buttons}`; null when none is due.
function onboardingCard(input) {
  var i = input && typeof input === "object" ? input : {}
  if (!i.summary) return null
  var live = liveTargets(i.targets)
  if (live.length === 0) return null
  var dismissed = i.dismissed && typeof i.dismissed === "object" ? i.dismissed : {}
  var skipped = function(kind, id) { return Array.isArray(dismissed[kind]) && dismissed[kind].indexOf(id) !== -1 }
  var notNow = { "name": "not_now", "text": copy("onboard.not_now") }
  var name = function(t) { return t.displayName || t.id }
  for (var a = 0; a < live.length; a++) {
    var t = live[a]
    if (num(t.addons) === 0 && num(t.unmanaged) > 0 && !skipped("adopt", t.id)) {
      return { "kind": "adopt", "target": t.id, "title": copy("onboard.adopt.title", { "client": name(t) }),
        "sentence": copy("onboard.adopt.sentence", { "n": num(t.unmanaged) }), "line": "", "action": null,
        "buttons": [{ "name": "take_over", "text": copy("installed.take_over") }, notNow] }
    }
  }
  var read = i.errors && Array.isArray(i.errors.targets) ? i.errors.targets : []
  for (var b = 0; b < live.length; b++) {
    var c = live[b]
    if (!capturesNothing(c) || skipped("buggrabber", c.id)) continue
    var e = read.filter(function(x) { return x && x.id === c.id })[0]
    if (!e || !e.install || !e.install.runnable) continue
    return { "kind": "buggrabber", "target": c.id, "title": copy("onboard.buggrabber.title", { "client": name(c) }),
      "sentence": copy("onboard.buggrabber.sentence"), "line": "", "action": e.install,
      "buttons": [{ "name": "install", "text": copy("errors.capture.install") }, notNow] }
  }
  if (i.keyHint === true && i.openedByMouse === true && !skipped("keyhint", "")) {
    return { "kind": "keyhint", "target": "", "title": copy("onboard.keyhint.title"), "sentence": copy("onboard.keyhint.sentence"),
      "line": keyHintLine(i.pluginId), "action": null,
      "buttons": [{ "name": "copy", "text": copy("onboard.keyhint.copy") }, { "name": "not_now", "text": copy("onboard.keyhint.done") }] }
  }
  return null
}

// Whether the first run's BugGrabber card needs the `errors` read to know if a build
// exists: a client here records nothing and no read is at hand.
function needsCaptureRead(targets, errors) {
  return !errors && liveTargets(targets).some(capturesNothing)
}

// The path a player typed into the path box, made usable: surrounding blanks gone, a
// leading "~" as the stamped home; `{path, error}`, the error a sentence when it cannot
// be a client folder (not absolute, more than one line, too long). The CLI judges what
// the folder holds.
function clientPath(text, home) {
  var s = String(text === null || text === undefined ? "" : text).replace(/^\s+|\s+$/g, "")
  if (s === "") return { "path": "", "error": copy("addclient.error.empty") }
  if ((s === "~" || s.indexOf("~/") === 0) && typeof home === "string" && home.charAt(0) === "/") s = home.replace(/\/+$/, "") + s.slice(1)
  if (s.charAt(0) !== "/") return { "path": "", "error": copy("addclient.error.relative") }
  if (!PATH_PATTERN.test(s) || utf8Length(s) > PATH_MAX_BYTES) return { "path": "", "error": copy("addclient.error.bad") }
  return { "path": s, "error": "" }
}

// One kind of fact per part, urgent first (design doc §2.5): new Lua errors, problems
// that need attention, updates, then the other problems when none needs attention.
function barParts(errors, urgentIssues, issues, updates) {
  var parts = []
  if (errors > 0) parts.push(copy("bar.errors", { "n": errors }))
  if (urgentIssues > 0) parts.push(copy("bar.urgent", { "n": urgentIssues }))
  if (updates > 0) parts.push(copy("bar.updates", { "n": updates }))
  if (urgentIssues === 0 && issues > 0) parts.push(copy("bar.problems", { "n": issues }))
  return parts
}

// The click hints, the middle click's view named for what it opens now (urgentView).
function barHints(summary) {
  return copy("bar.hints", { "view": VIEW_TITLES[urgentView(summary)] })
}

// The tooltip, a line each (design doc §2.5, §6.3): the headline ("2 new Lua errors · 3
// updates", or what keeps the bar from saying), one line per client with something to
// say ("Retail · 3 updates · running") and the computer's own problems, the last
// check's age, then the click hints. Clients by their player name, never an id.
function barTooltip(status, ctx) {
  var c = ctx && typeof ctx === "object" ? ctx : {}
  var facts = barFacts(status, c)
  if (facts.state === "broken") {
    var phase = str(c.phase)
    var head = phase === "missing" ? "bar.missing" : phase === "incompatible" ? "bar.incompatible"
      : status && status.failed === true ? "bar.failed" : "bar.unreachable"
    return [copy(head), copy("bar.hints.broken")]
  }
  if (facts.state === "starting") return [copy("bar.starting"), barHints(null)]
  var summary = status.summary
  var targets = Array.isArray(status.targets) ? status.targets : []
  var live = liveTargets(targets)
  var lines = []
  if (facts.state === "stale") {
    lines.push(copy("bar.stale", { "age": sinceText(new Date(num(c.answeredAtMs)).toISOString(), c.nowMs) }))
  }
  if (live.length === 0) {
    lines.push(copy("bar.empty"))
    lines.push(barHints(summary))
    return lines
  }
  var counts = barCounts(summary)
  var head = barParts(counts.errors, counts.urgentIssues, counts.issues, counts.updates)
  lines.push(head.length ? head.join(" · ") : copy("bar.all_good"))
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    var issues = Array.isArray(t.issues) ? t.issues : []
    var urgent = issues.filter(function(issue) { return issue && issue.urgent === true }).length
    var parts = t.missing === true ? [copy("client.not_found")]
      : barParts(t.errors ? num(t.errors["new"]) : 0, urgent, issues.length, t.updates ? num(t.updates.available) : 0)
    if (t.missing !== true && t.running === true) parts.push(copy("client.running"))
    if (parts.length) lines.push(copy("bar.client", { "client": t.displayName || t.id, "facts": parts.join(" · ") }))
  }
  var machine = Array.isArray(status.issues) ? status.issues : []
  if (machine.length) {
    var machineUrgent = machine.filter(function(issue) { return issue && issue.urgent === true }).length
    lines.push(copy("bar.client", { "client": copy("issues.section.machine"),
      "facts": barParts(0, machineUrgent, machine.length, 0).join(" · ") }))
  }
  if (live.some(function(t) { return t.runningUnknown === true })) lines.push(copy("bar.running_unknown"))
  var oldest = oldestCheck(live, "")
  var since = oldest ? sinceText(oldest, c.nowMs) : ""
  if (since) lines.push(copy("bar.checked", { "age": since }))
  else if (!oldest) lines.push(copy("bar.unchecked"))
  lines.push(barHints(summary))
  return lines
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
            "latestAsset": strOrNull(latest.asset),
            // When that release was published, when its source says (1n2p.21).
            "publishedAt": strOrNull(latest.published_at)
          }
        })
      }
    })
  }
}

// `search`: result rows, each with every client's hint by id (`hints`): the Search view
// reads the scope's client by its id, never a first key (1n2p.25, bmcx.6).
function reduceSearch(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || !Array.isArray(env.data.results)) {
    return { "query": "", "rows": [], "skipped": [] }
  }
  return {
    "query": str(env.data.query),
    "rows": env.data.results.map(function(r) {
      var compat = r.compat && typeof r.compat === "object" ? r.compat : {}
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
            var seen = e.last_seen && typeof e.last_seen === "object" ? e.last_seen : {}
            return {
              "fingerprint": str(e.fingerprint),
              // How the likely source was found (`protected_call` is a blocked call).
              "method": str(attribution.method),
              "lastSeen": str(seen.at),
              "sessions": num(e.sessions),
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
          var lastSeen = a.last_seen && typeof a.last_seen === "object" ? str(a.last_seen.at) : ""
          return {
            "addon": str(a.addon),
            "managed": a.managed === true,
            "kind": str(a.kind),
            "lastSeen": lastSeen,
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
            // Why it was made (`--reason`), as given: the row quotes it.
            "reason": strOrNull(b.reason),
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

// ---------------------------------------------------------------- activity (1n2p.18)

// What the pinned activity bar says while something the player waits on runs (design
// doc §2.1, §6.4). Each lane's running request may have one activity:
// `{kind, lane, id, resource, template, label, op, preview, mode, phase, current, total,
// target, ref, startedAt, lastEventAt, events, cancellable, rows}`.
// - `kind`: `change` (a change the player asked for), `check` (an update check, asked or
//   automatic), `read` (a read the player asked for that asks the sources: a search,
//   release notes). Background work the player did not ask for (settings checkpoints,
//   Lua error reads, `status`) has none (M3-D11).
// - `phase`, `current`, `total`, `target`, `ref`: the last progress event. What an event
//   is about is its typed `target` and `ref`, never its free-text `message` (UX-32).
// - `rows`: the rows a change moves, by key (`<target id>/<addon>`): `{target, addon,
//   ref, phase, current, total}`, `phase` "" until an event reaches the row.

// A run quiet this long says it is still working; one this long says how long it has
// run; a row that changed says so this long; the last result stays this long, or until
// the next change (design doc §2.1).
var ACTIVITY_STALL_MS = 30000
var ACTIVITY_ELAPSED_MS = 5000
var ACTIVITY_ROW_DONE_MS = 4000
var RESULT_HOLD_MS = 600000

var PROGRESS_PHASES = ["fetch", "download", "verify", "stage", "commit", "copy"]

// What a change does, by its template (a change without a plan to name it).
var TEMPLATE_OPS = {
  "update_all": "update", "update_target": "update", "update_addon": "update", "install": "install",
  "reinstall": "reinstall", "remove": "remove", "pin": "pin", "unpin": "unpin", "dismiss_errors": "dismiss",
  "backup_target": "backup", "backup_all": "backup", "restore": "restore", "restore_addon": "restore",
  "prune_backup": "prune", "doctor_fix": "repair", "doctor_fix_target": "repair"
}

// The changes whose own work is copying settings: their `copy` phase is the job, never
// the day's checkpoint taken before a change.
var SETTINGS_OPS = ["backup", "restore"]

// The kind of activity request `r` shows, or "" for one the player does not wait on.
function activityKind(r) {
  if (!r || typeof r !== "object") return ""
  if (r.kind === "mutation") return r.priority === "background" ? "" : "change"
  if (r.resource === "check" || r.resource === "cli:outdated") return "check"
  if (laneOf(r) === "network" && r.priority !== "background") return "read"
  return ""
}

// The operation of change `r`: its plan's, when every item of the plan names the same
// one, else its template's, else its command path's (an argv from the CLI: an undo).
function activityOp(r) {
  var selection = r.plan && r.plan.approval && Array.isArray(r.plan.approval.selection) ? r.plan.approval.selection : []
  var ops = []
  for (var i = 0; i < selection.length; i++) {
    var op = selection[i] ? str(selection[i].op) : ""
    if (op && ops.indexOf(op) === -1) ops.push(op)
  }
  if (ops.length === 1) return ops[0]
  if (TEMPLATE_OPS.hasOwnProperty(str(r.template))) return TEMPLATE_OPS[r.template]
  var spec = r.spec && typeof r.spec === "object" ? r.spec : {}
  if (TEMPLATE_OPS.hasOwnProperty(str(spec.template))) return TEMPLATE_OPS[spec.template]
  var argv = Array.isArray(r.cli) ? r.cli : (Array.isArray(spec.cli) ? spec.cli : null)
  return argv ? verdictPath(argv).split(" ")[0] : ""
}

// The ref `list` records for `addon` on `target` (ignoring ASCII case), or "".
function listedRef(lists, target, addon) {
  var ts = lists && Array.isArray(lists.targets) ? lists.targets : []
  var wanted = asciiLower(str(addon))
  for (var i = 0; i < ts.length; i++) {
    if (!ts[i] || ts[i].id !== target || !Array.isArray(ts[i].addons)) continue
    for (var j = 0; j < ts[i].addons.length; j++) {
      var a = ts[i].addons[j]
      if (a && asciiLower(str(a.name)) === wanted) return str(a.ref)
    }
  }
  return ""
}

// The rows change `r` moves: its plan's items, else the one addon its slots name. A
// preview moves none: it only asks what it would do.
function activityRows(r, lists) {
  var rows = {}
  if (r.preview === true) return rows
  var add = function(target, addon) {
    if (!target || !addon) return
    rows[target + "/" + addon] = { "target": target, "addon": addon, "ref": listedRef(lists, target, addon),
      "phase": "", "current": 0, "total": 0 }
  }
  var selection = r.plan && r.plan.approval && Array.isArray(r.plan.approval.selection) ? r.plan.approval.selection : []
  for (var i = 0; i < selection.length; i++) {
    if (selection[i]) add(str(selection[i].target_id), str(selection[i].addon))
  }
  var slots = r.slots && typeof r.slots === "object" ? r.slots : {}
  if (selection.length === 0 && TEMPLATE_OPS.hasOwnProperty(str(r.template))) add(str(slots.target), str(slots.addon))
  return rows
}

// The activity of request `r` as it starts on its lane at `nowMs`, or null when the
// player does not wait on it. `lists` (the service's `list -v`) gives each row its ref.
function activityStart(r, nowMs, lists) {
  var kind = activityKind(r)
  if (!kind) return null
  return {
    "kind": kind, "lane": laneOf(r), "id": num(r.id), "resource": str(r.resource), "template": str(r.template),
    "label": str(r.label), "op": kind === "change" ? activityOp(r) : "", "preview": r.preview === true,
    "mode": str(r.mode), "phase": "", "current": 0, "total": 0, "target": "", "ref": "",
    "startedAt": num(nowMs), "lastEventAt": num(nowMs), "events": 0,
    // A read can be stopped; a change in flight never is (it ends as a transaction).
    "cancellable": kind !== "change",
    "rows": kind === "change" ? activityRows(r, lists) : {}
  }
}

// Whether a row's ref is the event's: GitHub refs compare without case, as the CLI's
// do; every other ref is compared the same way, a near miss being harmless here.
function sameRef(a, b) {
  return a !== "" && b !== "" && asciiLower(a) === asciiLower(b)
}

// Activity `a` after one NDJSON line's `event` at `nowMs`: its phase and counts, what it
// is about, and the rows the event names (a client's rows for a stage or commit, a
// ref's rows for a fetch, the one row of a ref on a client for a download or verify).
// Anything but a progress event leaves it as it was.
function activityProgress(a, event, nowMs) {
  if (!a || !event || typeof event !== "object" || event.event !== "progress") return a
  var phase = PROGRESS_PHASES.indexOf(event.phase) !== -1 ? event.phase : ""
  var next = {}
  for (var key in a) if (a.hasOwnProperty(key)) next[key] = a[key]
  next.phase = phase
  next.current = num(event.current)
  next.total = num(event.total)
  next.target = str(event.target)
  next.ref = str(event.ref)
  next.lastEventAt = num(nowMs)
  next.events = num(a.events) + 1
  var rows = {}
  for (var k in a.rows) {
    if (!a.rows.hasOwnProperty(k)) continue
    var row = a.rows[k]
    var named = (next.target !== "" || next.ref !== "") && (next.target === "" || row.target === next.target)
      && (next.ref === "" || sameRef(row.ref, next.ref))
    rows[k] = !named ? row : { "target": row.target, "addon": row.addon, "ref": row.ref, "phase": phase,
      "current": next.current, "total": next.total }
  }
  next.rows = rows
  return next
}

// The changes whose commit puts addons in place: theirs is "installing", every other
// change's "finishing" (a remove installs nothing).
var INSTALLING_OPS = ["update", "install", "reinstall"]

// A phase in words (design doc §6.4). `copy` during a change is the day's settings
// checkpoint taken before it; in a backup or restore it is the job.
function phaseWord(phase, op) {
  if (PROGRESS_PHASES.indexOf(phase) === -1) return ""
  if (phase === "copy" && SETTINGS_OPS.indexOf(op) === -1) return copy("activity.phase.checkpoint")
  if (phase === "commit" && INSTALLING_OPS.indexOf(op) === -1) return copy("activity.phase.commit.other")
  return copy("activity.phase." + phase)
}

// " (2 of 5)", or "" without a count (an announcement counts nothing yet).
function countText(current, total) {
  return num(total) > 0 && num(current) > 0 ? " " + copy("activity.count", { "current": current, "total": total }) : ""
}

// "12 s", "1 min 5 s".
function durationText(ms) {
  var s = Math.max(0, Math.floor(num(ms) / 1000))
  return s < 60 ? copy("activity.seconds", { "s": s }) : copy("activity.minutes", { "m": Math.floor(s / 60), "s": s % 60 })
}

// The keys of a change's rows, in order.
function activityRowKeys(a) {
  return a && a.rows ? Object.keys(a.rows).sort() : []
}

// The clients a change's rows are on, by id, in order.
function activityClients(a) {
  var out = []
  var keys = activityRowKeys(a)
  for (var i = 0; i < keys.length; i++) if (out.indexOf(a.rows[keys[i]].target) === -1) out.push(a.rows[keys[i]].target)
  return out
}

// The activity bar's first words: what runs, by name.
function activityHeadline(a, targets) {
  if (a.kind === "check") return copy("activity.check")
  if (a.kind === "read") {
    var read = a.resource === "search" ? "activity.search" : (a.resource === "changelog" ? "activity.changelog"
      : (a.resource === "addon_info" ? "activity.addon_info" : ""))
    return read ? copy(read) : a.label
  }
  var keys = activityRowKeys(a)
  var verb = a.op && COPY.hasOwnProperty("activity.verb." + a.op) ? copy("activity.verb." + a.op) : ""
  if (a.preview || !verb || keys.length === 0) return a.label
  var clients = activityClients(a)
  var client = clients.length === 1 ? sheetClient(targets, clients[0], "") : ""
  if (keys.length === 1) return copy("activity.change.one", { "verb": verb, "addon": a.rows[keys[0]].addon, "client": client })
  if (clients.length === 1) return copy("activity.change.client", { "verb": verb, "n": keys.length, "client": client })
  return copy("activity.change.clients", { "verb": verb, "n": keys.length, "m": clients.length })
}

// The activity bar's detail: the last event in words ("DBM, downloading (2 of 3)"), or
// what a preview is doing before any; "" when there is nothing to add.
function activityDetail(a, targets) {
  if (a.events === 0) {
    if (!a.preview) return ""
    return copy(a.mode === "match" ? "activity.preview.match" : (a.mode === "bind" ? "activity.preview.bind" : "activity.preview.confirm"))
  }
  var word = phaseWord(a.phase, a.op)
  if (!word) return ""
  var count = countText(a.current, a.total)
  if (a.kind !== "change") return word + count
  // The one row the event names, else the client it names when the change has several.
  var named = []
  var keys = activityRowKeys(a)
  if (a.ref !== "") {
    for (var i = 0; i < keys.length; i++) {
      var row = a.rows[keys[i]]
      if (sameRef(row.ref, a.ref) && (a.target === "" || row.target === a.target)) named.push(row)
    }
  }
  var subject = named.length === 1 ? named[0].addon
    : (a.target !== "" && activityClients(a).length > 1 ? sheetClient(targets, a.target, "") : "")
  return (subject ? subject + ", " : "") + word + count
}

// What the activity bar shows for activity `a` at `nowMs`, or null when nothing runs:
// `{kind, text, fraction, elapsed, stalled, cancellable}`. A check and a read say only
// their count ("Checking for updates (12 of 42)"); a change says what it changes and
// the last step ("Updating 3 addons on Retail: DBM, downloading (2 of 3)"). `fraction`
// is -1 without a count (indeterminate). `elapsed` is how long it has run, after 5 s;
// `stalled` says it is still working after 30 s without news. A count reaching its
// total is not the end: the answer is (cached downloads report nothing).
function activityView(a, targets, nowMs) {
  if (!a || typeof a !== "object") return null
  var headline = activityHeadline(a, targets)
  var detail = activityDetail(a, targets)
  var text = a.kind !== "change"
    ? headline + (a.events > 0 && num(a.total) > 0 && num(a.current) > 0 ? countText(a.current, a.total) : "…")
    : headline + (detail ? ": " + detail : "…")
  var now = num(nowMs)
  var ran = now - num(a.startedAt)
  var quiet = now - num(a.lastEventAt)
  return {
    "kind": a.kind,
    "text": text,
    "fraction": num(a.total) > 0 ? Math.max(0, Math.min(1, num(a.current) / num(a.total))) : -1,
    "elapsed": ran >= ACTIVITY_ELAPSED_MS ? durationText(ran) : "",
    "stalled": quiet >= ACTIVITY_STALL_MS ? copy("activity.stalled", { "time": durationText(quiet) }) : "",
    "cancellable": a.cancellable === true
  }
}

// The activity the bar shows of the lanes' `{mutation, network, local}`: a change
// first, then a check or read; null when the player waits on nothing.
function shownActivity(lanes) {
  var l = lanes && typeof lanes === "object" ? lanes : {}
  return l.mutation || l.network || l.local || null
}

// What a row says while a change runs on it, or just after: `{pending, text}`, or null
// when it is neither. `done` is the service's `{<key>: word}` of rows that changed.
function activityRow(a, key, done) {
  var k = str(key)
  if (a && a.kind === "change" && a.rows && a.rows.hasOwnProperty(k)) {
    var row = a.rows[k]
    var verb = a.op && COPY.hasOwnProperty("activity.verb." + a.op) ? copy("activity.verb." + a.op) : copy("activity.verb.generic")
    var word = phaseWord(row.phase, a.op)
    return { "pending": true, "text": verb + "…" + (word ? " " + word + countText(row.current, row.total) : "") }
  }
  if (done && typeof done === "object" && done.hasOwnProperty(k)) return { "pending": false, "text": str(done[k]) }
  return null
}

// The rows a finished change changed, `{<key>: word}` ("Updated"), from its envelope
// (an `apply` reads as the command it ran): every item that changed, by its client.
function changedRows(env) {
  var view = applyResult(env) || env
  var out = {}
  if (!envelopeOk(view) || !view.data || typeof view.data !== "object") return out
  var entries = Array.isArray(view.data.targets) ? view.data.targets : [view.data]
  for (var i = 0; i < entries.length; i++) {
    var entry = entries[i]
    if (!entry || typeof entry !== "object" || !Array.isArray(entry.items)) continue
    var t = entry.target
    var id = t && typeof t === "object" ? str(t.id) : str(t)
    for (var j = 0; j < entry.items.length; j++) {
      var item = entry.items[j]
      if (!id || !isChange(item) || !str(item.name)) continue
      var word = "activity.done." + str(item.status)
      out[id + "/" + item.name] = COPY.hasOwnProperty(word) ? copy(word) : copy("activity.done.generic")
    }
  }
  return out
}

// "Next: Update Retail": the change that waits for this one, else a check that waits
// for the network lane; "" when nothing waits.
function queuedText(queues) {
  var q = queues && typeof queues === "object" ? queues : {}
  var mutation = Array.isArray(q.mutation) ? q.mutation : []
  for (var i = 0; i < mutation.length; i++) {
    if (mutation[i] && mutation[i].priority === "user") return copy("activity.next", { "label": str(mutation[i].label) })
  }
  var network = Array.isArray(q.network) ? q.network : []
  for (var j = 0; j < network.length; j++) {
    if (network[j] && network[j].resource === "check") return copy("activity.next", { "label": copy("activity.check") })
  }
  return ""
}

// Whether the bar spins (design doc §2.5, 1n2p.16's `working`): a change the player
// asked for runs, or a check runs or waits to.
function barWorking(lanes, queues) {
  var l = lanes && typeof lanes === "object" ? lanes : {}
  if (l.mutation && l.mutation.kind === "change") return true
  if (l.network && l.network.kind === "check") return true
  var network = queues && Array.isArray(queues.network) ? queues.network : []
  for (var i = 0; i < network.length; i++) if (network[i] && network[i].resource === "check") return true
  return false
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
  // A setting the settings sheet changed (1n2p.31): in the sheet's words.
  if (command === "config set" && /^targets\.[^.]+\.name$/.test(str(data.name))) {
    return str(data.value) === "" ? copy("settings.result.name.builtin") : copy("settings.result.name", { "value": str(data.value) })
  }
  if (command === "config set" && /^targets\.[^.]+\.family$/.test(str(data.name))) {
    var family = "settings.family." + str(data.value)
    return copy("settings.result.family", { "value": COPY.hasOwnProperty(family) ? copy(family) : str(data.value) })
  }
  if (command === "config set") {
    var key = str(data.name)
    var known = OMADDONS_SETTINGS.some(function(s) { return s.key === key })
    var setting = known ? copy("settings.omaddons." + key) : key
    var value = known ? configText(key, data.value) : str(data.value)
    return copy(data.changed === false ? "settings.result.unchanged" : "settings.result", { "setting": setting, "value": value })
  }
  // A client added by its folder (1n2p.32): its player name, as `targets add` gives it.
  if (command === "targets add") {
    if (data.changed === false) return copy("addclient.result.none")
    var added = Array.isArray(data.targets) ? data.targets.map(function(t) { return str(t.display_name) || str(t.id) })
      .filter(function(n) { return n !== "" }) : []
    return added.length ? copy("addclient.result", { "clients": joinNames(added) }) : copy("addclient.result.none")
  }
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
  var ms = instantMs(s)
  if (isNaN(ms)) return s
  var at = new Date(ms)
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

// ------------------------------------------------------------------ History (1n2p.30)

// How many changes History reads, as COMMANDS.history asks for them: `-v` lists every
// addon a change touched, so 100 lines stay under WIDGET_OUTPUT_MAX_BYTES at worst
// (tests/omarchy_widget.rs, the output cap), where 200 would not.
var HISTORY_LIMIT = 100
// The kinds undo never reverts (README `undo`): a backup, and a settings change.
var HISTORY_FIXED = ["backup", "config", "target", "auth", "export", "dismiss", "widget", "skill"]
// The kinds whose lines name the addons they changed, and so a title that counts them.
var HISTORY_ADDON_KINDS = ["install", "update", "remove", "adopt", "sync"]
// History's glyphs (§5.4): a change, and a change an agent made.
var HISTORY_GLYPH = 0xF02DA
var AGENT_GLYPH = 0xF06A9

function historyRef(t) {
  return t && typeof t === "object" ? { "id": str(t.id), "key": str(t.key) } : null
}

// `history -v --limit 200`'s envelope as History's model: `{failed, entries, truncated,
// skipped}`, each entry one transaction as `history` gives it, newest first. `skipped`:
// lines `history` could not read (`history_lines_skipped`), which it leaves out.
function reduceHistory(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || typeof env.data !== "object") {
    return { "failed": !!env && typeof env === "object", "entries": [], "truncated": false, "skipped": false }
  }
  var lines = Array.isArray(env.data.entries) ? env.data.entries : []
  var entries = []
  for (var i = 0; i < lines.length; i++) {
    var l = lines[i]
    if (!l || typeof l !== "object" || str(l.tx) === "") continue
    var refs = [historyRef(l.target)].concat(Array.isArray(l.targets) ? l.targets.map(historyRef) : [])
    // A restore's backup: the one its `wtf` or `saved_variables` op put files back from.
    var ops = Array.isArray(l.ops) ? l.ops : []
    var restored = ""
    for (var o = 0; o < ops.length && restored === ""; o++) if (ops[o] && typeof ops[o].backup === "string") restored = ops[o].backup
    entries.push({
      "restoredFrom": restored,
      "tx": str(l.tx), "run": strOrNull(l.run), "kind": str(l.kind), "at": str(l.at), "actor": str(l.actor),
      "reason": strOrNull(l.reason), "reverts": strOrNull(l.reverts), "plan": strOrNull(l.plan),
      "targets": refs.filter(function(t) { return !!t && t.id !== "" }),
      "items": (Array.isArray(l.items) ? l.items : []).filter(function(item) { return item && typeof item === "object" }).map(function(item) {
        return { "addon": strOrNull(item.addon), "change": str(item.change), "folders": num(item.folders),
          "from": strOrNull(item.from_version), "to": strOrNull(item.to_version) }
      }),
      "backup": l.backup && typeof l.backup === "object" ? { "id": str(l.backup.id), "kind": str(l.backup.kind) } : null
    })
  }
  var codes = Array.isArray(env.warnings) ? env.warnings.map(function(w) { return w && typeof w === "object" ? str(w.code) : "" }) : []
  return { "failed": false, "entries": entries, "truncated": env.data.truncated === true,
    "skipped": codes.indexOf("history_lines_skipped") !== -1 }
}

// Whether each transaction's change stands, by its id: no undo reverts it, or every undo
// that does was undone in turn (a redo). An undo outside the lines read counts as none.
function historyStanding(entries) {
  var revertedBy = {}
  for (var i = 0; i < entries.length; i++) {
    var e = entries[i]
    if (e.kind !== "undo" || !e.reverts) continue
    if (!revertedBy.hasOwnProperty(e.reverts)) revertedBy[e.reverts] = []
    revertedBy[e.reverts].push(e.tx)
  }
  var memo = {}
  var stands = function(tx, depth) {
    if (memo.hasOwnProperty(tx)) return memo[tx]
    // A cycle never happens (an undo reverts an older line); bound it all the same.
    if (depth > entries.length) return true
    var undos = revertedBy.hasOwnProperty(tx) ? revertedBy[tx] : []
    var result = !undos.some(function(u) { return stands(u, depth + 1) })
    memo[tx] = result
    return result
  }
  var out = {}
  for (var j = 0; j < entries.length; j++) out[entries[j].tx] = stands(entries[j].tx, 0)
  return out
}

// An addon's change as one phrase: "DBM 12.1.14 → 12.1.15", "Bar 2.1.0", "DBM"; a
// whole `WTF/` restore names no addon: "every setting".
function historyItemText(item) {
  var versions = item.from && item.to && item.from !== item.to ? item.from + " → " + item.to : (item.to || item.from || "")
  if (!item.addon) return copy("history.item.settings")
  return versions ? item.addon + " " + versions : item.addon
}

// What a run did, in one sentence (design doc §2.3): "Updated DBM", "Removed 2 addons",
// "Undid the update", "Backed up settings".
function historyTitle(kind, items, revertedKind) {
  var addons = []
  for (var i = 0; i < items.length; i++) if (items[i].addon && addons.indexOf(items[i].addon) === -1) addons.push(items[i].addon)
  var slots = { "n": addons.length, "addon": addons.length === 1 ? addons[0] : "", "owner": addons.length === 1 ? possessive(addons[0]) : "" }
  if (kind === "undo") {
    return revertedKind ? copy("history.title.undo", { "change": COPY.hasOwnProperty("sheet.change." + revertedKind)
      ? copy("sheet.change." + revertedKind) : copy("sheet.change.other") }) : copy("history.title.undo.unknown")
  }
  if (kind === "restore") return addons.length ? copy("history.title.restore.addons", slots) : copy("history.title.restore")
  if (kind === "pin") {
    var unpinned = items.some(function(item) { return item.change === "unpinned" }) && !items.some(function(item) { return item.change === "pinned" })
    return addons.length ? copy(unpinned ? "history.title.unpin" : "history.title.pin", slots) : copy("history.title.pin.none")
  }
  if (kind === "update" && items.length > 0 && items.every(function(item) { return item.change === "reinstalled" })) {
    return copy("history.title.reinstall", slots)
  }
  if (HISTORY_ADDON_KINDS.indexOf(kind) !== -1) {
    return addons.length ? copy("history.title." + kind, slots) : copy("history.title." + kind + ".none")
  }
  return COPY.hasOwnProperty("history.title." + kind) ? copy("history.title." + kind) : copy("history.title.other")
}

// Why a change was made, quoted, when someone said: an agent's reason is often the only
// explanation the player gets (bead Considerations). The panel's own is the label of
// the button pressed ("Update all (bar widget)"), which says no more than "you, in the
// panel" does, so it is left out.
function historyReason(run) {
  if (!run.reason || run.actor === "widget") return ""
  return copy("history.reason", { "reason": run.reason })
}

// Who made a change, in a player's words (§6.3).
function historyActor(actor) {
  return COPY.hasOwnProperty("history.actor." + actor) ? copy("history.actor." + actor) : str(actor)
}

// History's runs, newest first: the transactions of one command run together (a line
// from before runs, alone). Each is `{key, run, tx, kind, at, actor, reason, plan,
// targets, clients, items, title, state, why}`: `state` is `undoable`, `redo` (an undo
// that stands, undone in turn by undoing it), `undone` (nothing of it stands), `fixed`
// (a backup or a settings change, which undo never reverts) or `old` (no run to name).
// The widget never pre-judges an undo further: a later change on the same folders, or
// a pruned snapshot, is the CLI's preview to say (bead Considerations).
function historyRuns(history, statusTargets) {
  var entries = history && Array.isArray(history.entries) ? history.entries : []
  var stands = historyStanding(entries)
  var byTx = {}
  for (var e = 0; e < entries.length; e++) byTx[entries[e].tx] = entries[e]
  var groups = {}
  var order = []
  for (var i = 0; i < entries.length; i++) {
    var key = entries[i].run ? "run:" + entries[i].run : "tx:" + entries[i].tx
    if (!groups.hasOwnProperty(key)) {
      groups[key] = []
      order.push(key)
    }
    groups[key].push(entries[i])
  }
  return order.map(function(key) {
    var lines = groups[key]
    // The change itself: its newest line that is no automatic backup before it.
    var main = lines.filter(function(l) { return l.kind !== "backup" })[0] || lines[0]
    var items = []
    lines.forEach(function(l) { l.items.forEach(function(item) { items.push(item) }) })
    var targets = []
    var clients = []
    lines.forEach(function(l) {
      l.targets.forEach(function(t) {
        if (targets.indexOf(t.id) !== -1) return
        targets.push(t.id)
        clients.push(sheetClient(statusTargets, t.id, t.key))
      })
    })
    var undoable = lines.filter(function(l) { return HISTORY_FIXED.indexOf(l.kind) === -1 })
    var standing = undoable.filter(function(l) { return stands[l.tx] === true })
    var state = !undoable.length ? "fixed" : !main.run ? "old" : !standing.length ? "undone" : main.kind === "undo" ? "redo" : "undoable"
    var reverted = main.kind === "undo" && main.reverts && byTx.hasOwnProperty(main.reverts) ? byTx[main.reverts].kind : ""
    var why = ""
    if (state === "fixed") why = copy(main.kind === "backup" ? "history.why.backup" : main.kind === "dismiss" ? "history.why.dismiss" : "history.why.settings")
    else if (state === "old") why = copy("history.why.old")
    else if (state === "undone") why = copy("history.why.undone")
    return {
      "key": key, "run": main.run, "tx": main.tx, "kind": main.kind, "at": main.at, "actor": main.actor,
      "reason": main.reason, "plan": main.plan, "targets": targets, "clients": clients, "items": items,
      "title": historyTitle(main.kind, items, reverted), "state": state, "why": why,
      "restoredFrom": main.kind === "restore" ? str(main.restoredFrom) : ""
    }
  })
}

// Local midnight of an instant, as a day number for comparing days; NaN when it does not
// parse.
function localDay(ms) {
  var d = new Date(ms)
  return Math.round(new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime() / 86400000)
}

function wordAt(id, index) {
  var words = copy(id).split(" ")
  return index >= 0 && index < words.length ? words[index] : ""
}

// A day as a heading (design doc §6.9): "Today", "Yesterday", "Monday" within the week,
// else "9 November", with the year when it is not this year's.
function dayText(iso, nowMs) {
  var at = instantMs(iso)
  if (isNaN(at) || typeof nowMs !== "number") return ""
  var days = localDay(nowMs) - localDay(at)
  var d = new Date(at)
  if (days <= 0) return copy("time.day.today")
  if (days === 1) return copy("time.day.yesterday")
  if (days < 7) return wordAt("time.weekdays", d.getDay())
  var date = copy("time.day.date", { "d": d.getDate(), "month": wordAt("time.months", d.getMonth()) })
  return d.getFullYear() === new Date(nowMs).getFullYear() ? date : date + " " + d.getFullYear()
}

// An instant in a player's words (design doc §6.9): "just now", "25 min ago", "3 h ago"
// the same day, "yesterday 21:40", "Mon 18:02" within the week, then "9 Nov", with the
// year when it differs.
function whenText(iso, nowMs) {
  var at = instantMs(iso)
  if (isNaN(at) || typeof nowMs !== "number") return ""
  var minutes = Math.max(0, Math.floor((nowMs - at) / 60000))
  var days = localDay(nowMs) - localDay(at)
  var d = new Date(at)
  if (minutes < 1) return copy("time.just_now")
  if (minutes < 60) return copy("time.minutes", { "n": minutes })
  if (days <= 0) return copy("time.hours", { "n": Math.floor(minutes / 60) })
  if (days === 1) return copy("time.yesterday", { "time": clockText(iso) })
  if (days < 7) return copy("time.weekday", { "day": wordAt("time.weekdays.short", d.getDay()), "time": clockText(iso) })
  var date = copy("time.day.date", { "d": d.getDate(), "month": wordAt("time.months.short", d.getMonth()) })
  return d.getFullYear() === new Date(nowMs).getFullYear() ? date : date + " " + d.getFullYear()
}

// The undo of a run as a request for Service.run: the whole run, or (with `target`) its
// transaction on that client alone; null for a run nothing of which can be undone.
function historyUndo(run, target) {
  if (!run || !run.run || (run.state !== "undoable" && run.state !== "redo")) return null
  var title = run.title.length > 90 ? run.title.slice(0, 89) + "…" : run.title
  var label = copy(run.state === "redo" ? "history.redo.label" : "history.undo.label", { "title": title })
  if (target) {
    return { "template": "undo_run_target", "slots": { "run": run.run, "target": str(target) }, "label": label }
  }
  return { "template": "undo_run", "slots": { "run": run.run }, "label": label }
}

// History's rows (design doc §2.3), newest first, in the client filter: each run that
// touched a filtered client, matching `view.query` (filterMatches over what it did, the
// addons, the clients, who and why). A day's first row carries its heading (`group`);
// under All, each row carries its clients as chips (§2.2: History interleaves them).
// `view` is `{query, nowMs}`.
function historyRows(history, filter, statusTargets, view) {
  var options = view && typeof view === "object" ? view : {}
  var query = str(options.query).trim()
  var nowMs = typeof options.nowMs === "number" ? options.nowMs : Date.now()
  var runs = historyRuns(history, statusTargets).filter(function(r) { return !filter || r.targets.indexOf(filter) !== -1 })
  var rows = []
  var lastDay = ""
  for (var i = 0; i < runs.length; i++) {
    var r = runs[i]
    var actor = historyActor(r.actor)
    var addons = r.items.map(function(item) { return str(item.addon) })
    if (query !== "" && !filterMatches(query, [r.title, actor, str(r.reason)].concat(r.clients, addons))) continue
    var chips = filter ? [] : r.clients.map(function(c) { return { "text": c, "urgent": false } })
    if (r.state === "undone") chips.push({ "text": copy("history.chip.undone"), "urgent": false })
    // Each change once: an update of two clients names each addon once.
    var texts = []
    r.items.forEach(function(item) {
      var text = historyItemText(item)
      if (texts.indexOf(text) === -1) texts.push(text)
    })
    var shown = texts.slice(0, 2)
    if (texts.length > 2) shown.push(copy("history.item.more", { "n": texts.length - 2 }))
    var meta = [clockText(r.at), actor]
    var reason = historyReason(r)
    if (reason) meta.push(reason)
    var undo = historyUndo(r, "")
    var day = dayText(r.at, nowMs)
    var row = {}
    for (var k in r) if (r.hasOwnProperty(k)) row[k] = r[k]
    row.chips = chips
    row.detail = shown.join(" · ")
    row.meta = meta.filter(function(m) { return m !== "" }).join(" · ")
    row.band = ""
    row.glyph = r.actor === "agent" ? AGENT_GLYPH : HISTORY_GLYPH
    row.actorWords = actor
    row.undo = undo
    row.actions = undo ? [{ "name": "undo", "glyph": String.fromCodePoint(0xF054C),
      "tooltip": withKey(copy(r.state === "redo" ? "history.action.redo" : "history.action.undo"), "undo") }] : []
    row.group = day !== lastDay ? day : ""
    row.groupCount = ""
    lastDay = day
    rows.push(row)
  }
  return rows
}

// The filter box's "2 of 42" over History's runs in the client filter.
function historyCount(history, filter, statusTargets, query, nowMs) {
  var total = historyRuns(history, statusTargets).filter(function(r) { return !filter || r.targets.indexOf(filter) !== -1 }).length
  var shown = str(query).trim() === "" ? total : historyRows(history, filter, statusTargets, { "query": query, "nowMs": nowMs }).length
  return { "shown": shown, "total": total, "text": copy("installed.count", { "shown": shown, "total": total }), "none": total > 0 && shown === 0 }
}

// A change's page (design doc §2.4, Enter in History): what it did, when and by whom,
// why when a reason was given, the plan it applied, then each client's addons, one line
// each (`lines`, `{kind: "client"|"item", text, addon, target}`, the items cursorable
// for their addon's page), and its undo: the whole run, and with several clients each
// client's alone; or why it cannot be undone. Null when `key` names no run read.
function historyPage(history, key, statusTargets, nowMs) {
  var runs = historyRuns(history, statusTargets)
  var run = null
  for (var i = 0; i < runs.length; i++) if (runs[i].key === key) run = runs[i]
  if (!run) return null
  var entries = history && Array.isArray(history.entries) ? history.entries : []
  var lines = []
  var items = []
  run.targets.forEach(function(id, at) {
    var mine = entries.filter(function(e) {
      return (run.run ? e.run === run.run : e.tx === run.tx) && e.targets.some(function(t) { return t.id === id })
    })
    var changed = []
    mine.forEach(function(e) { e.items.forEach(function(item) { changed.push(item) }) })
    if (run.targets.length > 1 || changed.length === 0) lines.push({ "kind": "client", "text": run.clients[at], "addon": "", "target": id })
    changed.forEach(function(item) {
      // A whole `WTF/` names no addon: its own words ("Restored every setting").
      var id2 = (item.addon ? "history.change." : "history.change.whole.") + item.change
      var text = COPY.hasOwnProperty(id2) ? copy(id2, { "item": historyItemText(item) }) : historyItemText(item)
      var line = { "kind": "item", "text": text, "addon": str(item.addon), "target": id, "index": items.length }
      items.push(line)
      lines.push(line)
    })
  })
  var undos = []
  var whole = historyUndo(run, "")
  if (whole) {
    undos.push({ "name": "undo", "text": copy(run.targets.length > 1 ? (run.state === "redo" ? "history.page.redo_all" : "history.page.undo_all")
      : (run.state === "redo" ? "history.action.redo" : "history.action.undo")), "request": whole })
    if (run.targets.length > 1) {
      run.targets.forEach(function(id, at) {
        undos.push({ "name": "undo_" + id, "text": copy(run.state === "redo" ? "history.page.redo_on" : "history.page.undo_on",
          { "client": run.clients[at] }), "request": historyUndo(run, id) })
      })
    }
  }
  var header = [run.title, whenText(run.at, nowMs), historyActor(run.actor)].filter(function(p) { return p !== "" }).join(" · ")
  var notes = []
  var reason = historyReason(run)
  if (reason) notes.push(reason)
  if (run.plan) notes.push(copy("history.page.plan"))
  // A restore's backup, which its page's last line links to (the Backups view).
  var backup = run.restoredFrom ? { "id": run.restoredFrom, "target": run.targets.length ? run.targets[0] : "" } : null
  return { "key": run.key, "run": run, "title": run.title, "header": header, "notes": notes, "lines": lines, "items": items,
    "undos": undos, "why": run.why, "state": run.state, "backup": backup }
}

// -------------------------------------------------------------- the panel frame (F4)

// The panel's views, in tab order (design doc §2.1: Errors and History join with
// their views, .27 and .30): h/l step through them, the digits jump. The ids stay
// what IPC's `show <view>` takes; "installed" is titled Addons (§2.1, decided).
var VIEWS = ["updates", "installed", "search", "errors", "issues", "backups", "history"]
var VIEW_TITLES = { "updates": "Updates", "installed": "Addons", "search": "Search", "errors": "Errors", "issues": "Issues", "backups": "Backups",
  "history": "History" }
// A list longer than this builds only the rows in view (ui/VirtualList, design doc §5.5,
// 1n2p.38): Updates, Addons, Search and History.
var LONG_LIST_ROWS = 50
// Each view's glyph (§5.4), which the tabs show alone below 480 px (§2.1).
var VIEW_GLYPHS = { "updates": 0xF0CE1, "installed": 0xF0431, "search": 0xF0349, "errors": 0xF00E4, "issues": 0xF0026, "backups": 0xF003C,
  "history": 0xF02DA }

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
  { "id": "errors", "title": "Errors" },
  { "id": "issues", "title": "Issues" },
  { "id": "backups", "title": "Backups" },
  { "id": "history", "title": "History" },
  { "id": "page", "title": "A page" },
  { "id": "field", "title": "Search box" },
  { "id": "filter", "title": "Filter box" },
  { "id": "pathbox", "title": "Path box" },
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
  { "keys": ["1", "2", "3", "4", "5", "6", "7"], "context": "panel", "action": "view", "label": "view 1 to 7: Updates, Addons, Search, Errors, Issues, Backups, History", "hint": "1–7" },
  { "keys": ["?"], "context": "panel", "action": "help", "label": "this key reference", "hint": "?" },
  { "keys": [","], "context": "panel", "action": "settings", "label": "settings: the panel's, omaddons' and the clients'", "hint": "," },
  { "keys": ["z"], "context": "panel", "action": "zoom", "label": "fill the screen, and back", "hint": "z" },
  { "keys": ["Ctrl+=", "Ctrl+-", "Ctrl+0"], "context": "panel", "action": "text_zoom", "label": "bigger or smaller text, and back to normal (or Ctrl+wheel)", "hint": "Ctrl+= Ctrl+-" },
  { "keys": ["Esc"], "context": "panel", "action": "close", "label": "back out one layer: a page, then a filter or a search, then the panel closes", "hint": "Esc" },
  { "keys": ["Tab", "Shift+Tab"], "context": "panel", "action": "tab", "label": "the bar's next or previous panel", "hint": "Tab" },
  { "keys": ["j", "k", "Down", "Up"], "context": "view", "action": "move", "label": "move the cursor", "hint": "j k" },
  { "keys": ["g", "G"], "context": "view", "action": "edge", "label": "first or last row", "hint": "g G" },
  { "keys": ["PgUp", "PgDn"], "context": "view", "action": "page", "label": "a page up or down", "hint": "PgUp PgDn" },
  { "keys": ["t", "T"], "context": "view", "action": "client", "label": "next or previous client (All, then each client on this computer)", "hint": "t T" },
  { "keys": ["s"], "context": "view", "action": "search", "label": "Search, its box ready to type in", "hint": "s" },
  { "keys": ["c"], "context": "view", "action": "check", "label": "check for updates now", "hint": "c" },
  { "keys": ["U"], "context": "view", "action": "update_all", "label": "update everything on the chosen client, or on every client", "hint": "U" },
  { "keys": ["Ctrl+Z"], "context": "view", "action": "undo", "label": "undo the last change, after a confirmation (in History and on a change's page, that change)", "hint": "Ctrl+Z" },
  { "keys": ["o"], "context": "view", "action": "open", "label": "open the chosen client's AddOns folder (under All, the row's client)", "hint": "o" },
  { "keys": ["b"], "context": "view", "action": "backup", "label": "back up the chosen client's settings now, or every client's", "hint": "b" },
  { "keys": ["d"], "context": "view", "action": "dismiss", "label": "mark the new Lua errors shown as seen", "hint": "d" },
  { "keys": ["Enter", "Space"], "context": "updates", "action": "enter", "label": "open the addon's page", "hint": "Enter" },
  { "keys": ["u"], "context": "updates", "action": "update", "label": "update this addon", "hint": "u", "nothing": "Nothing to update here" },
  { "keys": ["p"], "context": "updates", "action": "pin", "label": "pin or unpin it", "hint": "p", "nothing": "Nothing to pin here" },
  { "keys": ["e"], "context": "updates", "action": "notes", "label": "show or hide the release notes since its version", "hint": "e", "nothing": "No release notes here" },
  { "keys": ["/"], "context": "updates", "action": "filter", "label": "filter the updates by name, source or state", "hint": "/", "nothing": "Nothing to filter here" },
  { "keys": ["Enter", "Space"], "context": "installed", "action": "enter", "label": "open the addon's page", "hint": "Enter" },
  { "keys": ["u"], "context": "installed", "action": "update", "label": "update this addon", "hint": "u", "nothing": "Nothing to update here" },
  { "keys": ["p"], "context": "installed", "action": "pin", "label": "pin or unpin it", "hint": "p", "nothing": "Nothing to pin here" },
  { "keys": ["x"], "context": "installed", "action": "remove", "label": "remove it, after a confirmation", "hint": "x", "nothing": "Nothing to remove here" },
  { "keys": ["r"], "context": "installed", "action": "restore", "label": "restore its settings from a backup", "hint": "r", "nothing": "No settings to restore here" },
  { "keys": ["/"], "context": "installed", "action": "filter", "label": "filter the list by name, source or state", "hint": "/", "nothing": "Nothing to filter here" },
  { "keys": ["S"], "context": "installed", "action": "sort", "label": "sort by name, by recently updated, or by source", "hint": "S", "nothing": "Nothing to sort here" },
  { "keys": ["Enter", "Space"], "context": "errors", "action": "enter", "label": "show the addon's errors and their stacks", "hint": "Enter" },
  { "keys": ["u"], "context": "errors", "action": "update", "label": "update the addon the errors come from, when one is waiting", "hint": "u", "nothing": "Nothing to update here" },
  { "keys": ["r"], "context": "errors", "action": "restore", "label": "restore the addon's settings from a backup", "hint": "r", "nothing": "No settings to restore here" },
  { "keys": ["y"], "context": "errors", "action": "copy", "label": "copy a report of its errors, to send its author", "hint": "y", "nothing": "Nothing to copy here" },
  { "keys": ["Enter", "Space"], "context": "issues", "action": "enter", "label": "run the row's fix (a repair previews first), or read why", "hint": "Enter" },
  { "keys": ["e"], "context": "issues", "action": "explain", "label": "read why: the problem's page, with the doctor's explanation", "hint": "e", "nothing": "Nothing to explain here" },
  { "keys": ["y"], "context": "issues", "action": "copy", "label": "copy the command a row shows, to run in a terminal", "hint": "y", "nothing": "Nothing to copy here" },
  { "keys": ["Enter", "Space"], "context": "backups", "action": "enter", "label": "open this backup's page: the addon settings it holds, restore, delete", "hint": "Enter" },
  { "keys": ["r"], "context": "backups", "action": "restore", "label": "restore from this backup: choose what", "hint": "r", "nothing": "No settings to restore here" },
  { "keys": ["x"], "context": "backups", "action": "remove", "label": "delete this backup, after a confirmation", "hint": "x", "nothing": "Nothing to remove here" },
  { "keys": ["Enter", "Space"], "context": "history", "action": "enter", "label": "open the change's page: what it did on each client, and its undo", "hint": "Enter" },
  { "keys": ["/"], "context": "history", "action": "filter", "label": "filter the changes by addon, client, who or reason", "hint": "/", "nothing": "Nothing to filter here" },
  { "keys": ["Enter", "Space"], "context": "page", "action": "enter", "label": "run the page's main action: a problem's fix, on an addon page the client line's repair, update or install, on a change's page the cursored addon's page, or on a backup's page the cursored addon's settings back", "hint": "Enter" },
  { "keys": ["u"], "context": "page", "action": "update", "label": "update the addon, when one is waiting (on an addon page, on the client line)", "hint": "u", "nothing": "Nothing to update here" },
  { "keys": ["i"], "context": "page", "action": "install", "label": "an addon page: install it on the client line's client", "hint": "i", "nothing": "Nothing to install here" },
  { "keys": ["x"], "context": "page", "action": "remove", "label": "remove the addon from the client line's client, or delete a backup's page's backup, after a confirmation", "hint": "x", "nothing": "Nothing to remove here" },
  { "keys": ["p"], "context": "page", "action": "pin", "label": "an addon page: pin or unpin it on the client line's client", "hint": "p", "nothing": "Nothing to pin here" },
  { "keys": ["r"], "context": "page", "action": "restore", "label": "restore the addon's settings from a backup, or choose what to restore from a backup's page's backup", "hint": "r", "nothing": "No settings to restore here" },
  { "keys": ["w"], "context": "page", "action": "web", "label": "an addon page: open its web page", "hint": "w", "nothing": "No web page here" },
  { "keys": ["y"], "context": "page", "action": "copy", "label": "copy an error page's report, a problem page's command, or an addon page's source", "hint": "y", "nothing": "Nothing to copy here" },
  { "keys": ["Enter", "Space"], "context": "search", "action": "enter", "label": "open the result's page (a pasted link's: where it works, and Install)", "hint": "Enter" },
  { "keys": ["i"], "context": "search", "action": "install", "label": "install the result where it works (under All, choose the client)", "hint": "i", "nothing": "Nothing to install here" },
  { "keys": ["Enter"], "context": "field", "action": "search", "label": "search now, and go to the results", "hint": "Enter" },
  { "keys": ["Down", "Up"], "context": "field", "action": "move", "label": "move the cursor in the results (the box keeps the keys)", "hint": "Down Up" },
  { "keys": ["Esc"], "context": "field", "action": "leave", "label": "leave the box (its text stays: Esc again clears the search)", "hint": "Esc" },
  { "keys": ["Down", "Up"], "context": "filter", "action": "move", "label": "move the cursor in the list (the box keeps the keys)", "hint": "Down Up" },
  { "keys": ["Enter"], "context": "filter", "action": "first", "label": "go to the first match, and leave the box", "hint": "Enter" },
  { "keys": ["Esc"], "context": "filter", "action": "leave", "label": "leave the box (the filter stays: Esc again clears it)", "hint": "Esc" },
  { "keys": ["Enter"], "context": "pathbox", "action": "add", "label": "add the client in this folder", "hint": "Enter" },
  { "keys": ["Esc"], "context": "pathbox", "action": "leave", "label": "leave the box (its text stays: Esc again goes back)", "hint": "Esc" },
  { "keys": ["j", "k", "Down", "Up"], "context": "card", "action": "move", "label": "move between the card's buttons", "hint": "j k" },
  { "keys": ["Enter", "Space"], "context": "card", "action": "press", "label": "press the button", "hint": "Enter" },
  { "keys": ["Esc", "n", "N"], "context": "sheet", "action": "cancel", "label": "cancel, or go back a step", "hint": "Esc n" },
  { "keys": ["j", "k", "Down", "Up"], "context": "sheet", "action": "line", "label": "scroll a plan sheet, or move between a chooser's choices", "hint": "j k" },
  { "keys": ["h", "l", "Left", "Right", "Tab", "Shift+Tab"], "context": "sheet", "action": "switch", "label": "move between Cancel and the confirm button, or change the setting under the cursor", "hint": "h l" },
  { "keys": ["y", "Y"], "context": "sheet", "action": "confirm", "label": "confirm a plan sheet", "hint": "y", "confirms": true },
  { "keys": ["Enter", "Space"], "context": "sheet", "action": "press", "label": "the button, choice or setting under the cursor (a plan sheet starts on Cancel)", "hint": "Enter", "confirms": true },
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
var VIEW_BOXES = { "search": "field", "installed": "filter", "history": "filter" }

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
  // No client on this computer (1n2p.32): nothing is "good" yet.
  if (!filter && liveTargets(targets).length === 0) return copy("welcome.hero").toUpperCase()
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
    if (checked && (!oldest || instantMs(checked) < instantMs(oldest))) oldest = checked
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
// Updates its updates; Errors its new Lua errors, urgent; Issues its problems, urgent
// for a P0 or P1; Backups the clients `status` asks to back up (`backup_create`, no
// checkpoint in 7 days).
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
    // New Lua errors, always urgent (§2.1); the problems in Issues, urgent for a P0 or P1.
    "errors": { "count": facts.errors, "urgent": facts.errors > 0 },
    "issues": { "count": facts.problems, "urgent": facts.urgent > 0 },
    "backups": { "count": backups, "urgent": false },
    // History carries no count (§2.1).
    "history": none
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
// it runs through the confirm flow, or why there is none. When the cached status cannot
// tell which change is last (M3-D37: never a guessed run), it opens History instead
// (`history: true`), where every change is listed with its own undo (1n2p.30).
function undoAction(lastChange) {
  if (!lastChange) return { "label": "Undo last", "argv": null, "why": "nothing to undo" }
  if (lastChange.unknown) return { "label": copy("history.undo_last.unknown"), "argv": null, "why": "", "history": true }
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

// The kind of Updates row each `list -v` status gives (updateRows); other statuses give
// none.
var UPDATE_KINDS = { "update_available": "update", "pinned": "pinned", "blocked": "blocked", "failed": "failed" }

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

// How old a release is, in words ("out today", "2 days old", "3 months old"), from its
// `published_at`; "" when its source gave no date (GitHub's keyless path gives none).
function releaseAge(iso, nowMs) {
  var at = instantMs(iso)
  if (!isFinite(at)) return ""
  var days = Math.floor(Math.max(0, num(nowMs) - at) / 86400000)
  if (days < 1) return copy("updates.age.today")
  if (days < 60) return copy("updates.age.days", { "n": days })
  if (days < 730) return copy("updates.age.months", { "n": Math.floor(days / 30) })
  return copy("updates.age.years", { "n": Math.floor(days / 365) })
}

// The glyph, band and order of each kind of Updates row (design doc §5.3, §5.4): an
// update (arrow up), one a change just updated (a check, in the update's place), a
// pinned addon (pin), a blocked release (cancel), a failed check (sync alert).
var UPDATE_GLYPHS = { "update": 0xF0CE1, "done": 0xF012C, "pinned": 0xF0403, "blocked": 0xF073A, "failed": 0xF04E7 }
var UPDATE_BANDS = { "update": "accent", "done": "accent", "pinned": "", "blocked": "", "failed": "urgent" }
var UPDATE_RANK = { "update": 0, "done": 0, "pinned": 1, "blocked": 2, "failed": 3 }

// One Updates row (1n2p.21, design doc §5.3): an AddonRow's fields, and what the plan
// runner compares (shownOf's).
function updateRow(t, a, kind, status, client, nowMs, doneWord) {
  var meta = []
  var running = !!status && status.running === true
  if (kind === "update") {
    var age = releaseAge(a.publishedAt, nowMs)
    if (age) meta.push(age)
    if (running) meta.push(copy("updates.meta.running", { "client": client }))
  } else if (kind === "done") {
    meta.push(str(doneWord))
  } else if (kind === "pinned") {
    meta.push(copy("updates.meta.pinned", { "version": a.version || "?", "latest": a.latest || "" }))
  } else if (kind === "blocked") {
    meta.push(copy("updates.meta.blocked", { "latest": a.latest || "", "client": client }))
  } else {
    meta.push(copy("updates.meta.failed", { "source": SOURCE_WORDS[a.source] || copy("installed.source.unknown") }))
  }
  var chips = a.channel && a.channel !== "stable" ? [{ "text": a.channel, "urgent": false }] : []
  var act = function(name, glyph, label) {
    return { "name": name, "glyph": String.fromCodePoint(glyph), "tooltip": withKey(copy(label), name, "updates") }
  }
  var actions = []
  if (kind === "update") actions.push(act("update", 0xF0CE1, "updates.action.update"))
  if (kind === "failed") actions.push(act("check", 0xF04E6, "updates.action.check"))
  if (kind !== "failed" && kind !== "done") actions.push(act("notes", 0xF0219, "updates.action.notes"))
  if (kind !== "failed" && kind !== "done") {
    actions.push(act("pin", a.pinned ? 0xF0404 : 0xF0403, a.pinned ? "installed.action.unpin" : "installed.action.pin"))
  }
  return {
    "key": t.id + "/" + a.name, "target": t.id, "client": client, "name": a.name, "ref": a.ref, "version": a.version, "latest": a.latest,
    "kind": kind, "pinned": a.pinned, "reload": running, "group": "", "groupCount": "", "groupTarget": "", "groupUpdates": 0,
    "targetKey": t.key, "channel": a.channel, "latestSource": a.latestSource, "latestReleaseId": a.latestReleaseId,
    "latestAsset": a.latestAsset, "sourceWord": SOURCE_WORDS[a.source] || copy("installed.source.unknown"),
    "fromVersion": kind === "done" ? "" : (a.version || "?"),
    "toVersion": kind === "update" ? str(a.latest) : (kind === "done" ? str(a.version) : ""),
    "meta": meta.join(" · "), "chips": chips, "glyph": UPDATE_GLYPHS[kind], "band": UPDATE_BANDS[kind], "actions": actions
  }
}

// The Updates view's rows, in the filter (1n2p.21, design doc §2.3): per client, its
// updates, then pinned addons with a newer release, blocked releases and failed checks,
// by name within each; under All, each client's header (its player name, whether it
// runs, its updates) above its first row, with what "Update {client}" updates there.
// `view`: `nowMs` (release ages), `done` (the service's `{<key>: word}` of rows a change
// just changed: such a row stays, in its update's place, until it expires) and `query`
// (`/`, as the Addons view filters: each word a subsequence of the name, the source or
// the row's words).
function updateRows(lists, filter, statusTargets, view) {
  var v = view && typeof view === "object" ? view : {}
  var nowMs = typeof v.nowMs === "number" ? v.nowMs : Date.now()
  var done = v.done && typeof v.done === "object" ? v.done : {}
  var query = str(v.query)
  var out = []
  var targets = listTargets(lists, filter)
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    var status = targetById(statusTargets, t.id)
    var client = sheetClient(statusTargets, t.id, "")
    var rows = []
    for (var j = 0; j < t.addons.length; j++) {
      var a = t.addons[j]
      var key = t.id + "/" + a.name
      var kind = UPDATE_KINDS[a.status]
      if (kind === "pinned" && (!a.latest || a.latest === a.version)) kind = undefined
      if (!kind && done.hasOwnProperty(key)) kind = "done"
      if (!kind) continue
      var row = updateRow(t, a, kind, status, client, nowMs, done[key])
      // Short words, as the Addons view's chips: a sentence would match nearly anything.
      var words = [row.name, row.sourceWord, copy("updates.kind." + kind)].concat(row.chips.map(function(c) { return c.text }))
      if (query && !filterMatches(query, words)) continue
      rows.push(row)
    }
    rows = stableSort(rows, function(x, y) {
      var byKind = UPDATE_RANK[x.kind] - UPDATE_RANK[y.kind]
      if (byKind) return byKind
      var nx = asciiLower(x.name)
      var ny = asciiLower(y.name)
      return nx < ny ? -1 : (nx > ny ? 1 : 0)
    })
    // Under All, the client's header above its first row: its player name, whether it
    // runs, and its updates (design doc §2.3).
    if (!filter && rows.length > 0) {
      var updates = updateCount(rows)
      var group = clientGroup(statusTargets, t.id, updates > 0 ? plural(updates, "update") : "")
      rows[0].group = group.title
      rows[0].groupCount = group.count
      rows[0].groupTarget = t.id
      rows[0].groupUpdates = updates
    }
    out = out.concat(rows)
  }
  return out
}

// "Update Retail (3)": a client header's button, the client's updates previewed as one
// (updateRequest's, for that client only).
function groupUpdateRequest(target, rows, targets) {
  var request = updateRequest(target, (Array.isArray(rows) ? rows : []).filter(function(r) { return !!r && r.target === target }), targets)
  request.label = copy("updates.update_client", { "client": sheetClient(targets, target, "") })
  return request
}

// The pending release notes of `row` (`e`, 1n2p.21): every release newer than the one
// installed, from the service's view-bound changelog (`info -t <client> --changelog`):
// `{state, entries: [{version, date, text}], behind, url, link, failed}`. `state` is
// `reading` until the answer for this row arrives (a late one for another row never
// matches), `failed` when it could not be read, else `ready`. `behind` is how many
// releases the row is behind ("3 releases behind"; "10+" when the feed cut the list);
// the link shows only when isSafeUrl accepts it.
function releaseNotes(changelog, row, nowMs) {
  var out = { "state": "reading", "entries": [], "behind": "", "url": "", "link": "", "failed": "" }
  if (!changelog || !changelog.selection || !row) return out
  if (changelog.selection.target !== row.target || changelog.selection.addon !== row.name) return out
  if (changelog.failed) {
    out.state = "failed"
    out.failed = copy("updates.notes.failed", { "why": str(changelog.failed) })
    return out
  }
  out.state = "ready"
  var log = changelog.data && changelog.data.changelog && typeof changelog.data.changelog === "object" ? changelog.data.changelog : {}
  var entries = Array.isArray(log.entries) ? log.entries.filter(function(e) { return !!e && typeof e === "object" }) : []
  out.entries = entries.map(function(e) {
    var age = releaseAge(e.published_at, nowMs)
    return { "version": str(e.version), "date": age, "text": str(e.text) || copy("updates.notes.empty") }
  })
  if (entries.length > 0) {
    out.behind = copy(log.truncated === true ? "updates.notes.behind_more" : "updates.notes.behind", { "n": entries.length })
  }
  var more = str(log.more_url)
  if (more && isSafeUrl(more)) {
    out.url = more
    out.link = copy("updates.notes.more", { "host": MORE_HOSTS[more.split("/")[2].toLowerCase()] || "the web" })
  }
  return out
}

// The Updates header's freshness (1n2p.21): "Checked 25 min ago", the oldest check of
// the clients in the filter; "Not checked yet" before any.
function updatesChecked(statusTargets, filter, nowMs) {
  var oldest = oldestCheck(Array.isArray(statusTargets) ? statusTargets : [], filter)
  return oldest ? copy("updates.checked", { "age": sinceText(oldest, nowMs) }) : copy("updates.unchecked")
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
    if (checked && (!oldest || instantMs(checked) < instantMs(oldest))) oldest = checked
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
// Issues lists `status`'s problems and its warnings: the frame's own read.
var VIEW_RESOURCES = { "updates": "list", "installed": "list", "search": "search", "errors": "errors", "issues": "status", "backups": "backups",
  "history": "history" }
// The codes that mean a source did not answer: on the last check's warnings (the check
// then showed what it remembered) or its failed items.
var SOURCE_TROUBLE = ["network", "http_status", "rate_limited", "offline", "stale_check"]
// A state's glyph (design doc §5.4): loading spins, a failure alerts, an empty view shows
// its own (all good is a check).
var STATE_GLYPHS = { "firstLoad": 0xF04E6, "failed": 0xF0028, "filteredEmpty": 0xF0236 }
var EMPTY_GLYPHS = { "updates": 0xF05E0, "installed": 0xF0431, "search": 0xF0349, "errors": 0xF05E0, "issues": 0xF05E0, "backups": 0xF003C,
  "history": 0xF02DA }

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

// The quiet line over rows a read is replacing (design doc §7.2): "Searching…" while a
// newer query runs over the last one's results (1n2p.25), else "Updating…".
function refreshingText(view) {
  return copy(view === "search" ? "state.refreshing.search" : "state.refreshing")
}

// "18:02", local time, from an RFC 3339 instant; "" for one that does not parse.
function clockText(iso) {
  var ms = instantMs(str(iso))
  if (isNaN(ms)) return ""
  var at = new Date(ms)
  var two = function(n) { return n < 10 ? "0" + n : String(n) }
  return two(at.getHours()) + ":" + two(at.getMinutes())
}

// Whether the service holds the view's data: the search's only for the query and client
// it was asked with.
function viewHas(view, s, query, filter) {
  if (view === "updates" || view === "installed") return !!s.lists
  if (view === "errors") return !!s.errors
  // Issues reads nothing of its own: `status`'s answer is its data.
  if (view === "issues") return !!s.summary
  if (view === "backups") return !!s.backups
  if (view === "history") return !!s.history
  if (view === "search") {
    var selection = s.search && s.search.selection && typeof s.search.selection === "object" ? s.search.selection : null
    return !!selection && selection.query === query && selection.target === filter
  }
  return false
}

// When the view's data was read, as an age ("25 min ago"), or "earlier".
function readAge(view, s, nowMs) {
  var at = view === "errors" ? num(s.errorsAt) : view === "backups" ? num(s.backupsAt) : view === "history" ? num(s.historyAt)
    : view === "search" || view === "issues" ? 0 : num(s.listsAt)
  return at > 0 ? sinceText(new Date(at).toISOString(), nowMs) : copy("state.age.unknown")
}

// The search's skipped sources as notices, one per reason: "GitHub and Tukui couldn't be
// searched: offline." A source that did not answer (`no_index`) has Try again; offline,
// a rate limit with its time and a source turned off say why asking again now would not
// help, and offer nothing (1n2p.25).
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
    return stateNotice(reason === "offline" ? "offline" : "partial", copy(id, { "sources": sources, "time": time }),
      reason === "other" ? stateAction("retry") : null)
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
  // No client on this computer (1n2p.32): the welcome card in every view, with Scan
  // again and Add a client folder; no view says it is empty or up to date.
  var welcome = filter ? null : welcomeCard(s.summary || null, targets)
  if (welcome) {
    out.state = "welcome"
    out.refreshing = false
    out.notices.push({ "kind": "welcome", "sentence": welcome.title, "body": welcome.sentence, "action": null,
      "actions": welcome.buttons })
    return out
  }
  // An empty box: the catalog's rows when there are some (1n2p.26), with nothing about
  // a search; else what to type.
  if (view === "search" && query === "") {
    out.refreshing = false
    if (num(i.rows) === 0) set("empty", copy("state.empty.search"))
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
  if (view === "errors" && !out.refreshing) {
    var errorTargets = s.errors && Array.isArray(s.errors.targets) ? s.errors.targets : []
    if (errorTargets.some(function(t) { return t && t.partial === true && (!filter || t.id === filter) })) {
      out.notices.push(stateNotice("partial", copy("state.notice.errors_partial"), stateAction("read_errors")))
    }
  }
  if (view === "backups" && !out.refreshing && scoped.some(function(t) { return !!t.backups && t.backups.stale === true })) {
    out.notices.push(stateNotice("stale", copy("state.notice.backups_stale"), stateAction("refresh_backups")))
  }
  // Only the answer for this query and client says which sources it skipped: rows kept
  // from the last query while the next one runs carry none of their notices.
  if (view === "search" && s.search && viewHas(view, s, query, filter)) out.notices = out.notices.concat(skippedNotices(s.search.skipped))
  // Lines `history` could not read (§6.5.3), and a list cut at its limit: said, never
  // guessed at.
  if (view === "history" && s.history && !out.refreshing) {
    if (s.history.skipped) out.notices.push(stateNotice("partial", copy("state.notice.history_skipped")))
    if (s.history.truncated) out.notices.push(stateNotice("partial", copy("state.notice.history_truncated", { "n": HISTORY_LIMIT })))
  }
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
  } else if (view === "errors") {
    set(filtered ? "filteredEmpty" : "empty", copy(filter ? "state.empty.errors.client" : "state.empty.errors", { "client": client }))
  } else if (view === "issues") {
    set(filtered ? "filteredEmpty" : "empty", copy(filter ? "state.empty.issues.client" : "state.empty.issues", { "client": client }))
  } else if (view === "backups") {
    set(filtered ? "filteredEmpty" : "empty", copy(filter ? "state.empty.backups.client" : "state.empty.backups", { "client": client }),
      stateAction("backup"))
  } else if (view === "history") {
    set(filtered ? "filteredEmpty" : "empty", copy(filter ? "state.empty.history.client" : "state.empty.history", { "client": client }))
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
  if (problem) actions.push({ "name": "repair", "glyph": String.fromCodePoint(0xF0DC9), "tooltip": copy("installed.action.repair") })
  if (a.updateAvailable) actions.push({ "name": "update", "glyph": String.fromCodePoint(0xF0CE1), "tooltip": withKey(copy("installed.action.update"), "update", "installed") })
  actions.push({ "name": "pin", "glyph": String.fromCodePoint(a.pinned ? 0xF0404 : 0xF0403),
    "tooltip": withKey(copy(a.pinned ? "installed.action.unpin" : "installed.action.pin"), "pin", "installed") })
  actions.push({ "name": "restore", "glyph": String.fromCodePoint(0xF006F), "tooltip": withKey(copy("installed.action.restore"), "restore", "installed") })
  actions.push({ "name": "remove", "glyph": String.fromCodePoint(0xF01B4), "tooltip": withKey(copy("installed.action.remove"), "remove", "installed") })
  return {
    "key": t.id + "/" + a.name, "target": t.id, "client": client, "name": a.name, "ref": a.ref, "version": a.version, "source": a.source,
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
// updates, else nothing (`primary`): what the addon page's Enter does on its client line;
// Enter on the row opens that page (1n2p.23).
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
// Each severity's glyph (design doc §5.4): broken, needs attention, minor.
var SEVERITY_GLYPHS = { "P0": 0xF0029, "P1": 0xF0026, "P2": 0xF0026, "P3": 0xF02FD }
var SUGGESTION_GLYPH = 0xF02FD
// `commands[]` actions the Issues view never lists (design doc §6.5.1): installing the
// agent skill is for agents. (A plan's apply, which runs only from its own preview, is
// neither runnable nor a command to copy, so it is never listed either.)
var SUGGESTION_SILENT = ["install_skill"]
// Warning codes the Issues view leaves to another place (design doc §6.5.3): the hero
// (`running_unknown`), the phase card (`widget_outdated`), the activity bar
// (`tx_in_progress`), nowhere (`state_version_older`, `atomic_swap_unavailable`), or
// `status`'s own problems, which name the same thing with its fix (the doctor's
// cached checks: a credential file others can read, an interrupted change, a newer
// omaddons's state).
var NOTE_ELSEWHERE = ["running_unknown", "widget_outdated", "tx_in_progress", "state_version_older", "atomic_swap_unavailable",
  "credential_file_mode", "journal_pending", "state_version_newer"]

// The Issues view (1n2p.28, design doc §2.3, §6.5, §6.6), in the filter, as sections
// whose rows the cursor walks in order (`index`):
//   - the problems of no client ("This computer"), under All only;
//   - each client's problems under its player name, the client whose problem is most
//     severe first, each by severity in the order `status` ranked them;
//   - `status`'s recommendations (`commands`) that no problem row already runs
//     ("Suggested"), a client's only when the scope is that client or All;
//   - the warnings of the last `status` the view shows ("Notes"), under All only.
// A section is `{key, title, count, rows}`. A row is fixFields' and its `kind`
// (problem, suggestion, note), a stable `key`, `target`, `client`, `severity` with its
// word (`severityWord`), `urgent`, `band`, `glyph` (a code point) and `title`, one
// sentence. Lua errors have their own view since 1n2p.27 (errorSections).
function issueSections(statusTargets, warnings, filter, machineIssues, commands) {
  var targets = Array.isArray(statusTargets) ? statusTargets : []
  var sections = []
  var offered = []
  var keys = {}
  var keyOf = function(key) {
    keys[key] = (keys[key] || 0) + 1
    return keys[key] === 1 ? key : key + "#" + keys[key]
  }
  var problem = function(issue, target) {
    var id = target ? target.id : ""
    var severity = str(issue.severity)
    var r = issue.remediation || {}
    if (r.runnable && Array.isArray(r.argv)) offered.push(r.argv)
    return Object.assign({
      "kind": "problem", "key": keyOf("p/" + id + "/" + str(issue.id) + "/" + str(issue.addon)), "target": id,
      "client": target ? (target.displayName || target.id) : "", "severity": severity,
      "severityWord": COPY.hasOwnProperty("issues.severity." + severity) ? copy("issues.severity." + severity) : severity,
      "urgent": issue.urgent === true, "band": issue.urgent === true ? "urgent" : "",
      "glyph": SEVERITY_GLYPHS.hasOwnProperty(severity) ? SEVERITY_GLYPHS[severity] : SUGGESTION_GLYPH,
      "title": str(issue.title), "autoFixable": issue.autoFixable === true
    }, problemRow(issue, id, targets))
  }
  // Within a severity, the order `status` ranked them in (Qt's sort is not stable).
  var bySeverity = function(a, b) { return SEVERITY_ORDER.indexOf(a.severity) - SEVERITY_ORDER.indexOf(b.severity) }
  var counted = function(rows) { return copy("issues.count", { "n": rows.length }) }

  var machine = !filter && Array.isArray(machineIssues) ? machineIssues : []
  if (machine.length) {
    var own = stableSort(machine.map(function(mi) { return problem(mi, null) }), bySeverity)
    sections.push({ "key": "machine", "title": copy("issues.section.machine"), "count": counted(own), "rows": own })
  }
  var clients = []
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    if ((filter && t.id !== filter) || !Array.isArray(t.issues) || t.issues.length === 0) continue
    var rows = stableSort(t.issues.map(function(issue) { return problem(issue, t) }), bySeverity)
    clients.push({ "key": "client/" + t.id, "title": t.displayName || t.id, "count": counted(rows), "rows": rows })
  }
  // The client with the most severe problem first; equals keep `status`'s client order.
  sections = sections.concat(stableSort(clients, function(a, b) { return bySeverity(a.rows[0], b.rows[0]) }))

  var suggested = []
  var list = Array.isArray(commands) ? commands : []
  for (var c = 0; c < list.length; c++) {
    var cmd = list[c]
    if (!cmd || typeof cmd !== "object" || SUGGESTION_SILENT.indexOf(cmd.action) !== -1) continue
    // Nothing to run and no command to copy: nothing to suggest.
    if (cmd.runnable !== true && cmd.terminal !== true) continue
    // A recommendation a problem row already runs is that row's fix, never a second row.
    if (offered.some(function(argv) { return sameArgv(argv, cmd.argv) })) continue
    var parts = Array.isArray(cmd.argv) ? argvParts(cmd.argv) : { "path": "", "target": "", "flags": [], "operands": [] }
    if (filter && parts.target !== filter) continue
    var client = parts.target ? sheetClient(targets, parts.target, "") : ""
    suggested.push(Object.assign({
      "kind": "suggestion", "key": keyOf("s/" + str(cmd.action) + "/" + str(cmd.command)), "target": parts.target, "client": client,
      "severity": "", "severityWord": "", "urgent": false, "band": "", "glyph": SUGGESTION_GLYPH,
      "title": suggestionText(str(cmd.action), parts, targets)
    }, fixFields(issueFix(cmd, targets), null, cmd, false, false)))
  }
  if (suggested.length) sections.push({ "key": "suggested", "title": copy("issues.section.suggested"), "count": "", "rows": suggested })

  // The warnings this view shows, by code: a code this build has words for gets them
  // (warnings carry no typed client, so the words name none); a code from a newer
  // omaddons shows its message. A prose fix is not shown (it names commands and config
  // keys); a fix the classifier accepts is a named button, one it refuses a command to
  // copy.
  var notes = []
  var shown = !filter && Array.isArray(warnings) ? warnings : []
  for (var w = 0; w < shown.length; w++) {
    var warning = shown[w]
    if (!warning || typeof warning !== "object" || NOTE_ELSEWHERE.indexOf(warning.code) !== -1) continue
    var words = COPY.hasOwnProperty("issues.note." + warning.code) ? copy("issues.note." + warning.code) : str(warning.message)
    if (words === "") continue
    var note = fixFields(issueFix(warning.cli, targets), null, warning.cli, false, false)
    note.prose = ""
    notes.push(Object.assign({
      "kind": "note", "key": keyOf("n/" + str(warning.code)), "target": "", "client": "", "severity": "", "severityWord": "",
      "urgent": false, "band": "", "glyph": SUGGESTION_GLYPH, "title": words
    }, note))
  }
  if (notes.length) sections.push({ "key": "notes", "title": copy("issues.section.notes"), "count": "", "rows": notes })

  var index = 0
  for (var s = 0; s < sections.length; s++) {
    for (var n = 0; n < sections[s].rows.length; n++) sections[s].rows[n].index = index++
  }
  return sections
}

// What a `status` recommendation is about, in a sentence (design doc §6.5.1's actions),
// from its action and the typed facts of `status` it names: the client its `-t` selects,
// the addons its argv names, the counts `status` gives that client.
function suggestionText(action, parts, targets) {
  var client = parts.target ? sheetClient(targets, parts.target, "") : ""
  var target = parts.target ? targetById(targets, parts.target) : null
  var slots = { "client": client, "owner": possessive(client), "addons": joinNames(parts.operands), "addon": parts.operands[0] || "",
    "n": parts.operands.length }
  if (action === "update") {
    var waiting = (target ? [target] : targets).filter(function(t) { return !!t.updates && t.updates.available > 0 })
    slots.n = waiting.reduce(function(sum, t) { return sum + t.updates.available }, 0)
    slots.m = waiting.length
    if (slots.n === 0) return copy("issues.suggest.update.unknown")
    return copy(target ? "issues.suggest.update" : "issues.suggest.update.clients", slots)
  }
  if (action === "adopt") {
    slots.n = target ? target.adoptable : 0
    return copy(slots.n > 0 ? "issues.suggest.adopt" : "issues.suggest.adopt.unknown", slots)
  }
  return COPY.hasOwnProperty("issues.suggest." + action) ? copy("issues.suggest." + action, slots) : copy("issues.suggest.other")
}

// How many problems the sections hold: what the Issues view's state counts (a view with
// suggestions or notes but no problem still says nothing needs fixing).
function problemCount(sections) {
  return issueRows(sections).filter(function(r) { return r.kind === "problem" }).length
}

// -------------------------------------------------------------- the addon page (1n2p.23)

// `info --changelog`'s answer as the addon page reads it: what the source says of the
// addon (`about`, its page only when isSafeUrl allows it), why it says nothing
// (`aboutWhy`, `about_unavailable.reason`), the newest release and how likely it loads
// on each client (`compat`), each installed record's folders, the folders it leaves out
// and its TOC, and the release notes (null when there are none, `changelogWhy` why).
function reduceInfo(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || typeof env.data !== "object") return null
  var d = env.data
  var about = d.about && typeof d.about === "object" ? d.about : null
  var popularity = about && about.popularity && typeof about.popularity === "object" ? about.popularity : {}
  var latest = d.latest && typeof d.latest === "object" ? d.latest : null
  var compat = latest && latest.compat && typeof latest.compat === "object" ? latest.compat : {}
  var changelog = d.changelog && typeof d.changelog === "object" ? d.changelog : null
  return {
    "name": str(d.name),
    "ref": str(d.ref),
    "about": about ? {
      "summary": str(about.summary), "author": str(about.author), "updatedAt": str(about.updated_at),
      "homepage": isSafeUrl(about.homepage) ? about.homepage : "",
      "stars": typeof popularity.stars === "number" ? popularity.stars : null,
      "downloads": typeof popularity.downloads === "number" ? popularity.downloads : null
    } : null,
    "aboutWhy": d.about_unavailable && typeof d.about_unavailable === "object" ? str(d.about_unavailable.reason) : "",
    "latest": latest ? { "version": str(latest.version), "compat": compat } : null,
    "installed": (Array.isArray(d.installed) ? d.installed : []).map(function(rec) {
      var toc = rec.toc && typeof rec.toc === "object" ? rec.toc : null
      return {
        "target": rec.target && typeof rec.target === "object" ? str(rec.target.id) : "",
        "version": strOrNull(rec.version),
        "folders": stringList(rec.folders),
        "leftOut": (Array.isArray(rec.left_out) ? rec.left_out : []).map(function(l) { return l && typeof l === "object" ? str(l.name) : "" })
          .filter(function(n) { return n !== "" }),
        "toc": toc ? {
          "title": str(toc.title_plain), "notes": str(toc.notes), "author": str(toc.author),
          "dependencies": stringList(toc.dependencies)
        } : null
      }
    }),
    "changelog": changelog ? {
      "entries": (Array.isArray(changelog.entries) ? changelog.entries : []).map(function(e) {
        return { "version": str(e.version), "publishedAt": str(e.published_at), "text": str(e.text) }
      }),
      "truncated": changelog.truncated === true,
      "moreUrl": isSafeUrl(changelog.more_url) ? changelog.more_url : ""
    } : null,
    "changelogWhy": d.changelog_unavailable && typeof d.changelog_unavailable === "object" ? str(d.changelog_unavailable.reason) : ""
  }
}

// How likely a release loads on a client, in words (design doc §6.3): `info`'s compat.
var COMPAT_WORDS = { "verified": "page.compat.works", "declared": "page.compat.works", "likely": "page.compat.likely",
  "unlikely": "page.compat.not", "blocked": "page.compat.not" }

// The addon page (1n2p.23, design doc §2.4), from what the panel holds: `name` and `ref`
// (the row it opened from; `ref` may be null), `lists` (`list -v`, reduced: the local
// facts, there at once), `info` (the page's own `info` read, reduced, null until it
// answers), `reading` and `failed` (that read's state), `errors` (`errors`, reduced),
// `statusTargets` (`status`'s clients, for their names), `nowMs`. The answer:
//   - the header: `title` (its TOC's title, else its name), `name`, `summary`, `facts`
//     ("by …", popularity, when it last changed), `source` (a word), `homepage` (a safe
//     URL or "");
//   - `clients`: one line per client, where it is installed first (each an Addons-view
//     row, `row`, so every action runs as it does there), then where it is not, with how
//     likely the newest release loads there; each line's `actions` (named, with their
//     key: the cursored line's) and `primary` (Enter);
//   - `notes`: the release notes newer than the oldest installed version, newest first;
//   - `folders`: what it owns on each client, and what that client leaves out;
//   - `needs` (its TOCs' dependencies, each `{name, installed}`) and `neededBy`;
//   - `errors`: its Lua errors on each client (`{target, client, key, text}`);
//   - `state`: "reading", "failed" (with `failed`) or "" as the read stands, and `why`,
//     a sentence when the source said nothing of it.
function addonPage(input) {
  var i = input && typeof input === "object" ? input : {}
  var info = i.info && typeof i.info === "object" ? i.info : null
  // A page opened from a pasted ref or link (`link`, 1n2p.25) knows no name or ref until
  // its read answers: both come from `info`, never from the text typed (M3-D2), so an
  // install from it takes the ref the CLI resolved.
  var link = str(i.link)
  var name = str(i.name) || (link && info ? info.name : "")
  var statusTargets = Array.isArray(i.statusTargets) ? i.statusTargets : []
  var nowMs = typeof i.nowMs === "number" ? i.nowMs : Date.now()
  var listTargets = i.lists && Array.isArray(i.lists.targets) ? i.lists.targets : []
  var ref = str(i.ref) || (info ? info.ref : "")
  // Its installed records: the same name, and the same ref when the page knows one.
  var records = []
  listTargets.forEach(function(t) {
    t.addons.forEach(function(a) {
      if (a.name === name && (!ref || !a.ref || a.ref === ref)) records.push({ "t": t, "a": a })
    })
  })
  if (!ref && records.length) ref = str(records[0].a.ref)
  var installedOn = function(id) { return (info ? info.installed : []).filter(function(r) { return r.target === id })[0] || null }
  var firstToc = (info ? info.installed : []).map(function(r) { return r.toc }).filter(function(t) { return !!t })[0] || null
  var about = info && info.about ? info.about : null

  var facts = []
  if (about && about.author) facts.push(copy("page.by", { "author": about.author }))
  else if (firstToc && firstToc.author) facts.push(copy("page.by", { "author": firstToc.author }))
  var popularity = about ? popularityText({ "downloads": about.downloads, "stars": about.stars }) : ""
  if (popularity) facts.push(popularity)
  if (about && about.updatedAt) facts.push(copy("page.updated", { "age": sinceText(about.updatedAt, nowMs) || dateText(about.updatedAt) }))

  var clients = []
  var seen = {}
  records.forEach(function(rec) {
    var client = sheetClient(statusTargets, rec.t.id, rec.t.key)
    var row = installedRow(rec.t, rec.a, client)
    seen[rec.t.id] = true
    var actions = []
    var act = function(action, label, glyph) {
      actions.push({ "name": action, "text": copy(label), "glyph": String.fromCodePoint(glyph), "tooltip": withKey(copy(label), action, "page") })
    }
    if (row.problem) act("enter", "installed.action.repair", 0xF0DC9)
    if (row.updateAvailable) act("update", "installed.action.update", 0xF0CE1)
    act("pin", row.pinned ? "installed.action.unpin" : "installed.action.pin", row.pinned ? 0xF0404 : 0xF0403)
    act("restore", "installed.action.restore", 0xF006F)
    act("remove", "installed.action.remove", 0xF01B4)
    var meta = [rec.a.version ? copy("page.installed", { "version": rec.a.version }) : copy("page.installed.unknown")]
    if (row.updateAvailable && rec.a.latest) meta.push(copy("page.update", { "version": rec.a.latest }))
    if (rec.a.channel && rec.a.channel !== "stable") meta.push(copy("page.channel", { "channel": rec.a.channel }))
    clients.push({
      "key": "c/" + rec.t.id, "target": rec.t.id, "client": client, "installed": true, "row": row,
      "meta": meta.join(" · "), "chips": row.chips, "band": row.band, "glyph": row.glyph,
      "primary": row.problem ? "reinstall" : (row.updateAvailable ? "update" : ""), "actions": actions
    })
  })
  // Where it is not installed: how likely the newest release loads there, when `info`
  // said, and Install here where it likely does.
  var compat = info && info.latest ? info.latest.compat : {}
  statusTargets.forEach(function(t) {
    if (seen[t.id] || t.missing === true) return
    var verdict = compat.hasOwnProperty(t.id) ? str(compat[t.id]) : ""
    var installable = !!ref && (verdict === "verified" || verdict === "declared" || verdict === "likely")
    clients.push({
      "key": "c/" + t.id, "target": t.id, "client": t.displayName || t.id, "installed": false, "row": null,
      "meta": copy("page.not_installed") + (COMPAT_WORDS.hasOwnProperty(verdict) ? " · " + copy(COMPAT_WORDS[verdict]) : ""),
      "chips": [], "band": "", "glyph": 0, "primary": installable ? "install" : "",
      "actions": installable ? [{ "name": "install", "text": copy("page.install_here"), "glyph": String.fromCodePoint(0xF0415),
        "tooltip": withKey(copy("page.install_here"), "install", "page") }] : []
    })
  })

  // The release notes the page's read brought: each version, its age, its text cut.
  var notes = info && info.changelog ? info.changelog.entries.map(function(e) {
    var age = e.publishedAt ? (sinceText(e.publishedAt, nowMs) || dateText(e.publishedAt)) : ""
    var text = e.text.length > NOTES_MAX_CHARS ? e.text.slice(0, NOTES_MAX_CHARS - 1) + "…" : e.text
    return { "version": e.version, "when": age, "text": text }
  }) : []
  var moreUrl = info && info.changelog ? info.changelog.moreUrl : ""

  var folders = records.map(function(rec) {
    var record = installedOn(rec.t.id)
    return { "target": rec.t.id, "client": sheetClient(statusTargets, rec.t.id, rec.t.key), "folders": rec.a.folders,
      "leftOut": record ? record.leftOut : [] }
  })

  // What its TOCs need, and what needs it (`required_by`), each a page of its own.
  var installedNames = {}
  listTargets.forEach(function(t) { t.addons.forEach(function(a) { installedNames[a.name.toLowerCase()] = a.name }) })
  var needNames = []
  var tocs = info ? info.installed.map(function(r) { return r.toc }).filter(function(t) { return !!t }) : []
  tocs.forEach(function(toc) {
    toc.dependencies.forEach(function(d) { if (needNames.indexOf(d) === -1) needNames.push(d) })
  })
  var needs = needNames.map(function(d) {
    var known = installedNames[d.toLowerCase()]
    return { "name": known || d, "installed": !!known }
  })
  var neededBy = []
  records.forEach(function(rec) { rec.a.requiredBy.forEach(function(n) { if (neededBy.indexOf(n) === -1) neededBy.push(n) }) })

  // Its Lua errors, per client, with the key of the Errors view's row (its error page).
  var errors = []
  var errorTargets = i.errors && Array.isArray(i.errors.targets) ? i.errors.targets : []
  errorTargets.forEach(function(e) {
    (Array.isArray(e.addons) ? e.addons : []).forEach(function(a) {
      if (a.addon !== name) return
      var client = sheetClient(statusTargets, e.id, "")
      var parts = [copy("page.errors.count", { "n": a.count, "client": client })]
      if (a["new"] > 0) parts.push(copy("errors.new", { "n": a["new"] }))
      errors.push({ "target": e.id, "client": client, "key": "a/" + e.id + "/" + a.addon, "text": parts.join(" · ") })
    })
  })

  // Its newest changes (1n2p.30), from History's read: at most ADDON_PAGE_CHANGES, each
  // opening its change's page; one undone since says so.
  var changes = name === "" ? [] : historyRuns(i.history || null, statusTargets).filter(function(r) {
    return r.items.some(function(item) { return item.addon === name })
  }).slice(0, ADDON_PAGE_CHANGES).map(function(r) {
    return { "key": r.key, "title": r.title, "state": r.state,
      "text": copy(r.state === "undone" ? "page.change.undone" : "page.change", { "title": r.title, "when": whenText(r.at, nowMs) }) }
  })

  var state = i.reading === true ? "reading" : (str(i.failed) !== "" ? "failed" : "")
  var why = ""
  if (state === "" && info && info.aboutWhy) {
    why = COPY.hasOwnProperty("page.why." + info.aboutWhy) ? copy("page.why." + info.aboutWhy) : copy("page.why.other")
  }
  return {
    "name": name,
    "ref": ref,
    "link": link,
    "title": firstToc && firstToc.title ? firstToc.title : (name || link),
    "summary": about && about.summary ? about.summary : (firstToc && firstToc.notes ? firstToc.notes : ""),
    "facts": facts.join(" · "),
    "source": ref ? (SOURCE_WORDS[sourceKind(ref)] || copy("installed.source.unknown")) : "",
    "homepage": about ? about.homepage : "",
    "clients": clients,
    "notes": notes,
    "moreUrl": moreUrl,
    "folders": folders,
    "needs": needs,
    "neededBy": neededBy,
    "errors": errors,
    "changes": changes,
    "state": state,
    "failed": str(i.failed),
    "why": why
  }
}

// How many of an addon's changes its page lists (the comment .23 left for 1n2p.30).
var ADDON_PAGE_CHANGES = 5

// -------------------------------------------------------------- the Errors view (1n2p.27)

// Who most likely threw an addon's errors, in words (design doc §6.6), from its kind,
// its strongest error's confidence and whether a blocked call named it.
function errorSourceText(a) {
  var blocked = (Array.isArray(a.errors) ? a.errors : []).some(function(e) { return e.method === "protected_call" })
  if (blocked) return copy("errors.source.blocked", { "addon": a.addon })
  if (a.kind === "library") return copy("errors.source.library", { "addon": a.addon })
  if (a.kind === "blizzard") return copy("errors.source.blizzard")
  if (a.kind === "unknown") return copy("errors.source.unknown")
  return copy(a.confidence === "high" ? "errors.source.high" : a.confidence === "medium" ? "errors.source.medium" : "errors.source.low")
}

// One error's message as the widget may show it (M3-D21, design doc §6.6): Lua's and the
// client's own messages as written, an addon's own message only as its length.
function errorMessageText(e) {
  if (e.messageKind === "custom" || e.message === null) {
    return copy("errors.message.custom", { "n": e.messageChars === null ? 0 : e.messageChars, "chars": e.messageChars === null ? "?" : e.messageChars })
  }
  return e.message
}

// A stack frame is the addon's own when its path starts in the addon's folder.
function ownFrame(frame, addon) {
  var f = str(frame)
  return addon !== "" && f.indexOf(addon + "/") === 0
}

// The Errors view, in the filter (1n2p.27, design doc §2.3, §6.6): per client of `errors`,
// its header (its player name, its last play session's age, its new errors), then a
// card when it records no errors (Install BugGrabber where a build is known), each addon
// with errors, the ones with new errors first, then the most recent; a read cut short
// and the errors other players sent are lines. Each addon row has `index`, its cursor
// position across sections.
function errorSections(errors, statusTargets, filter, nowMs) {
  var targets = errors && Array.isArray(errors.targets) ? errors.targets : []
  var sections = []
  var index = 0
  for (var i = 0; i < targets.length; i++) {
    var e = targets[i]
    if (filter && e.id !== filter) continue
    var status = targetById(statusTargets, e.id)
    var client = sheetClient(statusTargets, e.id, "")
    var count = []
    var lastSession = status && status.errors ? str(status.errors.lastSessionAt) : ""
    if (lastSession) count.push(copy("errors.last_session", { "age": sinceText(lastSession, nowMs) || dateText(lastSession) }))
    if (e["new"] > 0) count.push(copy("errors.new", { "n": e["new"] }))
    var addons = stableSort(e.addons.slice(), function(x, y) {
      if ((x["new"] > 0) !== (y["new"] > 0)) return x["new"] > 0 ? -1 : 1
      if (x.lastSeen !== y.lastSeen) return x.lastSeen > y.lastSeen ? -1 : 1
      return y.count - x.count
    })
    var rows = addons.map(function(a) {
      var meta = [plural(a.count, "time")]
      if (a["new"] > 0) meta.push(copy("errors.new", { "n": a["new"] }))
      if (a.lastSeen) meta.push(copy("errors.last_seen", { "age": sinceText(a.lastSeen, nowMs) || dateText(a.lastSeen) }))
      var first = a.errors.length ? a.errors[0] : null
      var chips = a.updateAvailable ? [{ "text": copy("errors.update_chip"), "urgent": false }] : []
      var actions = []
      if (a.updateAvailable) actions.push({ "name": "update", "glyph": String.fromCodePoint(0xF0CE1), "tooltip": withKey(copy("errors.action.update", { "addon": a.addon }), "update", "errors") })
      actions.push({ "name": "copy", "glyph": String.fromCodePoint(0xF018F), "tooltip": withKey(copy("errors.action.copy"), "copy", "errors") })
      actions.push({ "name": "restore", "glyph": String.fromCodePoint(0xF006F), "tooltip": withKey(copy("installed.action.restore"), "restore", "errors") })
      return {
        "kind": "addon", "key": "a/" + e.id + "/" + a.addon, "index": index++, "target": e.id, "client": client,
        "addon": a.addon, "version": a.version, "latest": a.latest, "updateAvailable": a.updateAvailable,
        "count": a.count, "new": a["new"], "source": errorSourceText(a), "meta": [errorSourceText(a)].concat(meta).join(" · "),
        "message": first ? errorMessageText(first) : "", "band": a["new"] > 0 ? "urgent" : "", "chips": chips,
        "actions": actions, "entry": a
      }
    })
    var lines = []
    if (e.partial) lines.push(copy("errors.partial"))
    if (e.received > 0) lines.push(copy("errors.received", { "n": e.received }))
    sections.push({
      "target": e.id, "title": client, "count": count.join(" · "), "lines": lines, "rows": rows,
      // No BugGrabber here: a card, never a row among errors (§2.3).
      "capture": e.capture === "none" ? {
        "text": copy(e.install ? "errors.capture.none" : "errors.capture.unknown", { "client": client }),
        "action": e.install && e.install.runnable ? e.install : null
      } : null,
      "empty": e.capture !== "none" && rows.length === 0 && !e.partial
    })
  }
  return sections
}

// The rows of errorSections, in cursor order.
function errorRows(sections) {
  var out = []
  for (var i = 0; i < sections.length; i++) out = out.concat(sections[i].rows)
  return out
}

// The error page of one addon row (§2.4): each distinct error, its message as the
// widget may show it, where, how often, whether it is new and when it was last seen,
// and its stack, the addon's own frames marked.
function errorPage(row, nowMs) {
  var entry = row && row.entry ? row.entry : { "errors": [] }
  return {
    "title": str(row ? row.addon : ""),
    "subtitle": row ? [str(row.source), plural(num(row.count), "time"), str(row.client)].filter(function(p) { return p !== "" }).join(" · ") : "",
    "errors": entry.errors.map(function(e) {
      var meta = [plural(e.count, "time")]
      if (e["new"]) meta.push(copy("errors.new_one"))
      if (e.lastSeen) meta.push(copy("errors.last_seen", { "age": sinceText(e.lastSeen, nowMs) || dateText(e.lastSeen) }))
      return {
        "fingerprint": e.fingerprint, "message": errorMessageText(e), "location": str(e.location), "meta": meta.join(" · "),
        "stack": e.stack.map(function(f) { return { "text": f, "own": ownFrame(f, entry.addon) } })
      }
    })
  }
}

// `y`'s report (design doc §6.6): the addon, its version, the client and its version,
// and each error's strict text (an addon's own message as its length) with its stack,
// as plain text to paste in a bug report. Never raw details, so never another player's
// name.
function errorReport(row, statusTargets) {
  if (!row || !row.entry) return ""
  var status = targetById(statusTargets, row.target)
  var lines = [copy("errors.report.head", { "addon": row.addon, "version": row.version || "?", "client": row.client,
    "client_version": status && status.version ? " " + status.version : "" })]
  row.entry.errors.forEach(function(e) {
    lines.push("")
    lines.push(errorMessageText(e))
    lines.push("  " + copy("errors.report.where", { "location": str(e.location) || "?", "times": plural(e.count, "time") }))
    e.stack.forEach(function(f) { lines.push("  " + f) })
  })
  return lines.join("\n")
}

// The view the bar's middle click opens (design doc §2.5): Errors with new Lua errors,
// else Issues with a P0 or P1 problem, else Updates.
function urgentView(summary) {
  if (summary && summary.errors && summary.errors["new"] > 0) return "errors"
  if (summary && summary.issuesUrgent > 0) return "issues"
  return "updates"
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

// Live search (design doc §2.3): a query is searched once typing has paused this long,
// and only from this many characters (Enter searches a shorter one at once). Each search
// can cost a GitHub search request (10 a minute without a token): the pause and the
// minimum keep a typed word to one or two.
var SEARCH_DEBOUNCE_MS = 400
var SEARCH_MIN_CHARS = 3

// Whether the box's text is searched as it is typed (after the pause).
function searchesLive(text) {
  var t = str(text).trim()
  return t.length >= SEARCH_MIN_CHARS && !searchLink(t)
}

// A pasted ref or link (README "Sources and refs"): the ref prefixes the CLI reads, and
// the https hosts it resolves links from. The widget only knows the shape; `info`
// resolves the text, and an install takes the ref `info` answered, never the text the
// player typed (M3-D2: typed text fills read-only templates only).
var LINK_PREFIXES = { "gh": "github", "git": "git", "wowi": "wowi", "tukui": "tukui", "wowace": "wowace", "url": "url" }
var LINK_HOSTS = {
  "github.com": "github", "www.github.com": "github",
  "wowinterface.com": "wowi", "www.wowinterface.com": "wowi", "cdn.wowinterface.com": "wowi",
  "tukui.org": "tukui", "www.tukui.org": "tukui",
  "wowace.com": "wowace", "www.wowace.com": "wowace", "repos.wowace.com": "wowace"
}
var LINK_MAX_CHARS = 512

// `{source, text}` when the box holds a ref or a link the CLI resolves, else null (the
// text is searched as words). Shapes, not lookups:
//   - `gh:owner/repo`, `wowi:<digits>`, `tukui:<slug>`, `wowace:<slug>…`, and `git:` or
//     `url:` followed by an https address;
//   - `https://` and a host above, with a path (GitHub's names an owner and a repository).
// Whitespace, control characters, credentials or a port in the host, and text past
// LINK_MAX_CHARS never are.
function searchLink(text) {
  var t = str(text).trim()
  if (t === "" || t.length > LINK_MAX_CHARS || /[\s\u0000-\u001f\u007f]/.test(t)) return null
  var prefixed = /^([a-z]+):(.+)$/.exec(t)
  if (prefixed && LINK_PREFIXES.hasOwnProperty(prefixed[1])) {
    var kind = prefixed[1]
    var rest = prefixed[2]
    var shaped = kind === "gh" ? /^[^\/]+\/[^\/]+$/.test(rest)
      : kind === "wowi" ? /^[0-9]+$/.test(rest)
      : kind === "git" || kind === "url" ? /^https:\/\/[^\/?#]+\//i.test(rest)
      : /^[^\/:]/.test(rest)
    return shaped ? { "source": LINK_PREFIXES[kind], "text": t } : null
  }
  var link = /^https:\/\/([^\/?#]+)([\/?#].*)?$/i.exec(t)
  if (!link) return null
  var host = link[1].toLowerCase()
  if (!LINK_HOSTS.hasOwnProperty(host)) return null
  var path = String(link[2] || "").split(/[?#]/)[0]
  var source = LINK_HOSTS[host]
  if (source === "github" ? !/^\/[^\/]+\/[^\/]+/.test(path) : path.replace(/\//g, "") === "") return null
  return { "source": source, "text": t }
}

// What the search box's hint says each verdict is, for one client: the words, the glyph
// (§5.4), and its rank (the best a result has across clients decides its row's glyph).
var SEARCH_VERDICTS = {
  "yes": { "word": "search.verdict.works", "glyph": 0xF05E0, "rank": 4 },
  "likely": { "word": "search.verdict.likely", "glyph": 0xF05E0, "rank": 3 },
  "unknown": { "word": "search.verdict.unknown", "glyph": 0xF0625, "rank": 2 },
  "abandoned": { "word": "search.verdict.abandoned", "glyph": 0xF02FD, "rank": 1 },
  "unlikely": { "word": "search.verdict.not", "glyph": 0xF02FD, "rank": 0 },
  "no": { "word": "search.verdict.not", "glyph": 0xF02FD, "rank": 0 }
}

function searchVerdict(hint) {
  return SEARCH_VERDICTS.hasOwnProperty(hint) ? SEARCH_VERDICTS[hint] : SEARCH_VERDICTS.unknown
}

// Where a result is installed already, from `list -v` (reduced): the clients whose
// records hold its ref, or its name (ignoring case), each `{id, version}`.
function installedOf(result, lists) {
  return installedIn(result, installedIndex(lists))
}

// `list -v` (reduced) as installedIn looks it up: per client, the position of the first
// addon of each ref and of each name (lowercased). Built once for a page of results, so
// each result is looked up instead of scanning every addon: the catalog's 500 rows over
// 500 addons were half a million comparisons under Qt's engine (1n2p.38).
function installedIndex(lists) {
  var listTargets = lists && Array.isArray(lists.targets) ? lists.targets : []
  return listTargets.map(function(t) {
    var addons = Array.isArray(t.addons) ? t.addons : []
    var byRef = new Map()
    var byName = new Map()
    for (var i = 0; i < addons.length; i++) {
      var ref = str(addons[i].ref)
      var name = str(addons[i].name).toLowerCase()
      if (ref !== "" && !byRef.has(ref)) byRef.set(ref, i)
      if (name !== "" && !byName.has(name)) byName.set(name, i)
    }
    return { "id": str(t.id), "addons": addons, "byRef": byRef, "byName": byName }
  })
}

// installedOf, from installedIndex's index: on each client, its first addon with the
// result's ref or name.
function installedIn(result, index) {
  var name = str(result && result.name).toLowerCase()
  var ref = str(result && result.ref)
  var out = []
  index.forEach(function(t) {
    var at = ref !== "" && t.byRef.has(ref) ? t.byRef.get(ref) : -1
    var named = name !== "" && t.byName.has(name) ? t.byName.get(name) : -1
    if (named >= 0 && (at < 0 || named < at)) at = named
    if (at >= 0) out.push({ "id": t.id, "version": str(t.addons[at].version) })
  })
  return out
}

// Where a search result may be installed: every client on this computer with its hint
// and what that means in words (`words`, as the chooser lists it); only `yes` and
// `likely` are eligible (the CLI's own rule for its `commands`), and never a client that
// has it already (`installed`). `lists` (`list -v`, reduced) is optional.
function installTargets(result, statusTargets, lists) {
  return targetsFor(result, statusTargets, installedOf(result, lists))
}

// installTargets, with the result's installedOf already found.
function targetsFor(result, statusTargets, installed) {
  var hints = result && result.hints ? result.hints : {}
  return liveTargets(statusTargets).map(function(t) {
    var hint = hints.hasOwnProperty(t.id) ? hints[t.id] : "unknown"
    var client = str(t.displayName) || str(t.id)
    var has = installed.some(function(i) { return i.id === t.id })
    return { "id": t.id, "client": client, "hint": hint, "installed": has,
      "words": has ? copy("search.verdict.installed", { "client": client }) : copy(searchVerdict(hint).word, { "client": client }),
      "eligible": !has && INSTALLABLE_HINTS.indexOf(hint) >= 0 }
  })
}

// One result's verdict in the scope (design doc §2.3, §6.3): `{words, glyph, action,
// note}`. For one client, its own hint in words; under All, how many clients it works
// on. `action` is what the row offers: "install" where a client is eligible, "check"
// (its page, whose `info` read decides) when no client's hint is known, else "" (Enter
// still opens its page). `note` is what `i` says when it installs nothing.
function searchVerdictOf(targets, filter) {
  var scoped = filter ? targets.filter(function(t) { return t.id === filter }) : targets
  if (scoped.length === 1) {
    var t = scoped[0]
    var v = searchVerdict(t.hint)
    var action = t.eligible ? "install" : (!t.installed && t.hint === "unknown" ? "check" : "")
    var note = t.installed ? copy("search.note.installed", { "client": t.client })
      : t.hint === "unknown" ? copy("search.note.unknown") : copy("search.note.not", { "words": copy(v.word, { "client": t.client }) })
    return { "words": copy(v.word, { "client": t.client }), "glyph": v.glyph, "action": action, "note": action === "install" ? "" : note }
  }
  // The words count every client, the ones that have it too; what `i` may install
  // leaves those out.
  var count = function(hint) { return scoped.filter(function(t) { return t.hint === hint }).length }
  var m = scoped.length
  var works = count("yes")
  var likely = count("likely")
  var best = scoped.reduce(function(at, t) { return searchVerdict(t.hint).rank > searchVerdict(at).rank ? t.hint : at }, "no")
  var unknown = works + likely === 0 && scoped.some(function(t) { return t.hint === "unknown" })
  var words = works > 0 && likely > 0 ? copy("search.verdict.all.works_likely", { "n": works, "m": m, "k": likely })
    : works > 0 ? copy("search.verdict.all.works", { "n": works, "m": m })
    : likely > 0 ? copy("search.verdict.all.likely", { "n": likely, "m": m })
    : unknown ? copy("search.verdict.all.unknown")
    : best === "abandoned" ? copy("search.verdict.abandoned") : copy("search.verdict.all.not")
  var eligible = scoped.some(function(t) { return t.eligible })
  var check = !eligible && unknown && scoped.some(function(t) { return t.hint === "unknown" && !t.installed })
  var note = eligible ? "" : check ? copy("search.note.unknown")
    : works + likely > 0 ? copy("search.note.installed_where") : copy("search.note.not", { "words": words })
  return { "words": words, "glyph": searchVerdict(best).glyph, "action": eligible ? "install" : (check ? "check" : ""), "note": note }
}

// The installed marker (a chip): "installed 5.19.2" on the scope's client, else
// "installed on Retail" (or "on 3 clients") where other clients have it; null when no
// client does.
function installedChip(installed, statusTargets, filter) {
  if (installed.length === 0) return null
  var here = filter ? installed.filter(function(i) { return i.id === filter })[0] : null
  if (here) return { "text": here.version ? copy("search.chip.installed.version", { "version": here.version }) : copy("search.chip.installed"), "urgent": false }
  var names = installed.map(function(i) { return sheetClient(statusTargets, i.id, "") })
  return { "text": names.length > 2 ? copy("search.chip.installed.clients", { "n": names.length })
    : copy("search.chip.installed.on", { "clients": joinNames(names) }), "urgent": false }
}

// The Search view's rows (1n2p.25, design doc §2.3, §5.3), from what the panel holds:
// `rows` (the shown search answer's results, reduced), `link` (searchLink of the box's
// text, or null), `filter`, `statusTargets` (`status`'s clients), `lists` (`list -v`,
// reduced, for the installed marker) and `nowMs`. A link gives one row, "Install from
// GitHub", whose page resolves it; each result gives an AddonRow's fields: its name, its
// summary (`detail`), its verdict first on the meta line with the source, popularity,
// age and author after it, the verdict's glyph, the installed and catalog chips, and the
// cursored row's actions. `action` is the row's install action ("install", "check" or
// ""), `note` what `i` says when it installs nothing, and `targets` its clients with
// their verdicts (installTargets, for the chooser).
function searchRows(input) {
  var i = input && typeof input === "object" ? input : {}
  var filter = str(i.filter)
  var statusTargets = Array.isArray(i.statusTargets) ? i.statusTargets : []
  var nowMs = typeof i.nowMs === "number" ? i.nowMs : Date.now()
  if (i.link && typeof i.link === "object") {
    var source = copy("search.link.source." + str(i.link.source))
    return [{
      "key": "link", "kind": "link", "name": "", "ref": "", "link": str(i.link.text),
      "title": copy("search.link.title", { "source": source }), "detail": "", "meta": str(i.link.text),
      "glyph": String.fromCodePoint(0xF059F), "chips": [], "action": "open", "note": "", "targets": [],
      "actions": [{ "name": "enter", "glyph": String.fromCodePoint(0xF0142), "tooltip": withKey(copy("search.action.open_link"), "enter", "search") }]
    }]
  }
  var rows = Array.isArray(i.rows) ? i.rows : []
  var index = installedIndex(i.lists)
  var live = liveTargets(statusTargets)
  return rows.map(function(r, at) {
    var everywhere = installedIn(r, index)
    var targets = targetsFor(r, statusTargets, everywhere)
    var verdict = searchVerdictOf(targets, filter)
    var installed = everywhere.filter(function(x) { return !!targetById(live, x.id) })
    var chips = []
    var marker = installedChip(installed, statusTargets, filter)
    if (marker) chips.push(marker)
    if (r.source === "catalog") chips.push({ "text": copy("search.chip.catalog"), "urgent": false })
    var meta = [verdict.words]
    var kind = sourceKind(r.ref)
    meta.push(r.source === "catalog" || !SEARCH_SOURCES.hasOwnProperty(r.source) ? (SOURCE_WORDS[kind] || copy("installed.source.unknown"))
      : copy(SEARCH_SOURCES[r.source]))
    var popularity = popularityText(r)
    if (popularity) meta.push(popularity)
    var age = r.updatedAt ? sinceText(r.updatedAt, nowMs) : ""
    if (age) meta.push(copy("search.meta.updated", { "age": age }))
    if (r.author) meta.push(copy("page.by", { "author": r.author }))
    var actions = []
    if (verdict.action === "install") {
      actions.push({ "name": "install", "glyph": String.fromCodePoint(0xF0415), "tooltip": withKey(copy("search.action.install"), "install", "search") })
    } else if (verdict.action === "check") {
      actions.push({ "name": "enter", "glyph": String.fromCodePoint(0xF0142), "tooltip": withKey(copy("search.action.check"), "enter", "search") })
    }
    // Installed here: on the scope's client, or under All wherever it works (nothing
    // left to install).
    var here = filter ? installed.some(function(x) { return x.id === filter })
      : installed.length > 0 && verdict.action !== "install"
    return {
      "key": "r/" + at + "/" + str(r.ref), "kind": "result", "name": str(r.name), "ref": str(r.ref), "link": "",
      "title": str(r.name), "detail": str(r.summary), "meta": meta.join(" · "), "glyph": String.fromCodePoint(verdict.glyph),
      "chips": chips, "action": verdict.action, "note": verdict.note, "targets": targets, "actions": actions,
      "installed": here
    }
  })
}

// The Search view with an empty box (1n2p.26): the built-in catalog (`search --catalog`,
// read for the scope: with `-t` for one client, so a name with variants is that client's
// build), as searchRows' rows, those not installed here first, each part in the CLI's
// order (by name). The CLI already leaves out what fits no client in scope. Every row is
// the catalog's, which the view's header line says, so no row carries its chip.
function catalogRows(input) {
  var mark = copy("search.chip.catalog")
  var rows = searchRows(input).map(function(r) {
    return Object.assign({}, r, { "chips": r.chips.filter(function(c) { return c.text !== mark }) })
  })
  return rows.filter(function(r) { return !r.installed }).concat(rows.filter(function(r) { return r.installed }))
}

// The catalog read for a scope: the template and its slots (All reads every client).
function catalogRequest(filter) {
  if (filter) return { "template": "catalog_target", "slots": { "target": filter } }
  return { "template": "catalog", "slots": {} }
}

// A search hit's own source, as a word (a catalog hit takes its ref's).
var SEARCH_SOURCES = { "wowi": "source.wowi", "tukui": "source.tukui", "github": "source.github" }

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

// -------------------------------------------------------------- Backups, a backup's page and the restore chooser (F9, 1n2p.29)

// "412 kB", "15.9 MB", "212 GB" (design doc §6.9): decimal units, as the desktop's file
// manager shows them, with one decimal under 100.
function bytesText(n) {
  var value = num(n)
  if (value < 1000) return value + " B"
  var units = ["kB", "MB", "GB", "TB"]
  var at = -1
  while (value >= 999.5 && at < units.length - 1) {
    value /= 1000
    at++
  }
  return (value < 99.95 ? (Math.round(value * 10) / 10).toFixed(1) : String(Math.round(value))) + " " + units[at]
}

// `parts` without the empty ones, joined by " · " (design doc §6.9: never a trailing
// separator).
function dotted(parts) {
  return parts.filter(function(p) { return typeof p === "string" && p !== "" }).join(" · ")
}

// A backup's kind in words (design doc §6.3): "made by you", "automatic, after play".
// `short` drops "automatic, " where a trigger says when ("after play"), as a row's title
// does.
function backupKindText(b, short) {
  var kind = str(b && b.kind)
  var trigger = str(b && b.trigger)
  if (kind === "auto" && (trigger === "pre_change" || trigger === "post_session")) {
    return copy((short ? "backup.kind.short." : "backup.kind.auto.") + trigger)
  }
  return COPY.hasOwnProperty("backup.kind." + kind) ? copy("backup.kind." + kind) : kind
}

// What a backup proves (M3-D47, design doc §6.3): "checked when made", "checked when
// imported" or "checked since" for a verified one; nothing for metadata only. A damaged
// one is a chip.
function backupProof(b) {
  if (!b || b.integrity !== "verified") return ""
  var baseline = str(b.baseline)
  return COPY.hasOwnProperty("backup.proof." + baseline) ? copy("backup.proof." + baseline) : copy("backup.proof.verified")
}

// A client's automatic backups in words (design doc §7, the Backups card): on, off, or
// what keeps them from working; "" when nothing says.
function autoSentence(auto) {
  if (!auto || !auto.mode) return ""
  if (auto.mode === "off") return copy("backups.auto.off")
  var result = auto.last ? auto.last.result : ""
  if (result === "space_low") return copy("backups.auto.space_low")
  // Only `daily` needs clones: `daily-copy` copies where the drive cannot clone.
  if (auto.mode === "daily" && (auto.capability === "unsupported" || result === "no_reflink")) return copy("backups.auto.no_reflink")
  if (result === "failed") return copy("backups.auto.failed")
  return copy("backups.auto.on")
}

var BACKUP_GLYPH = 0xF003C
var RESTORE_GLYPH = 0xF006F
var DELETE_GLYPH = 0xF01B4

// When a backup's content was captured, as a player says it (§6.9).
function backupWhen(b, nowMs) {
  return whenText(b.capturedAt, nowMs) || dateText(b.capturedAt) || copy("backups.when.unknown")
}

// One backup as a row (design doc §5.3, UX-14, UX-40): when and why on the first line
// ("3 h ago · after play · “before the raid”"), its size and what it proves on the
// second; markers as chips ("in use"; "damaged", urgent, with the urgent band; "can't be
// restored"); and the cursored row's actions: restore a restorable backup, delete one
// no restore, export or verify is reading (M3-D48).
function backupRow(t, b, index, nowMs) {
  var damaged = b.integrity === "damaged"
  var chips = []
  if (b.busy) chips.push({ "text": copy("backups.chip.busy"), "urgent": false })
  if (damaged) chips.push({ "text": copy("backups.chip.damaged"), "urgent": true })
  else if (!b.restorable) chips.push({ "text": copy("backups.chip.unrestorable"), "urgent": false })
  var actions = []
  if (b.restorable) {
    actions.push({ "name": "restore", "glyph": String.fromCodePoint(RESTORE_GLYPH), "tooltip": withKey(copy("backups.action.restore"), "restore", "backups") })
  }
  if (!b.busy) {
    actions.push({ "name": "remove", "glyph": String.fromCodePoint(DELETE_GLYPH), "tooltip": withKey(copy("backups.action.delete"), "remove", "backups") })
  }
  return {
    "key": t.id + "/" + b.id, "index": index, "target": t.id, "id": b.id, "busy": b.busy === true, "restorable": b.restorable === true,
    "damaged": damaged, "at": dateText(b.capturedAt),
    "title": dotted([backupWhen(b, nowMs), backupKindText(b, true), b.reason ? copy("backups.reason", { "reason": b.reason }) : ""]),
    "meta": dotted([plural(num(b.files), "file"), bytesText(b.bytes), backupProof(b), b.consistency === "best_effort" ? copy("backups.writing") : ""]),
    "chips": chips, "band": damaged ? "urgent" : "", "glyph": String.fromCodePoint(BACKUP_GLYPH), "actions": actions
  }
}

// A client card's lines (design doc §2.3, §7): its automatic backups and free space; how
// many backups and the room they take, with the newest's age ("No backups of Retail
// yet." with none); its undo data, when `status` measured it.
function backupCardLines(t, status, client, nowMs) {
  var lines = []
  var free = typeof t.freeBytes === "number" ? copy("backups.free", { "size": bytesText(t.freeBytes) }) : ""
  var first = dotted([autoSentence(t.auto), free])
  if (first) lines.push(first)
  if (t.total === 0) {
    lines.push(copy("state.empty.backups.client", { "client": client }))
  } else {
    lines.push(dotted([copy("backups.count", { "n": t.total }), typeof t.bytes === "number" ? bytesText(t.bytes) : "",
      t.backups.length ? copy("backups.newest", { "when": backupWhen(t.backups[0], nowMs) }) : "",
      t.shown < t.total ? copy("backups.shown", { "shown": t.shown, "total": t.total }) : ""]))
  }
  var undo = status ? status.undoSnapshots : null
  if (undo && undo.count > 0) {
    lines.push(dotted([copy("backups.undo", { "n": undo.count }),
      typeof undo.bytes === "number" ? copy(undo.truncated ? "backups.undo.at_least" : "backups.undo.size", { "size": bytesText(undo.bytes) }) : "",
      undo.prunable > 0 ? copy("backups.undo.due", { "n": undo.prunable }) : ""]))
  }
  return lines
}

// The Backups view in the filter (design doc §2.3, 1n2p.29): a card per client (its
// player name and whether it runs; backUp, the client's `b`; its lines; cleanUp, true
// when retention would prune some of its undo data now), then its backups as rows,
// newest first as listed, walked by one cursor (`index`). The cleanup itself is the
// client's retention problem in Issues, repaired there like any problem: the widget
// lists problems generically and never matches one by its id (M4-D48), and composes no
// deletion of its own.
function backupSections(backups, filter, nowMs, statusTargets) {
  var listed = backups && Array.isArray(backups.targets) ? backups.targets : []
  var known = Array.isArray(statusTargets) ? statusTargets : []
  // `backup list` names only clients with backups: every other client `status` knows
  // gets its card, so "this drive can't make them for free" shows exactly where none is
  // ever made.
  var targets = listed.slice()
  for (var s = 0; s < known.length; s++) {
    var st = known[s]
    if (!st.backups || targetById(listed, st.id)) continue
    targets.push({ "id": st.id, "total": 0, "shown": 0, "bytes": st.backups.bytes, "freeBytes": st.backups.freeBytes,
      "auto": st.backups.auto, "backups": [] })
  }
  targets = stableSort(targets, function(a, b) { return a.id < b.id ? -1 : a.id > b.id ? 1 : 0 })
  var sections = []
  var index = 0
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i]
    if (filter && t.id !== filter) continue
    var status = targetById(known, t.id)
    var rows = []
    for (var j = 0; j < t.backups.length; j++) rows.push(backupRow(t, t.backups[j], index++, nowMs))
    // Titled with the client's player name, whether it runs (design doc §2.2).
    var group = clientGroup(known, t.id, "")
    sections.push({ "target": t.id, "title": group.title, "count": group.count,
      "lines": backupCardLines(t, status, sheetClient(known, t.id, ""), nowMs),
      "backUp": backupRequest(t.id, known),
      "cleanUp": !!status && !!status.undoSnapshots && status.undoSnapshots.prunable > 0, "rows": rows })
  }
  return sections
}

// `b`: the filtered client, or every client (no confirmation: creation is undoable and
// expected, M3-D9). The label names the client as `targets` (`status`'s) does.
function backupRequest(filter, targets) {
  if (filter) return { "template": "backup_target", "slots": { "target": filter }, "label": "Back up " + sheetClient(targets, filter, "") }
  return { "template": "backup_all", "slots": {}, "label": "Back up all" }
}

// `x` on a backup: its deletion, previewed and bound (the plan sheet names it by date
// and kind). The label, recorded as the change's reason, names it by its client and the
// absolute time it was captured, never its id (UX-29).
function pruneRequest(row, targets) {
  return { "template": "prune_backup", "slots": { "target": row.target, "backup": row.id },
    "label": copy("backups.delete.label", { "owner": possessive(sheetClient(targets, row.target, "")), "at": row.at || row.id }) }
}

// Why restores on a client are off (M3-D29, design doc §6.4): it runs, or nobody can
// tell (the CLI refuses then too); "" when they are on.
function restoreBlocked(statusTargets, target) {
  var t = targetById(statusTargets, target)
  if (!t) return ""
  var client = sheetClient(statusTargets, target, "")
  if (t.running) return copy("restore.blocked", { "client": client })
  if (t.runningUnknown) return copy("restore.blocked.unknown", { "client": client })
  return ""
}

// The addons whose settings backup `id` holds, from the service's view-bound `backup list
// -v` read (`{selection, data, failed}`): `{state, addons}`, state "reading" until the
// read for its client answers, "failed", "none" (it records none) or "ready"; each addon
// `{name, version}`, the version that wrote its settings.
function backupAddons(detail, target, id) {
  if (!detail || !detail.selection || detail.selection.target !== target) return { "state": "reading", "addons": [] }
  if (detail.failed) return { "state": "failed", "addons": [] }
  var t = targetById(detail.data && detail.data.targets, target)
  var addons = []
  for (var i = 0; t && i < t.backups.length; i++) {
    if (t.backups[i].id === id && Array.isArray(t.backups[i].addons)) addons = t.backups[i].addons
  }
  return { "state": addons.length ? "ready" : "none", "addons": addons }
}

// A backup's page (design doc §2.4, 1n2p.29), pushed by Enter on its row: its client,
// when and its kind; when exactly, its size and what it proves; its reason; the addons
// whose settings it holds, each with the version that wrote them (each takes the cursor:
// Enter restores that addon's settings from it); Restore… and Delete…. Null when the
// backup is no longer listed.
function backupPage(backups, detail, target, id, statusTargets, nowMs) {
  var t = targetById(backups && backups.targets, target)
  var b = null
  for (var i = 0; t && i < t.backups.length; i++) if (t.backups[i].id === id) b = t.backups[i]
  if (!b) return null
  var row = backupRow(t, b, 0, nowMs)
  var client = sheetClient(statusTargets, target, "")
  var held = backupAddons(detail, target, id)
  return {
    "target": target, "id": id, "client": client, "at": row.at,
    "header": dotted([client, backupWhen(b, nowMs), backupKindText(b, false)]),
    "facts": dotted([row.at, row.meta]),
    "reason": b.reason ? copy("backups.reason", { "reason": b.reason }) : "",
    "chips": row.chips, "restorable": row.restorable, "busy": row.busy,
    "blocked": row.restorable ? restoreBlocked(statusTargets, target) : "",
    "addonsState": held.state,
    "addonsText": held.state === "ready" ? "" : copy("backups.page." + held.state),
    "addons": held.addons.map(function(a, k) {
      return { "index": k, "name": a.name, "version": str(a.version), "text": a.name + (a.version ? " " + a.version : "") }
    })
  }
}

// A restore from `backup` on `target`, previewed in the plan sheet first: one addon's
// settings (`addon`), or all of the client's (`addon` "").
function restoreRequest(target, backup, addon, targets) {
  if (addon) {
    return { "template": "restore_addon", "slots": { "target": target, "addon": addon, "backup": backup },
      "label": copy("restore.label.addon", { "owner": possessive(addon) }) }
  }
  return { "template": "restore", "slots": { "target": target, "backup": backup },
    "label": copy("restore.label.whole", { "owner": possessive(sheetClient(targets, target, "")) }) }
}

// The managed addons of `target`, for "One addon's settings…" until the backup's own list
// is read.
function managedAddons(lists, target) {
  var t = targetById(lists && lists.targets, target)
  return t ? t.addons.map(function(a) { return { "name": a.name, "version": "" } }) : []
}

// The restore chooser (design doc §2.4, 1n2p.29): which settings, from which backup.
// `input`: {origin, step, target, addon, backup, backups, detail, lists, statusTargets,
// nowMs}. From an addon (origin "addon") it opens on the newest restorable backup with
// that addon's settings first ("WeakAuras' settings (written by 5.19.2)"), all of the
// client's settings second, and the backup above them as a choice of its own (step
// "backup" lists the others, newest first, each with the version of the addon that
// wrote it). From a backup (origin "backup") it offers one addon's settings (step
// "addon": which addon) or all of them. Returns {title, from, blocked, choices, start,
// empty}: each choice {key, text, note, enabled, next, backup, run}, where `next` is the
// step it opens, `backup` the backup it picks and `run` the restore it previews
// ({template, slots, label}). The chooser decides no folder, file or version: the CLI's
// preview does (D7). While the client runs, or nobody can tell, nothing restores
// (M3-D29).
function restoreChooser(input) {
  var i = input || {}
  var target = str(i.target)
  var addon = str(i.addon)
  var origin = i.origin === "addon" ? "addon" : "backup"
  var step = i.step === "backup" || i.step === "addon" ? i.step : "scope"
  var nowMs = typeof i.nowMs === "number" ? i.nowMs : Date.now()
  var targets = Array.isArray(i.statusTargets) ? i.statusTargets : []
  var client = sheetClient(targets, target, "")
  var blocked = restoreBlocked(targets, target)
  // The client's `-v` read lists its backups too, the freshest: Backups' own list may not
  // be read yet (it is read with its view, 1n2p.38).
  var mine = !!i.detail && !!i.detail.selection && i.detail.selection.target === target
  var detail = mine && i.detail.data ? i.detail.data : null
  var detailed = targetById(detail && detail.targets, target)
  var listed = detailed || targetById(i.backups && i.backups.targets, target)
  var all = listed ? listed.backups : []
  var restorable = all.filter(function(b) { return b.restorable })
  // Neither read has answered: the chooser waits for them, it never says there is none.
  var waiting = !listed ? (mine && i.detail.failed ? copy("restore.failed", { "owner": possessive(client) }) : copy("restore.reading")) : ""
  // The version of `addon` that wrote each backup's settings, from the `-v` read.
  var versions = {}
  for (var d = 0; addon && detailed && d < detailed.backups.length; d++) {
    var held = Array.isArray(detailed.backups[d].addons) ? detailed.backups[d].addons : []
    for (var h = 0; h < held.length; h++) if (held[h].name === addon && held[h].version) versions[detailed.backups[d].id] = held[h].version
  }
  var backupId = str(i.backup) || (origin === "addon" && restorable.length ? restorable[0].id : "")
  var chosen = null
  for (var c = 0; c < all.length; c++) if (all[c].id === backupId) chosen = all[c]
  var describe = function(b) { return dotted([backupWhen(b, nowMs), backupKindText(b, true)]) }
  var restoreAddon = function(name) { return restoreRequest(target, backupId, name, targets) }
  var whole = { "key": "whole", "text": copy("restore.whole", { "owner": possessive(client) }), "note": copy("restore.whole.note"),
    "enabled": blocked === "", "next": "", "backup": "", "run": restoreRequest(target, backupId, "", targets) }
  var out = { "title": "", "from": "", "blocked": blocked, "choices": [], "start": 0, "empty": "" }
  if (step === "backup") {
    out.title = copy("restore.title.backup", { "owner": possessive(addon) })
    out.choices = restorable.map(function(b) {
      return { "key": b.id, "text": dotted([describe(b), versions[b.id] ? addon + " " + versions[b.id] : ""]), "note": "", "enabled": true,
        "next": "", "backup": b.id, "run": null }
    })
    for (var k = 0; k < out.choices.length; k++) if (out.choices[k].backup === backupId) out.start = k
    out.empty = waiting || copy("restore.none", { "client": client })
    return out
  }
  if (!chosen || !chosen.restorable) {
    out.title = origin === "addon" ? copy("restore.title.owner", { "owner": possessive(addon), "client": client }) : copy("restore.title.client", { "client": client })
    out.empty = chosen ? copy("restore.unrestorable") : (waiting || copy("restore.none", { "client": client }))
    return out
  }
  if (step === "addon") {
    out.title = copy("restore.title.addon", { "client": client })
    out.from = copy("restore.from", { "backup": describe(chosen) })
    var own = backupAddons(i.detail, target, backupId)
    var names = own.state === "ready" ? own.addons : managedAddons(i.lists, target)
    out.choices = names.map(function(a) {
      return { "key": a.name, "text": a.name + (a.version ? " " + a.version : ""), "note": "", "enabled": blocked === "",
        "next": "", "backup": "", "run": restoreAddon(a.name) }
    })
    out.empty = copy("restore.no_addons")
    return out
  }
  if (origin === "addon") {
    out.title = copy("restore.title.owner", { "owner": possessive(addon), "client": client })
    var version = versions[backupId] || ""
    out.choices = [
      { "key": "from", "text": copy("restore.from", { "backup": describe(chosen) }), "note": "", "enabled": true, "next": "backup",
        "backup": "", "run": null },
      { "key": "addon", "text": copy(version ? "restore.addon.written" : "restore.addon", { "owner": possessive(addon), "version": version }),
        "note": "", "enabled": blocked === "", "next": "", "backup": "", "run": restoreAddon(addon) },
      whole
    ]
    // The narrow fix first (design doc J6: r, Enter, y).
    out.start = 1
    return out
  }
  out.title = copy("restore.title.client", { "client": client })
  out.from = copy("restore.from", { "backup": describe(chosen) })
  out.choices = [
    { "key": "one", "text": copy("restore.one"), "note": "", "enabled": blocked === "", "next": "addon", "backup": "", "run": null },
    whole
  ]
  return out
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
// clients by the name a player knows them by, from `status`'s per-client top list.
// Nothing else of the errors reaches it.
function errorFacts(targets) {
  var names = []
  var counts = []
  var where = []
  var list = Array.isArray(targets) ? targets : []
  for (var i = 0; i < list.length; i++) {
    var errors = list[i].errors
    if (!errors || errors["new"] <= 0) continue
    where.push(str(list[i].displayName) || str(list[i].id))
    for (var j = 0; j < errors.top.length; j++) {
      if (errors.top[j]["new"] <= 0) continue
      names.push(errors.top[j].addon)
      counts.push(errors.top[j]["new"])
    }
  }
  return { "names": names, "counts": counts, "targets": where }
}

// What an update toast may say: how many updates wait, and on which clients, by name.
function updateFacts(targets, summary) {
  var ids = summary && summary.updates && Array.isArray(summary.updates.targets) ? summary.updates.targets : []
  var list = Array.isArray(targets) ? targets : []
  var where = ids.map(function(id) {
    var t = list.filter(function(x) { return x.id === id })[0]
    return t ? (str(t.displayName) || str(t.id)) : String(id)
  })
  return { "count": summary && summary.updates ? num(summary.updates.available) : 0, "targets": where }
}

// U+F0297 nf-md-gamepad_variant: the widget's own glyph.
var TOAST_GLYPH = String.fromCodePoint(0xF0297)

// One toast of a kind at most this often (1n2p.33): what rises meanwhile waits for it.
var TOAST_MIN_GAP_MS = 10 * 60 * 1000

// A toast as an argv for `omarchy-notification-send`, under the widget's own app name
// (never one that passes Do Not Disturb). A click opens the panel of `pluginId` at
// `view` through its IPC (`show`, which opens, never toggles; "" opens it where it
// would open anyway): a fixed argv, never a shell.
function toastArgv(title, body, urgency, pluginId, view) {
  return ["omarchy-notification-send", "--app-name", "omaddons", "-g", TOAST_GLYPH, "-u", urgency, title, body,
    "--exec", "omarchy-shell", String(pluginId), "show", String(view || "")]
}

// The errors part of a toast: "DBM 3 · WeakAuras 1".
function errorParts(names, counts) {
  var parts = []
  for (var i = 0; i < names.length; i++) parts.push(String(names[i]) + " " + num(counts[i]))
  return parts
}

// "Lua errors in your last session on Retail" · "DBM 3 · WeakAuras 1": only `{names,
// counts, targets}`, so no message, path or other error text can reach a toast (F7
// scan 7). A click opens Errors.
function errorToastArgv(facts, pluginId) {
  var names = Array.isArray(facts && facts.names) ? facts.names : []
  var counts = Array.isArray(facts && facts.counts) ? facts.counts : []
  var where = Array.isArray(facts && facts.targets) ? facts.targets : []
  var title = where.length ? copy("toast.errors.title.on", { "clients": joinNames(where.map(String)) }) : copy("toast.errors.title")
  return toastArgv(title, errorParts(names, counts).join(" · "), "normal", pluginId, "errors")
}

// "Addon updates" · "3 updates on Retail and Classic Era"; a click opens Updates.
function updateToastArgv(facts, pluginId) {
  var count = num(facts && facts.count)
  var where = Array.isArray(facts && facts.targets) ? facts.targets.map(String) : []
  var body = where.length ? copy("toast.updates.on", { "n": count, "clients": joinNames(where) }) : copy("toast.updates", { "n": count })
  return toastArgv(copy("toast.updates.title"), body, "low", pluginId, "updates")
}

// One toast for what a session held back (1n2p.33): "After your session" · "3 new Lua
// errors in DBM and WeakAuras · 2 updates", or, for both kinds rising at once outside a
// session, "Lua errors and addon updates". `kinds` `{errors, updates, session}` says
// which parts it carries; the errors part reads `{names, counts}` only, as
// errorToastArgv does. A click opens Errors when it names errors, else Updates.
function digestToastArgv(facts, updates, kinds, pluginId) {
  var parts = []
  var withErrors = !!(kinds && kinds.errors)
  if (withErrors) {
    var names = Array.isArray(facts && facts.names) ? facts.names : []
    var counts = Array.isArray(facts && facts.counts) ? facts.counts : []
    var total = 0
    for (var i = 0; i < counts.length; i++) total += num(counts[i])
    parts.push(copy("toast.digest.errors", { "n": total, "addons": joinNames(names.map(String)) }))
  }
  if (kinds && kinds.updates) parts.push(copy("toast.updates", { "n": num(updates && updates.count) }))
  var title = kinds && kinds.session === false ? copy("toast.digest.title.now") : copy("toast.digest.title")
  return toastArgv(title, parts.join(" · "), withErrors ? "normal" : "low", pluginId, withErrors ? "errors" : "updates")
}

// A panel action that finished while no panel was open: its result line, `normal` when
// it failed (the player asked for it), else `low`. A click opens the panel, where the
// result stays shown.
function resultToastArgv(line, ok, pluginId) {
  return toastArgv(copy("toast.result.title"), String(line), ok === false ? "normal" : "low", pluginId, "")
}

// Whether a game may be running: a client running, or one whose state could not be read
// (the safe side while the player may be streaming).
function playing(targets) {
  return (Array.isArray(targets) ? targets : []).some(function(t) {
    return !!t && (t.running === true || t.runningUnknown === true)
  })
}

// What the toasts hold back and when each kind last spoke.
function emptyToastState() {
  return { "held": { "errors": false, "updates": false }, "session": false, "lastMs": { "errors": 0, "updates": 0 } }
}

// What a compatible status announces (1n2p.33), pure: `state` from the status before,
// `input` `{first, before: {errorsNew, updates}, errorsNew, updates, playing,
// notifyErrors, notifyUpdates, whilePlaying, nowMs}`. Answers `{state, toast, kinds}`:
// `toast` is null, "errors", "updates" or "digest", and `kinds` what a digest carries.
// - Never on the first status: a fresh start floods no one.
// - A rise of a kind that is on is held. A held kind lets go when it is answered (its
//   count back to 0) or turned off.
// - While a game may run, nothing speaks unless `whilePlaying`: what rose waits for the
//   session's end, and then one digest says it all.
// - Otherwise each held kind speaks once its last toast is TOAST_MIN_GAP_MS old; two
//   kinds at once, or anything held through a session, are one digest.
function toastPlan(state, input) {
  var s = state && state.held && state.lastMs ? state : emptyToastState()
  var i = input || {}
  var held = { "errors": s.held.errors, "updates": s.held.updates }
  var lastMs = { "errors": num(s.lastMs.errors), "updates": num(s.lastMs.updates) }
  if (i.first === true) return { "state": { "held": { "errors": false, "updates": false }, "session": false, "lastMs": lastMs }, "toast": null, "kinds": null }
  var before = i.before || {}
  if (rose(before.errorsNew, i.errorsNew, false) && i.notifyErrors === true) held.errors = true
  if (rose(before.updates, i.updates, false) && i.notifyUpdates === true) held.updates = true
  held.errors = held.errors && i.notifyErrors === true && num(i.errorsNew) > 0
  held.updates = held.updates && i.notifyUpdates === true && num(i.updates) > 0
  var session = s.session === true && (held.errors || held.updates)
  if (i.playing === true && i.whilePlaying !== true) {
    return { "state": { "held": held, "session": held.errors || held.updates, "lastMs": lastMs }, "toast": null, "kinds": null }
  }
  var now = num(i.nowMs)
  var due = {
    "errors": held.errors && now - lastMs.errors >= TOAST_MIN_GAP_MS,
    "updates": held.updates && now - lastMs.updates >= TOAST_MIN_GAP_MS
  }
  var toast = null
  if (due.errors && due.updates) toast = "digest"
  else if ((due.errors || due.updates) && session) toast = "digest"
  else if (due.errors) toast = "errors"
  else if (due.updates) toast = "updates"
  for (var kind in due) {
    if (!due[kind]) continue
    held[kind] = false
    lastMs[kind] = now
  }
  return { "state": { "held": held, "session": session && (held.errors || held.updates), "lastMs": lastMs },
    "toast": toast, "kinds": toast === "digest" ? { "errors": due.errors, "updates": due.updates, "session": session } : null }
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
  // An install on several clients (omad-3rre).
  "sheet.title.install.each": "Install {addon} on {clients}?",
  "sheet.confirm.install.each": { "one": "Install on 1 client", "other": "Install on {n} clients" },
  "sheet.each.effect": "{client}: {text}",
  "each.result.installed": "Installed {addon} on {clients}",
  "each.result.failed": "{client} failed: {why}",
  "each.result.not_run": "not run on {clients}",
  "each.result.none": "Installed {addon} nowhere",
  "each.result.undo": "Undo takes back {owner} install; History has each",
  "each.skipped": "{client}: {why}",
  "each.stale": "changed since you looked, so it is shown again",
  "search.chooser.every": { "one": "Every client it works on (1)", "other": "Every client it works on ({n})" },
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
  "backup.kind.auto.pre_change": "automatic, before a change",
  "backup.kind.auto.post_session": "automatic, after play",
  "backup.kind.short.pre_change": "before a change",
  "backup.kind.short.post_session": "after play",
  "backup.proof.capture": "checked when made",
  "backup.proof.import": "checked when imported",
  "backup.proof.later": "checked since",
  "backup.proof.verified": "checked",

  // The Backups view (1n2p.29, design doc §2.3, §6.3, §7).
  "backups.auto.on": "Automatic backups: on",
  "backups.auto.off": "Automatic backups: off",
  "backups.auto.space_low": "Automatic backups: skipped: low disk space",
  "backups.auto.no_reflink": "Automatic backups: on, but this drive can't make them for free: back up by hand",
  "backups.auto.failed": "Automatic backups: on, but the last one failed",
  "backups.free": "{size} free",
  "backups.count": { "one": "1 backup", "other": "{n} backups" },
  "backups.newest": "newest {when}",
  "backups.shown": "{shown} of {total} shown",
  "backups.undo": { "one": "Undo data: 1 change", "other": "Undo data: {n} changes" },
  "backups.undo.size": "{size}",
  "backups.undo.at_least": "at least {size}",
  "backups.undo.due": { "one": "1 due for cleanup", "other": "{n} due for cleanup" },
  "backups.back_up": "Back up",
  "backups.clean_up": "Clean up undo data…",
  "backups.clean_up.tip": "Its repair is in Issues: it says what it deletes before it runs",
  "backups.when.unknown": "date unknown",
  "backups.reason": "“{reason}”",
  "backups.writing": "taken while the game was writing",
  "backups.chip.busy": "in use",
  "backups.chip.damaged": "damaged",
  "backups.chip.unrestorable": "can't be restored",
  "backups.action.restore": "Restore settings…",
  "backups.action.delete": "Delete…",
  "backups.delete.label": "Delete {owner} backup from {at}",
  "backups.busy": "This backup is in use. Try again when it's free.",
  "backups.page.held": "Addon settings in it",
  "backups.page.reading": "Reading which addons' settings it holds…",
  "backups.page.failed": "Couldn't read which addons' settings it holds.",
  "backups.page.none": "It holds no addon's settings by name.",
  "backups.page.gone": "This backup is gone.",
  "backups.page.restore_one": "Restore its settings",

  // The settings sheet (1n2p.31, design doc §2.7).
  "settings.title": "Settings",
  "settings.keys": "j/k to move · Enter or l to change, h back · Esc to close",
  "settings.section.panel": "Panel",
  "settings.panel.panelWidth": "Width",
  "settings.panel.panelWidth.about": "How wide the panel opens, from 400 to 800 px. z fills the screen.",
  "settings.panel.refreshIntervalSec": "Refresh when nothing changed",
  "settings.panel.refreshIntervalSec.about": "omaddons tells the panel about every change; this is the safety net. Every 15 s while the panel is open.",
  "settings.panel.autoCheck": "Check for updates when due",
  "settings.panel.autoCheck.about": "In the background, when a client's last check is older than omaddons' check interval.",
  "settings.panel.notifyErrors": "Notify about Lua errors after a session",
  "settings.panel.notifyErrors.about": "Once you quit, if the session left new Lua errors.",
  "settings.panel.notifyUpdates": "Notify about new updates",
  "settings.panel.notifyUpdates.about": "When a check finds updates that weren't there before.",
  "settings.panel.notifyWhilePlaying": "Notify while a game runs",
  "settings.panel.notifyWhilePlaying.about": "Off holds them while a game runs, and says what came up in one notification when it stops.",
  "settings.panel.binary": "omaddons binary",
  "settings.panel.binary.about": "Empty runs the omaddons that installed the panel. A full path runs another build.",
  "settings.panel.binary.edit": "Type a full path, then Enter. Esc keeps the one before.",
  "settings.panel.binary.invalid": "A full path starts with /. Empty runs the one that installed the panel.",
  "settings.value.on": "On",
  "settings.value.off": "Off",
  "settings.value.px": "{n} px",
  "settings.value.seconds": "{n} s",
  "settings.value.minutes": "{n} min",
  "settings.value.minutes_seconds": "{n} min {s} s",
  "settings.value.binary_stamped": "the one that installed the panel",
  "settings.value.hours": "{n} h",
  "settings.section.omaddons": "omaddons",
  "settings.omaddons.reading": "Reading omaddons' settings…",
  "settings.omaddons.failed": "Couldn't read omaddons' settings.",
  "settings.omaddons.change": "{setting}: {value}",
  "settings.result": "{setting}: {value}",
  "settings.result.unchanged": "{setting}: already {value}",
  "settings.result.name": "Renamed it {value}",
  "settings.result.name.builtin": "Its built-in name is back",
  "settings.result.family": "Its game: {value}",
  "settings.result.default": "Default client: {client}",
  "settings.section.clients": "Clients",
  "settings.clients.default_mark": "default",
  "settings.clients.name.about": "Enter renames it: the panel and status show this name, and empty brings back the built-in one. Its id stays {id}.",
  "settings.clients.name.label": "Rename {client}",
  "settings.clients.name.edit": "Type its name, then Enter. Empty brings back the built-in name; Esc keeps this one.",
  "settings.clients.name.invalid": "One line, at most 200 characters.",
  "settings.clients.family": "{owner} game",
  "settings.clients.family.about": "omaddons can't read this client's version. Its game decides which addon builds it takes.",
  "settings.clients.family.unknown": "not known",
  "settings.clients.default": "Default client",
  "settings.clients.default.about": "Where an install goes when you don't choose a client. default_target in config.toml wins over it.",
  "settings.clients.default.none": "none",
  "settings.clients.missing": "{client} · not found",
  "settings.clients.forget": "Forget…",
  "settings.clients.forget.label": "Forget {client}",
  "settings.clients.forget.about": "It's no longer installed. Enter forgets it, after showing what that does.",
  "settings.clients.forget.none": "It's no longer installed. Issues says what can be done about it.",
  "settings.clients.rescan": "Scan for clients",
  "settings.clients.rescan.about": "Looks for World of Warcraft clients again: new ones, and ones that moved.",
  "settings.clients.add": "Add a client by its folder…",
  "settings.clients.add.about": "For a client omaddons doesn't find by itself, such as a private server's.",
  "settings.family.mainline": "Retail",
  "settings.family.vanilla": "Classic Era",
  "settings.family.tbc": "Burning Crusade Classic",
  "settings.family.wrath": "Wrath of the Lich King Classic",
  "settings.family.cata": "Cataclysm Classic",
  "settings.family.mists": "Mists of Pandaria Classic",
  "settings.family.titan": "Titan",
  "settings.family.forever": "WoW: Forever",
  "settings.family.vanilla-legacy": "1.12 private server",
  "settings.family.tbc-legacy": "2.4.3 private server",
  "settings.family.wrath-legacy": "3.3.5a private server",
  "settings.family.cata-legacy": "4.3.4 private server",
  "settings.family.mists-legacy": "5.4.8 private server",
  "settings.omaddons.default_channel": "Channel for new installs",
  "settings.omaddons.default_channel.about": "Which releases a new install follows. Each addon can follow its own.",
  "settings.omaddons.default_channel.stable": "Stable",
  "settings.omaddons.default_channel.beta": "Beta",
  "settings.omaddons.default_channel.alpha": "Alpha",
  "settings.omaddons.running_policy": "Changes while a game runs",
  "settings.omaddons.running_policy.about": "Safe ones go through and take effect after a /reload or a restart. Settings are never written while a game runs.",
  "settings.omaddons.running_policy.safe": "Safe ones",
  "settings.omaddons.running_policy.refuse": "None",
  "settings.omaddons.running_policy.allow": "All",
  "settings.omaddons.auto_backup": "Automatic backups",
  "settings.omaddons.auto_backup.about": "A copy of a client's settings before a change and after you play, once a day. Free copies need a btrfs or xfs drive.",
  "settings.omaddons.auto_backup.daily": "Daily, free copies only",
  "settings.omaddons.auto_backup.daily-copy": "Daily, full copies where needed",
  "settings.omaddons.auto_backup.off": "Off",
  "settings.omaddons.check_interval_hours": "Check for updates every",
  "settings.omaddons.check_interval_hours.about": "After this long a client's update check is due again; the panel checks then when its own check is on.",

  // The restore chooser (1n2p.29, design doc §2.4).
  "restore.title.owner": "Restore {owner} settings on {client}",
  "restore.title.client": "Restore on {client}",
  "restore.title.backup": "Restore {owner} settings from which backup?",
  "restore.title.addon": "Whose settings, on {client}?",
  "restore.from": "From: {backup}",
  "restore.addon": "{owner} settings",
  "restore.addon.written": "{owner} settings (written by {version})",
  "restore.whole": "All of {owner} settings…",
  "restore.whole.note": "Keybindings, macros, chat and every addon's settings",
  "restore.one": "One addon's settings…",
  "restore.label.addon": "Restore {owner} settings",
  "restore.label.whole": "Restore all of {owner} settings",
  "restore.none": "No backup of {client} can be restored.",
  "restore.reading": "Reading backups…",
  "restore.failed": "Couldn't read {owner} backups.",
  "restore.unrestorable": "This backup can't be restored.",
  "restore.no_addons": "No addon's settings to choose.",
  "restore.blocked": "Quit {client} first.",
  "restore.blocked.unknown": "Quit {client} first: omaddons can't tell if it's running.",
  "restore.keys": "j/k to move, Enter to choose, Esc to go back",

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
  "sheet.effect.adopt_in_place": "Nothing is reinstalled or moved: omaddons starts keeping track of these folders as they are, and can undo that.",
  "sheet.effect.adopt_unknown": "Folders nothing identifies stay as they are, unmanaged.",
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
  "sheet.item.prune_snapshots": "Delete {snapshots} on {client}",
  "sheet.effect.prune_final": "Those changes can no longer be undone; omaddons would delete these snapshots after its next change anyway.",
  "result.snapshots_pruned": "Deleted {snapshots} on {client}",
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
  "state.failed.issues": "Couldn't read the problems.",
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
  "state.loading.history": "Reading history…",
  "state.failed.history": "Couldn't read the history.",
  "state.empty.history": "No changes yet.",
  "state.empty.history.client": "No changes on {client} yet.",
  "state.what.history": "the history",
  "state.notice.history_skipped": "Some history can't be read and isn't listed.",
  "state.notice.history_truncated": "Only the newest {n} changes are listed.",

  "history.actor.human": "you, in a terminal",
  "history.actor.agent": "an agent",
  "history.actor.widget": "you, in the panel",
  "history.title.install": { "one": "Installed {addon}", "other": "Installed {n} addons" },
  "history.title.install.none": "Installed addons",
  "history.title.update": { "one": "Updated {addon}", "other": "Updated {n} addons" },
  "history.title.update.none": "Updated addons",
  "history.title.reinstall": { "one": "Repaired {owner} files", "other": "Repaired the files of {n} addons" },
  "history.title.remove": { "one": "Removed {addon}", "other": "Removed {n} addons" },
  "history.title.remove.none": "Removed addons",
  "history.title.adopt": { "one": "Took over {addon}", "other": "Took over {n} addons" },
  "history.title.adopt.none": "Took over addons",
  "history.title.sync": { "one": "Matched your addon list: {addon}", "other": "Matched your addon list: {n} addons" },
  "history.title.sync.none": "Matched your addon list",
  "history.title.pin": { "one": "Pinned {addon}", "other": "Pinned {n} addons" },
  "history.title.unpin": { "one": "Unpinned {addon}", "other": "Unpinned {n} addons" },
  "history.title.pin.none": "Pinned or unpinned addons",
  "history.title.undo": "Undid the {change}",
  "history.title.undo.unknown": "Undid a change",
  "history.title.restore": "Restored the settings",
  "history.title.restore.addons": { "one": "Restored {owner} settings", "other": "Restored the settings of {n} addons" },
  "history.title.repair": "Repaired a problem",
  "history.title.backup": "Backed up the settings",
  "history.title.config": "Changed a setting",
  "history.title.target": "Changed the clients",
  "history.title.auth": "Changed the GitHub token",
  "history.title.export": "Saved the addon list",
  "history.title.dismiss": "Marked Lua errors as seen",
  "history.title.widget": "Installed or removed the bar widget",
  "history.title.skill": "Installed the agent guide",
  "history.title.other": "Changed something",
  "history.item.settings": "every setting",
  "history.item.more": { "one": "1 more", "other": "{n} more" },
  "history.change.installed": "Installed {item}",
  "history.change.updated": "Updated {item}",
  "history.change.reinstalled": "Repaired the files of {item}",
  "history.change.removed": "Removed {item}",
  "history.change.adopted": "Took over {item}",
  "history.change.pinned": "Pinned {item}",
  "history.change.unpinned": "Unpinned {item}",
  "history.change.edited": "Changed {item}",
  "history.change.settings_restored": "Restored the settings of {item}",
  "history.change.reverted": "Put back {item}",
  "history.change.whole.settings_restored": "Restored every setting",
  "history.change.whole.reverted": "Put every setting back as it was",
  "history.reason": "“{reason}”",
  "history.filter": "Filter by addon, client, who or why",
  "history.chip.undone": "undone",
  "history.action.undo": "Undo",
  "history.action.redo": "Redo",
  "history.undo.label": "Undo: {title}",
  "history.redo.label": "Redo: {title}",
  "history.undo_last.unknown": "Undo: choose the change in History",
  "history.page.undo_all": "Undo all of it",
  "history.page.redo_all": "Redo all of it",
  "history.page.undo_on": "Undo on {client} only",
  "history.page.redo_on": "Redo on {client} only",
  "history.page.plan": "Made from a preview you approved.",
  "history.page.backup": "Put back from a backup: Enter on this line shows it in Backups.",
  "history.page.gone": "This change isn't among the newest changes History reads any more.",
  "page.section.changes": "Changes",
  "page.change": "{title} · {when}",
  "page.change.undone": "{title} · {when} · undone",
  "history.why.backup": "A backup is no change to undo.",
  "history.why.settings": "A settings change: undo doesn't revert it.",
  "history.why.dismiss": "Marking errors as seen can't be undone.",
  "history.why.old": "Made by an older omaddons, before changes were grouped: undo it from a terminal.",
  "history.why.undone": "It was already undone.",

  "time.just_now": "just now",
  "time.minutes": "{n} min ago",
  "time.hours": "{n} h ago",
  "time.yesterday": "yesterday {time}",
  "time.weekday": "{day} {time}",
  "time.day.today": "Today",
  "time.day.yesterday": "Yesterday",
  "time.day.date": "{d} {month}",
  "time.weekdays": "Sunday Monday Tuesday Wednesday Thursday Friday Saturday",
  "time.weekdays.short": "Sun Mon Tue Wed Thu Fri Sat",
  "time.months": "January February March April May June July August September October November December",
  "time.months.short": "Jan Feb Mar Apr May Jun Jul Aug Sep Oct Nov Dec",
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
  "state.refreshing.search": "Searching…",
  "state.loading.errors": "Reading the Lua errors…",
  "state.failed.errors": "Couldn't read the Lua errors.",
  "state.empty.errors": "No client records Lua errors here.",
  "state.empty.errors.client": "{client} records no Lua errors.",
  "state.what.errors": "the Lua errors",
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
  "installed.take_over.label": "Take over the addons on {client}",

  // The Errors view and its page (1n2p.27, design doc §6.6).
  "errors.source.high": "probably caused by it",
  "errors.source.medium": "maybe caused by it",
  "errors.source.low": "possibly caused by it",
  "errors.source.library": "in a library {addon} bundles",
  "errors.source.blizzard": "in the game's own code",
  "errors.source.unknown": "source unknown",
  "errors.source.blocked": "blocked call from {addon}",
  "errors.message.custom": { "one": "a message from the addon (1 character)", "other": "a message from the addon ({chars} characters)" },
  "errors.new": "{n} new",
  "errors.new_one": "new",
  "errors.last_seen": "last {age}",
  "errors.last_session": "last session {age}",
  "errors.update_chip": "update available (may fix it)",
  "errors.action.update": "Update {addon}",
  "errors.action.copy": "Copy report",
  "errors.action.open": "Show its errors",
  "errors.partial": "Some errors couldn't be read: the game was writing.",
  "errors.received": { "one": "1 error sent by other players (not counted)", "other": "{n} errors sent by other players (not counted)" },
  "errors.capture.none": "Lua errors aren't recorded on {client}: install BugGrabber to record them.",
  "errors.capture.unknown": "Lua errors aren't recorded on {client}, and no BugGrabber build for it is known.",
  "errors.capture.install": "Install BugGrabber",
  "errors.none": "No Lua errors",
  "errors.copied": "Copied the report on {addon}",
  "errors.copy_failed": "Couldn't copy (wl-copy is missing)",
  "errors.raw.show": "Show raw details…",
  "errors.raw.hide": "Hide raw details",
  "errors.raw.warning": "Raw details may contain other players' names and your own code.",
  "errors.raw.reading": "Reading the raw details…",
  "errors.report.head": "{addon} {version} on {client}{client_version}",
  "errors.report.where": "at {location} · {times}",

  // The clients (design doc §2.1, §2.2): the scope's words, and a group's header.
  "client.all": "All clients",
  "client.running": "running",
  "client.running_unknown": "can't tell if running",
  "client.not_found": "not found",
  "client.choose": "Choose a client first: t, or a row of one",

  // The bar button's tooltip (1n2p.16, design doc §2.5, §6.4).
  "bar.starting": "Asking omaddons…",
  "bar.missing": "Can't find omaddons",
  "bar.incompatible": "The panel is from another omaddons version",
  "bar.failed": "omaddons ran into a problem",
  "bar.unreachable": "omaddons didn't answer",
  "bar.stale": "Last heard from omaddons {age}",
  "bar.empty": "No World of Warcraft here yet",
  "bar.all_good": "All good",
  "bar.errors": { "one": "1 new Lua error", "other": "{n} new Lua errors" },
  "bar.urgent": { "one": "1 problem needs attention", "other": "{n} problems need attention" },
  "bar.updates": { "one": "1 update", "other": "{n} updates" },
  "bar.problems": { "one": "1 problem", "other": "{n} problems" },
  "bar.client": "{client} · {facts}",
  "bar.running_unknown": "Can't tell if the game is running",
  "bar.checked": "Checked {age}",
  "bar.unchecked": "Not checked for updates yet",
  "bar.hints": "Left: open · Middle: {view} · Right: check for updates",
  "bar.hints.broken": "Left: open to see what to do",

  // The cards shown when omaddons cannot be used (1n2p.34, design doc §6.4).
  "card.missing.title": "Can't find omaddons",
  "card.missing.sentence": "It was at {path}. Reinstall the panel from the omaddons you use now, then press Retry.",
  "card.malformed.title": "omaddons didn't answer",
  "card.malformed.sentence": "It stopped before it said anything the panel can read.",
  "card.oversized.title": "omaddons didn't answer",
  "card.oversized.sentence": "Its answer was larger than the panel reads.",
  "card.timeout.title": "omaddons didn't answer in time",
  "card.timeout.sentence": "It was still working after {seconds} seconds, so the panel stopped it.",
  "card.stuck.title": "omaddons stopped answering",
  "card.stuck.sentence": "It did not end even when stopped, so the panel let it go and freed its worker.",
  "card.version.title": "The panel is from another omaddons version",
  "card.version.sentence": "Panel {mine}, omaddons {theirs}. Update the panel to match; until then it only shows what it read last.",
  "card.version.unreadable": "a version this panel can't read",
  "card.outdated.title": "The panel is out of date",
  "card.outdated.sentence": "Its files differ from this omaddons. Update the panel; until then it only shows what it read last.",
  "card.retrying": "The panel tries again by itself.",
  "card.reinstalled": "Update the panel ran {age}; this is still the old panel.",
  "card.action.retry": "Retry",
  "card.action.reinstall": "Update the panel",
  "card.action.copy_command": "Copy command",
  "card.action.copy_details": "Copy details",
  "card.copied": "Copied",

  // A failure's details, for a bug report (Copy details).
  "details.what": "What: {what}",
  "details.unknown": "unknown",
  "details.command": "Command: {command}",
  "details.ended": "Ended: {how}, {code}",
  "details.no_exit": "no exit code",
  "details.exit": "exit code {code}",
  "details.panel": "Panel: omaddons {version}",
  "details.output": "Last output:",
  "details.outcome.missing": "not found",
  "details.outcome.malformed": "an answer the panel can't read",
  "details.outcome.oversized": "an answer too large to read",
  "details.outcome.timeout": "timed out",
  "details.outcome.stuck": "still running after SIGKILL, let go",
  "details.outcome.cancelled": "stopped",
  "details.outcome.failed": "failed",
  "details.outcome.ok": "answered",
  "details.outcome.incompatible": "answered as another omaddons build",
  "details.outcome.unknown": "unknown",

  // A request that ended with no envelope, in the failure card.
  "failure.timeout": "{label} didn't finish: omaddons took too long",
  "failure.malformed": "{label}: omaddons answered with something the panel can't read",
  "failure.oversized": "{label}: omaddons' answer was too large to read",
  "failure.cancelled": "{label} was stopped",
  "failure.stuck": "omaddons stopped answering during {label}; the panel freed its worker and goes on",
  "failure.missing": "Can't find omaddons",
  "failure.other": "{label} didn't finish ({outcome})",

  // The first run (1n2p.32, design doc §6.4).
  "welcome.title": "No World of Warcraft here yet",
  "welcome.hero": "No clients yet",
  "welcome.sentence": "omaddons looked where Battle.net, Steam, Lutris, Heroic and Bottles keep their games, in ~/Games and in ~/.wine. Install the game with one of them and scan again, or add the folder where it is.",
  "welcome.rescan": "Scan again",
  "welcome.add": "Add a client folder…",
  "onboard.not_now": "Not now",
  "onboard.adopt.title": "Take over the addons already on {client}?",
  "onboard.adopt.sentence": { "one": "1 folder there came by hand or from another addon manager. omaddons can keep track of it as it is: nothing is reinstalled or moved.",
    "other": "{n} folders there came by hand or from another addon manager. omaddons can keep track of them as they are: nothing is reinstalled or moved." },
  "onboard.buggrabber.title": "Record Lua errors on {client}?",
  "onboard.buggrabber.sentence": "BugGrabber, one small addon, saves them as they happen, so omaddons can tell which addon throws them and whether an update may fix it.",
  "onboard.keyhint.title": "Open this panel with a key",
  "onboard.keyhint.sentence": "Add this line to ~/.config/hypr/bindings.lua (any free key will do), and the key opens and closes the panel:",
  "onboard.keyhint.name": "WoW addons",
  "onboard.keyhint.copy": "Copy the line",
  "onboard.keyhint.done": "Got it",
  "addclient.title": "Add a client",
  "addclient.sentence": "The folder that holds the game: the one with Wow.exe (or WowClassic.exe) and its Interface folder, or the one above _retail_ and _classic_. Type it from / or ~/.",
  "addclient.placeholder": "~/Games/World of Warcraft",
  "addclient.add": "Add",
  "addclient.adding": "Adding…",
  "addclient.result": "Added {clients}",
  "addclient.result.none": "Nothing new to add: omaddons knows this client already",
  "addclient.error.empty": "Type the folder's path first.",
  "addclient.error.relative": "Type the whole path, from / or ~/.",
  "addclient.error.bad": "That isn't a path a folder can have: one line, at most 4096 bytes.",

  // A change a crash or a reload cut off (1n2p.34).
  "interrupted.title.none": "A change was interrupted",
  "interrupted.title.one": "A change on {clients} was interrupted",
  "interrupted.title.many": "Changes on {clients} were interrupted",
  "interrupted.sentence": "omaddons finishes or undoes it before the next change. See what it would do first.",
  "interrupted.action": "See what to do",

  // The Issues view and a problem's page (1n2p.28, design doc §2.3, §6.3, §6.5).
  "issues.severity.P0": "Broken",
  "issues.severity.P1": "Needs attention",
  "issues.severity.P2": "Worth fixing",
  "issues.severity.P3": "Minor",
  "issues.section.machine": "This computer",
  "issues.section.suggested": "Suggested",
  "issues.section.notes": "Notes",
  "issues.count": { "one": "1 problem", "other": "{n} problems" },
  "issues.fix.repair": "Repair",
  "issues.fix.reinstall": "Repair {addon}",
  "issues.fix.update_all": "Update all",
  "issues.fix.update_client": "Update {client}",
  "issues.fix.update_addon": "Update {addon}",
  "issues.fix.backup_client": "Back up {client}",
  "issues.fix.backup_all": "Back up every client",
  "issues.fix.check": "Check for updates",
  "issues.fix.refresh": "Refresh",
  "issues.fix.widget": "Reinstall panel",
  "issues.fix.errors": "Read Lua errors",
  "issues.fix.backups": "Refresh backups",
  "issues.fix.addons": "Show {owner} addons",
  "issues.fix.take_over": "Take over {owner} addons…",
  "issues.fix.addons.all": "Show the addons",
  "issues.fix.remove": "Remove {addons}",
  "issues.fix.other": "Run the fix",
  "issues.explain": "Explain",
  "issues.copy": "Copy command",
  "issues.copied": "Copied",
  "issues.hint.repair": "{key} previews the repair",
  "issues.hint.fix": "{key} runs the fix",
  "issues.hint.explain": "{key} explains it",
  "issues.terminal.forced": "This needs your go-ahead, which the panel never gives: read the explanation, then run it in a terminal.",
  "issues.terminal.forced.unexplained": "This needs your go-ahead, which the panel never gives: run it in a terminal.",
  "issues.terminal.other": "The panel never runs this one: run it in a terminal.",
  "issues.suggest.update": { "one": "1 update is waiting on {client}.", "other": "{n} updates are waiting on {client}." },
  "issues.suggest.update.clients": { "one": "1 update is waiting.", "other": "{n} updates are waiting on {m} clients." },
  "issues.suggest.update.unknown": "Updates are waiting.",
  "issues.suggest.check_updates": "Not checked for updates lately.",
  "issues.suggest.adopt": {
    "one": "1 addon on {client} was installed by hand or by another addon manager.",
    "other": "{n} addons on {client} were installed by hand or by another addon manager."
  },
  "issues.suggest.adopt.unknown": "Addons on {client} were installed by hand or by another addon manager.",
  "issues.suggest.check_errors": "Lua errors changed since they were last read.",
  "issues.suggest.backup_create": "{client} has no recent backup of its settings.",
  "issues.suggest.refresh_backups": "{owner} backup details are out of date.",
  "issues.suggest.reinstall_widget": "The panel is out of date.",
  "issues.suggest.remove_orphans": "Nothing on {client} needs {addons} any more.",
  "issues.suggest.forget_target": "{client} isn't on this computer any more.",
  "issues.suggest.inspect": "Some addons on {client} need a look.",
  "issues.suggest.list_unmanaged": "{client} has folders omaddons doesn't manage.",
  "issues.suggest.reinstall": "{addon} on {client} isn't as it was installed: files changed, or a folder is gone.",
  "issues.suggest.status": "omaddons hasn't looked for your clients yet.",
  "issues.suggest.other": "omaddons suggests this next step.",
  "issues.note.unknown_flavor": "A client's game version isn't recognised, so omaddons treats it carefully.",
  "issues.note.client_not_launched_since_patch": "A client hasn't started since its last patch. Start it once.",
  "issues.note.target_key_duplicate": "Two clients claim to be the same install (a copied folder?).",
  "issues.note.target_addons_shared": "Two clients share one AddOns folder. Manage those addons from one of them.",
  "issues.note.snapshot_cap_exceeded": "A client's undo data is over its size limit.",
  "issues.note.restore_rolled_back": "An interrupted restore was undone; the settings it replaced are kept as a backup.",
  "issues.note.config_unknown_key": "config.toml has a setting omaddons doesn't know.",
  "issues.note.discovery_truncated": "Not every folder was searched for clients. Add a missing client by its folder.",
  "issues.note.catalog_override_invalid": "One of your catalog files was skipped: it has a mistake.",
  "issues.note.history_lines_skipped": "Some history can't be read, and isn't listed.",
  "issues.page.reading": "Reading the explanation…",
  "issues.page.failed": "Couldn't read the explanation: {why}",
  "issues.page.gone": "omaddons no longer reports this problem.",
  "issues.errors_title": "Lua errors on {client}",

  // The addon page (1n2p.23, design doc §2.4, §6.3).
  "page.by": "by {author}",
  "page.updated": "updated {age}",
  "page.installed": "{version} installed",
  "page.installed.unknown": "installed",
  "page.update": "{version} available",
  "page.channel": "{channel} releases",
  "page.not_installed": "not installed",
  "page.compat.works": "works here",
  "page.compat.likely": "probably works here",
  "page.compat.not": "not made for this client",
  "page.install_here": "Install here",
  "page.web": "Web page",
  "page.copy_ref": "Copy its source",
  "page.copied": "Copied {ref}",
  "page.section.clients": "On each client",
  "page.section.notes": "What's new",
  "page.section.folders": "Folders",
  "page.section.needs": "Needs",
  "page.section.needed_by": "Needed by",
  "page.section.errors": "Lua errors",
  "page.notes.none": "No newer release notes.",
  "page.notes.more": "More on {host}",
  "page.needs.missing": "{name} (not installed)",
  "page.folders.left_out": "{client} leaves out {folders}",
  "page.errors.count": { "one": "1 error on {client}", "other": "{n} errors on {client}" },
  "page.reading": "Reading about it…",
  "page.failed": "Couldn't read more about it: {why}",
  "page.why.offline": "Not checked: omaddons is offline.",
  "page.why.not_found": "Its source knows no such project.",
  "page.why.rate_limited": "GitHub asked to wait; try again later.",
  "page.why.fetch_failed": "Its source didn't answer.",
  "page.why.source_disabled": "Its source is turned off.",
  "page.why.not_supported": "Its source says nothing more about it.",
  "page.why.source_has_no_metadata": "Its source says nothing more about it.",
  "page.why.other": "Nothing more is known about it.",

  "source.catalog": "omaddons' catalog",
  "source.github": "GitHub",
  "source.wowi": "WoWInterface",
  "source.tukui": "Tukui",
  "source.wowace": "WowAce",
  "source.git": "a git repository",
  "source.file": "a file",

  "search.box": "Search addons, or paste a link",
  "search.verdict.works": "Works on {client}",
  "search.verdict.likely": "Probably works on {client}",
  "search.verdict.unknown": "Not known yet for {client}",
  "search.verdict.abandoned": "Not updated in two years",
  "search.verdict.not": "Not made for {client}",
  "search.verdict.installed": "Already on {client}",
  "search.verdict.all.works": { "one": "Works on 1 of {m} clients", "other": "Works on {n} of {m} clients" },
  "search.verdict.all.works_likely": "Works on {n} of {m} clients, probably on {k} more",
  "search.verdict.all.likely": { "one": "Probably works on 1 of {m} clients", "other": "Probably works on {n} of {m} clients" },
  "search.verdict.all.unknown": "Not known yet where it works",
  "search.verdict.all.not": "Not made for these clients",
  "search.note.not": "{words}: nothing to install there.",
  "search.note.unknown": "Not known yet where it works: Enter opens its page, which checks.",
  "search.note.installed": "Already installed on {client}.",
  "search.note.installed_where": "Already installed where it works.",
  "search.chip.installed": "installed",
  "search.chip.installed.version": "installed {version}",
  "search.chip.installed.on": "installed on {clients}",
  "search.chip.installed.clients": "installed on {n} clients",
  "search.chip.catalog": "checked by omaddons",
  "search.meta.updated": "updated {age}",
  "search.action.install": "Install",
  "search.action.check": "Check where it works",
  "search.action.open_link": "See where it works, then install",
  "search.link.title": "Install from {source}",
  "search.link.page": "A pasted link",
  "search.link.source.github": "GitHub",
  "search.link.source.git": "a git repository",
  "search.link.source.wowi": "WoWInterface",
  "search.link.source.tukui": "Tukui",
  "search.link.source.wowace": "WowAce",
  "search.link.source.url": "a download link",
  "search.chooser.title": "Install {addon} on:",
  "search.browse.client": "Addons omaddons has checked for {client}. Type to search everywhere.",
  "search.browse.all": "Addons omaddons has checked for your clients. Type to search everywhere.",

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
  "stale.repair.unknown": "the repair",

  // The activity bar and the rows a change moves (design doc §2.1, §6.4; 1n2p.18).
  "activity.check": "Checking for updates",
  "activity.search": "Searching",
  "activity.changelog": "Reading the release notes",
  "activity.addon_info": "Reading about the addon",
  "activity.preview.match": "preparing",
  "activity.preview.confirm": "checking what it would do",
  "activity.preview.bind": "checking what it would repair",
  "activity.change.one": "{verb} {addon} on {client}",
  "activity.change.client": "{verb} {n} addons on {client}",
  "activity.change.clients": "{verb} {n} addons on {m} clients",
  "activity.verb.update": "Updating",
  "activity.verb.install": "Installing",
  "activity.verb.reinstall": "Repairing",
  "activity.verb.remove": "Removing",
  "activity.verb.pin": "Pinning",
  "activity.verb.unpin": "Unpinning",
  "activity.verb.adopt": "Taking over",
  "activity.verb.edit": "Changing",
  "activity.verb.undo": "Undoing",
  "activity.verb.restore": "Restoring",
  "activity.verb.prune": "Deleting",
  "activity.verb.backup": "Backing up",
  "activity.verb.dismiss": "Marking as seen",
  "activity.verb.repair": "Repairing",
  "activity.verb.generic": "Working",
  "activity.phase.fetch": "checking",
  "activity.phase.download": "downloading",
  "activity.phase.verify": "checking files",
  "activity.phase.stage": "preparing",
  "activity.phase.commit": "installing",
  "activity.phase.commit.other": "finishing",
  "activity.phase.copy": "copying settings",
  "activity.phase.checkpoint": "backing up settings first",
  "activity.count": "({current} of {total})",
  "activity.seconds": "{s} s",
  "activity.minutes": "{m} min {s} s",
  "activity.stalled": "Still working (no news for {time})",
  "activity.next": "Next: {label}",
  "activity.busy": "Other changes wait until this one finishes.",
  "activity.cancel": "Cancel",
  "activity.cancelled": "Stopped: {label}",
  "activity.lane_full": "Too many checks and searches are waiting; try again in a moment.",
  "activity.dismiss": "Dismiss",
  "activity.done.updated": "Updated",
  "activity.done.installed": "Installed",
  "activity.done.removed": "Removed",
  "activity.done.pinned": "Pinned",
  "activity.done.unpinned": "Unpinned",
  "activity.done.edited": "Changed",
  "activity.done.adopted": "Taken over",
  "activity.done.generic": "Done",

  // The Updates view (design doc §2.3, §6.3; 1n2p.21).
  "updates.age.today": "out today",
  "updates.age.days": { "one": "1 day old", "other": "{n} days old" },
  "updates.age.months": { "one": "1 month old", "other": "{n} months old" },
  "updates.age.years": { "one": "1 year old", "other": "{n} years old" },
  "updates.kind.update": "update",
  "updates.kind.done": "updated",
  "updates.kind.pinned": "pinned",
  "updates.kind.blocked": "blocked",
  "updates.kind.failed": "check failed",
  "updates.meta.running": "{client} is running",
  "updates.meta.pinned": "pinned at {version} · {latest} available",
  "updates.meta.blocked": "{latest} is out, but it doesn't load on {client}",
  "updates.meta.failed": "Couldn't check {source} for a new release",
  "updates.action.update": "Update",
  "updates.action.notes": "Release notes",
  "updates.action.check": "Check again",
  "updates.update_scope": { "one": "Update 1", "other": "Update {n}" },
  "updates.update_client": "Update {client}",
  "updates.checked": "Checked {age}",
  "updates.unchecked": "Not checked yet",
  "updates.check_now": "Check now",
  "updates.filter": "Filter updates",
  "updates.count": "{shown} of {total}",
  "updates.notes.reading": "Reading the release notes…",
  "updates.notes.failed": "The release notes could not be read: {why}",
  "updates.notes.empty": "No release notes",
  "updates.notes.none": "No newer release notes",
  "updates.notes.behind": { "one": "1 release behind", "other": "{n} releases behind" },
  "updates.notes.behind_more": "{n}+ releases behind",
  "updates.notes.more": "More on {host}",

  // Notifications (1n2p.33): names and counts only, never an error's text (M3-D21).
  "toast.errors.title": "Lua errors in your last session",
  "toast.errors.title.on": "Lua errors in your last session on {clients}",
  "toast.updates.title": "Addon updates",
  "toast.updates": { "one": "1 update", "other": "{n} updates" },
  "toast.updates.on": { "one": "1 update on {clients}", "other": "{n} updates on {clients}" },
  "toast.digest.title": "After your session",
  "toast.digest.title.now": "Lua errors and addon updates",
  "toast.digest.errors": { "one": "1 new Lua error in {addons}", "other": "{n} new Lua errors in {addons}" },
  "toast.result.title": "omaddons"
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
  // A take-over (1n2p.32): the player's main worry first. Nothing moves; what nothing
  // identifies stays as it is.
  if (kind === "adopt") {
    effects = [sheetEffect("note", copy("sheet.effect.adopt_in_place")), sheetEffect("note", copy("sheet.effect.adopt_unknown"))]
      .concat(effects)
  }
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

// One sheet for an install on several clients (omad-3rre, Search's "Every client it
// works on"): every client's items, grouped under its name (finishSheet), each effect
// once when every client's sheet has it, else once per client that has it, named; then
// the clients left out, with why (`notes`). Titled with the clients, by name up to three.
function eachSheet(addon, sheets, notes) {
  var list = Array.isArray(sheets) ? sheets : []
  var items = []
  var count = {}
  list.forEach(function(s) {
    (s.effects || []).forEach(function(e) { count[e.kind + "\n" + e.text] = (count[e.kind + "\n" + e.text] || 0) + 1 })
  })
  var effects = []
  var seen = {}
  list.forEach(function(s) {
    var its = Array.isArray(s.items) ? s.items : []
    its.forEach(function(it) { items.push(Object.assign({}, it)) })
    var client = sheetClients(its)[0] || ""
    ;(s.effects || []).forEach(function(e) {
      var key = e.kind + "\n" + e.text
      if (count[key] === list.length) {
        if (!seen[key]) effects.push(e)
        seen[key] = true
      } else {
        effects.push(Object.assign({}, e, { "text": copy("sheet.each.effect", { "client": client, "text": e.text }) }))
      }
    })
  })
  ;(Array.isArray(notes) ? notes : []).forEach(function(n) { effects.push(sheetEffect("note", n)) })
  var clients = sheetClients(items)
  return finishSheet({
    "title": copy("sheet.title.install.each", { "addon": addon, "clients": clients.length <= 3 ? joinNames(clients) : plural(clients.length, "client") }),
    "stale": list.map(function(s) { return str(s.stale) }).filter(function(t) { return t !== "" }).join(" · "),
    "items": items, "effects": effects, "confirmLabel": copy("sheet.confirm.install.each", { "n": clients.length }),
    "destructive": list.some(function(s) { return s.destructive === true }), "glyph": list.length ? list[0].glyph : 0
  })
}

// What an install on several clients did (omad-3rre): the clients it installed on, the
// one that failed and why, the ones it never reached; and that Undo takes back the last
// one only (each client's install is its own change), History each.
function eachInstallLine(addon, committed, failure, notRun) {
  var parts = []
  if (committed.length) parts.push(copy("each.result.installed", { "addon": addon, "clients": joinNames(committed) }))
  if (failure) parts.push(copy("each.result.failed", { "client": failure.client, "why": str(failure.why) }))
  if (notRun.length) parts.push(copy("each.result.not_run", { "clients": joinNames(notRun) }))
  var line = parts.join("; ") || copy("each.result.none", { "addon": addon })
  if (committed.length > 1) line += " · " + copy("each.result.undo", { "owner": possessive(committed[committed.length - 1]) })
  return line
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
    // Undo snapshots engine retention prunes, per client: `txs` planned, `pruned` done.
    "prunes": list(dryRun ? env.prunes_planned : env.prunes),
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
  if (result.actions.length === 0 && result.delegated.length === 0 && result.prunes.length === 0) {
    decision.kind = "refresh"
    decision.line = "Nothing to repair now; the view is refreshed"
    return decision
  }
  decision.kind = "refuse"
  var mine = result.actions.concat(result.prunes)
  for (var i = 0; i < mine.length; i++) {
    if (str(mine[i].fm) !== str(slots.fm)) {
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
  result.prunes.forEach(function(p) {
    var txs = Array.isArray(p.txs) ? p.txs.length : 0
    items.push(sheetItem({ "op": "repair", "name": copy("sheet.item.prune_snapshots", {
      "snapshots": plural(txs, "undo snapshot"), "client": sheetClient(targets, str(p.target), str(p.key)) }) }))
  })
  var effects = result.delegated.map(function(d) {
    return sheetEffect("delegated", copy("sheet.effect.delegated", { "tx": str(d.tx), "direction": str(d.direction).replace(/_/g, " ") }))
  })
  if (result.prunes.length > 0) effects.push(sheetEffect("folders_removed", copy("sheet.effect.prune_final")))
  if (result.summary.bytesToBackUp > 0) effects.push(sheetEffect("kept_backup", copy("sheet.effect.keeps_copy", { "size": bytesText(result.summary.bytesToBackUp) })))
  // A prune is no repair transaction: Undo last never brings snapshots back.
  if (result.actions.length > 0 || result.delegated.length > 0) effects.push(sheetEffect("undo_last", copy("sheet.effect.undo_last")))
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
  var pruned = r.prunes.filter(function(p) { return p.outcome === "pruned" && Array.isArray(p.pruned) && p.pruned.length > 0 })
  if (pruned.length > 0) {
    return copy("result.snapshots_pruned", { "snapshots": plural(pruned[0].pruned.length, "undo snapshot"),
      "client": sheetClient(targets, str(pruned[0].target), str(pruned[0].key)) })
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
    PANEL_SETTING_KEYS: PANEL_SETTING_KEYS,
    secondsText: secondsText,
    settingText: settingText,
    panelSettingRows: panelSettingRows,
    steppedSetting: steppedSetting,
    typedPath: typedPath,
    settingsSections: settingsSections,
    OMADDONS_SETTINGS: OMADDONS_SETTINGS,
    CONFIG_VALUE_PATTERN: CONFIG_VALUE_PATTERN,
    reduceConfig: reduceConfig,
    configText: configText,
    omaddonsSettingRows: omaddonsSettingRows,
    configRequest: configRequest,
    CLIENT_FAMILIES: CLIENT_FAMILIES,
    clientSettingRows: clientSettingRows,
    clientNameRequest: clientNameRequest,
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
    BAR_STATES: BAR_STATES,
    BAR_TICK_MS: BAR_TICK_MS,
    BAR_MARK_GLYPHS: BAR_MARK_GLYPHS,
    barStaleAfterMs: barStaleAfterMs,
    barState: barState,
    barTooltip: barTooltip,
    RETRY_BACKOFF_MS: RETRY_BACKOFF_MS,
    retryDelayMs: retryDelayMs,
    incompatibleOf: incompatibleOf,
    homeless: homeless,
    failureDetails: failureDetails,
    failureWords: failureWords,
    phaseCard: phaseCard,
    missingBinaryCommand: missingBinaryCommand,
    interruptedChange: interruptedChange,
    listOf: listOf,
    welcomeCard: welcomeCard,
    clientPath: clientPath,
    KEY_HINT_KEY: KEY_HINT_KEY,
    keyHintLine: keyHintLine,
    onboardingCard: onboardingCard,
    needsCaptureRead: needsCaptureRead,
    PATH_MAX_BYTES: PATH_MAX_BYTES,
    badgeText: badgeText,
    reduceList: reduceList,
    reduceSearch: reduceSearch,
    reduceErrors: reduceErrors,
    reduceBackups: reduceBackups,
    ageText: ageText,
    progressLine: progressLine,
    ACTIVITY_STALL_MS: ACTIVITY_STALL_MS,
    ACTIVITY_ELAPSED_MS: ACTIVITY_ELAPSED_MS,
    ACTIVITY_ROW_DONE_MS: ACTIVITY_ROW_DONE_MS,
    RESULT_HOLD_MS: RESULT_HOLD_MS,
    PROGRESS_PHASES: PROGRESS_PHASES,
    activityKind: activityKind,
    activityStart: activityStart,
    activityProgress: activityProgress,
    activityView: activityView,
    activityHeadline: activityHeadline,
    shownActivity: shownActivity,
    activityRow: activityRow,
    changedRows: changedRows,
    queuedText: queuedText,
    barWorking: barWorking,
    phaseWord: phaseWord,
    durationText: durationText,
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
    releaseAge: releaseAge,
    groupUpdateRequest: groupUpdateRequest,
    releaseNotes: releaseNotes,
    updatesChecked: updatesChecked,
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
    issueSections: issueSections,
    issueRows: issueRows,
    problemRow: problemRow,
    fixFields: fixFields,
    argvParts: argvParts,
    issueFix: issueFix,
    buttonName: buttonName,
    sameArgv: sameArgv,
    suggestionText: suggestionText,
    problemCount: problemCount,
    reduceInfo: reduceInfo,
    addonPage: addonPage,
    COMPAT_WORDS: COMPAT_WORDS,
    SEVERITY_GLYPHS: SEVERITY_GLYPHS,
    NOTE_ELSEWHERE: NOTE_ELSEWHERE,
    errorSections: errorSections,
    errorRows: errorRows,
    errorPage: errorPage,
    errorReport: errorReport,
    errorSourceText: errorSourceText,
    errorMessageText: errorMessageText,
    urgentView: urgentView,
    dismissPlan: dismissPlan,
    searchRequest: searchRequest,
    installTargets: installTargets,
    installedOf: installedOf,
    searchLink: searchLink,
    searchesLive: searchesLive,
    searchRows: searchRows,
    catalogRows: catalogRows,
    catalogRequest: catalogRequest,
    refreshingText: refreshingText,
    SEARCH_DEBOUNCE_MS: SEARCH_DEBOUNCE_MS,
    SEARCH_MIN_CHARS: SEARCH_MIN_CHARS,
    boundStaleText: boundStaleText,
    rawLines: rawLines,
    popularityText: popularityText,
    sinceText: sinceText,
    instantMs: instantMs,
    dateText: dateText,
    bytesText: bytesText,
    backupKindText: backupKindText,
    backupProof: backupProof,
    autoSentence: autoSentence,
    backupRow: backupRow,
    backupSections: backupSections,
    backupRequest: backupRequest,
    pruneRequest: pruneRequest,
    restoreBlocked: restoreBlocked,
    backupAddons: backupAddons,
    backupPage: backupPage,
    restoreRequest: restoreRequest,
    restoreChooser: restoreChooser,
    ambiguityChoices: ambiguityChoices,
    backupFolderOf: backupFolderOf,
    stoppedTargets: stoppedTargets,
    errorsToRefresh: errorsToRefresh,
    stoppedNow: stoppedNow,
    AUTO_CHECK_INTERVAL_MS: AUTO_CHECK_INTERVAL_MS,
    autoCheckDue: autoCheckDue,
    rose: rose,
    errorFacts: errorFacts,
    updateFacts: updateFacts,
    errorToastArgv: errorToastArgv,
    updateToastArgv: updateToastArgv,
    digestToastArgv: digestToastArgv,
    resultToastArgv: resultToastArgv,
    TOAST_MIN_GAP_MS: TOAST_MIN_GAP_MS,
    playing: playing,
    emptyToastState: emptyToastState,
    toastPlan: toastPlan,
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
    eachSheet: eachSheet,
    eachInstallLine: eachInstallLine,
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
    lastChangeOf: lastChangeOf,
    HISTORY_LIMIT: HISTORY_LIMIT,
    HISTORY_FIXED: HISTORY_FIXED,
    RUN_PATTERN: RUN_PATTERN,
    reduceHistory: reduceHistory,
    historyStanding: historyStanding,
    historyItemText: historyItemText,
    historyTitle: historyTitle,
    historyReason: historyReason,
    historyActor: historyActor,
    historyRuns: historyRuns,
    historyUndo: historyUndo,
    historyRows: historyRows,
    historyCount: historyCount,
    historyPage: historyPage,
    dayText: dayText,
    whenText: whenText,
    WINDOW_MEMORY: WINDOW_MEMORY,
    windowEventAction: windowEventAction,
    FULL_SCAN_MAX_AGE_MS: FULL_SCAN_MAX_AGE_MS,
    fullScanDue: fullScanDue,
    LONG_LIST_ROWS: LONG_LIST_ROWS
  }
}
