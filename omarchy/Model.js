// The omaddons bar widget's model: every decision the widget makes that is not drawing.
//
// The CLI is the authority (M3-D2, M3-D3): this file reduces `--json` envelopes into
// view models, builds the argv arrays the widget may run, classifies argv the CLI hands
// out, and holds the scheduler's pure decisions (M3-D11). The service (Service.qml)
// only applies them, so bun tests the widget's logic against envelopes the real binary
// writes (M3-D41).
//
// The model is four files, each importing only the one below it, so that none passes
// the 512 KiB a file the plugin marketplace's scan reads (omad-widget-plugin-3bcbh.1;
// tests/budget.test.js holds each to 384 KiB):
//
// - Words.js: the words (COPY, ERROR_WORDS, WARNING_WORDS) and copy();
// - Shared.js: the helpers and patterns the views share with this file;
// - View.js: what the views show (History, the key map, the Updates and Installed rows,
//   colours, the addon page, the Errors view, Backups);
// - Model.js, this file: what the widget runs and every decision about running it (the
//   settings, the templates, argv and the classifier, the scheduler, the reducers,
//   activity, the plan runner and the plan sheet, the doctor's repairs), and the views
//   that need those.
//
// A file can read only what it imports, and only qualified (`View.addonPage`), so each
// file names every top-level name of the one below it again, right after its `.import`
// (`var addonPage = View.addonPage`): code reads the same in every file, and QML and bun
// still see one namespace through this file (`Model.addonPage`). tests/load.test.js
// holds every file to that. To add a function, put it in the file that holds what it
// reads; one that only views read goes in View.js, with its name added below.
//
// Loading: QML reads the `.import` line itself. bun cannot parse it, so the bun tests
// load this file through tests/load.js, which turns each `.import "X.js" as X` into
// X.js's top-level names, as QML gives them, on the same line.
//
// No `.pragma`: each importer gets its own copy, of every file. Engine-neutral
// JavaScript only (no QML globals, ES2017 at most, `var`/`function`), so Qt's V4 and bun
// behave alike. It ends with a guarded export, which tests/load.js returns.
.import "View.js" as View

// View.js's names, again: first those it names from Shared.js and Words.js, then its own,
// section by section.
var COPY = View.COPY
var ERROR_WORDS = View.ERROR_WORDS
var WARNING_WORDS = View.WARNING_WORDS
var copy = View.copy
var RELEASE_ID_PATTERN = View.RELEASE_ID_PATTERN
var RELEASE_ID_MAX_CHARS = View.RELEASE_ID_MAX_CHARS
var REVISION_PATTERN = View.REVISION_PATTERN
var VERSION_PATTERN = View.VERSION_PATTERN
var CHANNELS = View.CHANNELS
var LABEL_MAX_CHARS = View.LABEL_MAX_CHARS
var envelopeOk = View.envelopeOk
var plural = View.plural
var joinNames = View.joinNames
var num = View.num
var str = View.str
var strOrNull = View.strOrNull
var stringList = View.stringList
var INSTANT_PATTERN = View.INSTANT_PATTERN
var instantMs = View.instantMs
var sinceText = View.sinceText
var ageText = View.ageText
var isSafeHost = View.isSafeHost
var isSafeUrl = View.isSafeUrl
var dateText = View.dateText
var clockText = View.clockText
var SOURCE_WORDS = View.SOURCE_WORDS
var filterMatches = View.filterMatches
var NOTES_MAX_CHARS = View.NOTES_MAX_CHARS
var MORE_HOSTS = View.MORE_HOSTS
var nonEmpty = View.nonEmpty
var asciiLower = View.asciiLower
var shownOf = View.shownOf
var possessive = View.possessive
var sheetClient = View.sheetClient
// History
var HISTORY_LIMIT = View.HISTORY_LIMIT
var HISTORY_FIXED = View.HISTORY_FIXED
var HISTORY_ADDON_KINDS = View.HISTORY_ADDON_KINDS
var HISTORY_GLYPH = View.HISTORY_GLYPH
var AGENT_GLYPH = View.AGENT_GLYPH
var historyRef = View.historyRef
var reduceHistory = View.reduceHistory
var historyStanding = View.historyStanding
var historyItemText = View.historyItemText
var historyTitle = View.historyTitle
var historyReason = View.historyReason
var historyActor = View.historyActor
var historyRuns = View.historyRuns
var localDay = View.localDay
var wordAt = View.wordAt
var dayText = View.dayText
var whenText = View.whenText
var historyUndo = View.historyUndo
var historyRows = View.historyRows
var historyCount = View.historyCount
var historyPage = View.historyPage
// the panel frame
var VIEWS = View.VIEWS
var VIEW_TITLES = View.VIEW_TITLES
var LONG_LIST_ROWS = View.LONG_LIST_ROWS
var VIEW_GLYPHS = View.VIEW_GLYPHS
// the key map
var KEY_CONTEXTS = View.KEY_CONTEXTS
var KEYMAP = View.KEYMAP
var QT_KEY_NAMES = View.QT_KEY_NAMES
var QT_SHIFT = View.QT_SHIFT
var QT_CONTROL = View.QT_CONTROL
var keyName = View.keyName
var KEY_STEPS = View.KEY_STEPS
var keyStep = View.keyStep
var VIEW_BOXES = View.VIEW_BOXES
var keyScopes = View.keyScopes
var keyRows = View.keyRows
var keyRow = View.keyRow
var panelKey = View.panelKey
var dialogKey = View.dialogKey
var helpKey = View.helpKey
var actionRow = View.actionRow
var keyHint = View.keyHint
var withKey = View.withKey
var keyTip = View.keyTip
var contextTitle = View.contextTitle
var helpSections = View.helpSections
var stepView = View.stepView
var scopeTargets = View.scopeTargets
var liveTargets = View.liveTargets
var openingScope = View.openingScope
var keptScope = View.keptScope
var clientGroup = View.clientGroup
var scopeFacts = View.scopeFacts
var heroTitle = View.heroTitle
var heroState = View.heroState
var footerText = View.footerText
var scopeOptions = View.scopeOptions
var viewCounts = View.viewCounts
var targetById = View.targetById
var nextFilter = View.nextFilter
var stepFilter = View.stepFilter
var openTarget = View.openTarget
var updateRequest = View.updateRequest
var undoAction = View.undoAction
var progressFraction = View.progressFraction
// the Updates and Installed views, and colours
var SOURCE_KINDS = View.SOURCE_KINDS
var sourceKind = View.sourceKind
var UPDATE_KINDS = View.UPDATE_KINDS
var rgbOf = View.rgbOf
var luminance = View.luminance
var contrastRatio = View.contrastRatio
var mixColor = View.mixColor
var DIM_ALPHA_FLOOR = View.DIM_ALPHA_FLOOR
var DIM_MIN_CONTRAST = View.DIM_MIN_CONTRAST
var CURSOR_TINT_ALPHA = View.CURSOR_TINT_ALPHA
var dimAlpha = View.dimAlpha
var dimColor = View.dimColor
var tintedSurface = View.tintedSurface
var badgeColors = View.badgeColors
var stableSort = View.stableSort
var listTargets = View.listTargets
var releaseAge = View.releaseAge
var UPDATE_GLYPHS = View.UPDATE_GLYPHS
var UPDATE_BANDS = View.UPDATE_BANDS
var UPDATE_RANK = View.UPDATE_RANK
var rowRelease = View.rowRelease
var autoAfterText = View.autoAfterText
var updateRow = View.updateRow
var updateRows = View.updateRows
var groupUpdateRequest = View.groupUpdateRequest
var releaseNotes = View.releaseNotes
var updateCount = View.updateCount
var updateClients = View.updateClients
var oldestCheck = View.oldestCheck
// the Addons view's row
var installedRow = View.installedRow
// the addon page
var reduceInfo = View.reduceInfo
var COMPAT_WORDS = View.COMPAT_WORDS
var addonPage = View.addonPage
var ADDON_PAGE_CHANGES = View.ADDON_PAGE_CHANGES
var noteFirstLine = View.noteFirstLine
var dependencyInstall = View.dependencyInstall
var addonVersions = View.addonVersions
// the Errors view, and Search's requests and rows, which its section holds
var errorSourceText = View.errorSourceText
var errorMessageText = View.errorMessageText
var ownFrame = View.ownFrame
var errorSections = View.errorSections
var errorRows = View.errorRows
var errorPage = View.errorPage
var firstSeenText = View.firstSeenText
var changeSection = View.changeSection
var errorPageAction = View.errorPageAction
var errorReport = View.errorReport
var urgentView = View.urgentView
var issueRows = View.issueRows
var dismissPlan = View.dismissPlan
var searchRequest = View.searchRequest
var INSTALLABLE_HINTS = View.INSTALLABLE_HINTS
var SEARCH_DEBOUNCE_MS = View.SEARCH_DEBOUNCE_MS
var SEARCH_MIN_CHARS = View.SEARCH_MIN_CHARS
var searchesLive = View.searchesLive
var LINK_PREFIXES = View.LINK_PREFIXES
var LINK_HOSTS = View.LINK_HOSTS
var LINK_MAX_CHARS = View.LINK_MAX_CHARS
var searchLink = View.searchLink
var SEARCH_VERDICTS = View.SEARCH_VERDICTS
var searchVerdict = View.searchVerdict
var installedOf = View.installedOf
var installedIndex = View.installedIndex
var installedIn = View.installedIn
var installTargets = View.installTargets
var targetsFor = View.targetsFor
var searchVerdictOf = View.searchVerdictOf
var installedChip = View.installedChip
var searchRows = View.searchRows
var catalogRows = View.catalogRows
var catalogRequest = View.catalogRequest
var SEARCH_SOURCES = View.SEARCH_SOURCES
var rawLines = View.rawLines
var popularityText = View.popularityText
// Backups, a backup's page and the restore chooser
var bytesText = View.bytesText
var dotted = View.dotted
var backupKindText = View.backupKindText
var backupProof = View.backupProof
var autoSentence = View.autoSentence
var BACKUP_GLYPH = View.BACKUP_GLYPH
var RESTORE_GLYPH = View.RESTORE_GLYPH
var DELETE_GLYPH = View.DELETE_GLYPH
var backupWhen = View.backupWhen
var backupRow = View.backupRow
var backupCardLines = View.backupCardLines
var backupSections = View.backupSections
var backupRequest = View.backupRequest
var pruneRequest = View.pruneRequest
var restoreBlocked = View.restoreBlocked
var backupAddons = View.backupAddons
var backupPage = View.backupPage
var restoreRequest = View.restoreRequest
var managedAddons = View.managedAddons
var restoreChooser = View.restoreChooser

// ---------------------------------------------------------------- settings (M3-D10)

// Omarchy ignores manifest defaults, so they live here too; a test holds them equal to
// manifest.json's `barWidget.defaults`.
var DEFAULT_SETTINGS = {
  "binary": "",
  "panelWidth": 600,
  "autoCheck": "On",
  "notifyUpdates": "Off",
  "notifyErrors": "On",
  // Toasts while a game runs (1n2p.33): Off holds them for one digest after the session.
  "notifyWhilePlaying": "Off",
  "refreshIntervalSec": 300,
  // The first run's keybinding tip (1n2p.32): On until the player has seen it.
  "keyHint": "On",
  // The widget reinstalls itself once after its binary was upgraded (1n2p.35).
  "autoUpdateWidget": "On",
  // Routine updates installed by themselves after a background check (1n2p.36): Off
  // until the player turns it on.
  "autoUpdate": "Off"
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
  "keyHint": { "type": "switch" },
  "autoUpdateWidget": { "type": "switch" },
  "autoUpdate": { "type": "switch" }
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
var PANEL_SETTING_KEYS = ["panelWidth", "refreshIntervalSec", "autoCheck", "autoUpdate", "notifyErrors", "notifyUpdates", "notifyWhilePlaying",
  "autoUpdateWidget", "binary"]

// "30 s", "5 min", "1 min 30 s".
function secondsText(n) {
  var s = num(n)
  if (s < 60) return copy("settings.value.seconds", { "n": s })
  var minutes = Math.floor(s / 60)
  var rest = s % 60
  return rest === 0 ? copy("settings.value.minutes", { "n": minutes }) : copy("settings.value.minutes_seconds", { "n": minutes, "s": rest })
}

// A panel setting's value in words: "On", "Off", "600 px", "5 min", the binary's path.
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
function panelSettingRows(settings, autoUpdateStopped) {
  return PANEL_SETTING_KEYS.map(function(key) {
    var value = setting(settings, key)
    // Automatic updates a failure turned off say so until turned on again (1n2p.36).
    var stopped = key === "autoUpdate" && autoUpdateStopped === true && value === "Off"
    return { "section": "panel", "key": key, "kind": SETTING_RULES[key].type, "label": copy("settings.panel." + key),
      "about": stopped ? copy("settings.panel.autoUpdate.stopped") : copy("settings.panel." + key + ".about"), "value": value,
      "text": settingText(key, value) }
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
  { "key": "check_interval_hours", "template": "config_interval", "values": [1, 2, 3, 4, 6, 8, 12, 24, 48] },
  // How long an automatic update waits after omaddons first saw a release (RG-63,
  // design 7.7): 0 is no wait at all.
  { "key": "update_min_age_hours", "template": "config_min_age", "values": [0, 1, 3, 6, 12, 24, 48] }
]

// `config --json`: each setting's value and whether it is the default.
function reduceConfig(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || !Array.isArray(env.data.settings)) return null
  var settings = {}
  env.data.settings.forEach(function(s) {
    if (s && typeof s.name === "string") settings[s.name] = { "value": s.value, "source": str(s.source) }
  })
  // The file they live in, for "Everything else: config.toml" (omad-r0to).
  var path = str(env.data.path)
  return { "settings": settings, "path": PATH_PATTERN.test(path) ? path : "", "exists": env.data.exists === true }
}

// An omaddons setting's value in words ("Stable", "Safe ones", "6 h").
function configText(key, value) {
  if (key === "check_interval_hours") return copy("settings.value.hours", { "n": value })
  if (key === "update_min_age_hours") {
    return num(value) === 0 ? copy("settings.value.off") : copy("settings.value.hours", { "n": value })
  }
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
// The GitHub section (omad-r0to, design doc §2.7), from `status`'s `github` (null until a
// status says): the token's state in words, never the token; "Use the gh login" (`auth
// github --from-gh`) and "Forget the token" (`auth github clear`) where a stored token is
// what counts (the environment and config.toml outrank it); setting one by hand is a
// command to copy and run in a terminal, since the token goes only on standard input.
function githubSettingRows(github) {
  if (!github || typeof github !== "object") return []
  var source = str(github.source)
  var state = source === "config" && github.configured !== true ? "config_unreadable" : (source || "none")
  var rows = [{ "section": "github", "key": "github:state", "kind": "action", "action": "",
    "label": copy("settings.github.state." + state), "text": "", "about": copy("settings.github.state." + state + ".about") }]
  if (source === "none" || source === "file") {
    rows.push({ "section": "github", "key": "github:from_gh", "kind": "action", "action": "from_gh",
      "label": copy("settings.github.from_gh"), "text": "", "about": copy("settings.github.from_gh.about") })
    rows.push({ "section": "github", "key": "github:set", "kind": "action", "action": "copy_set",
      "label": copy("settings.github.set"), "text": "", "about": copy("settings.github.set.about") })
  }
  if (source === "file") {
    rows.push({ "section": "github", "key": "github:clear", "kind": "action", "action": "clear",
      "label": copy("settings.github.clear"), "text": "", "about": copy("settings.github.clear.about") })
  }
  return rows
}

// What "Set a token in a terminal" copies: the token on standard input, never argv.
var GITHUB_SET_COMMAND = "echo \"$TOKEN\" | omaddons auth github set"

// The sheet's last line (design doc §2.7): everything else lives in config.toml, opened
// in the player's editor (configEditorArgv) once `config` said where it is.
function otherSettingRows(config) {
  var path = config && typeof config === "object" ? str(config.path) : ""
  if (!configEditorArgv(path)) return []
  return [{ "section": "other", "key": "other:config", "kind": "action", "action": "open_config", "path": path,
    "label": copy("settings.other.config"), "text": "", "about": copy("settings.other.config.about", { "path": path }) }]
}

function settingsSections(input) {
  var i = input || {}
  var sections = [{ "key": "panel", "title": copy("settings.section.panel"), "rows": panelSettingRows(i.settings, i.autoUpdateStopped), "note": "" }]
  sections.push({ "key": "omaddons", "title": copy("settings.section.omaddons"), "rows": i.config ? omaddonsSettingRows(i.config) : [],
    "note": i.config ? "" : (i.configFailed ? copy("settings.omaddons.failed") : copy("settings.omaddons.reading")) })
  sections.push({ "key": "clients", "title": copy("settings.section.clients"), "rows": clientSettingRows(i.targets), "note": "" })
  var github = githubSettingRows(i.github)
  if (github.length) sections.push({ "key": "github", "title": copy("settings.section.github"), "rows": github, "note": "" })
  var other = otherSettingRows(i.config)
  if (other.length) sections.push({ "key": "other", "title": copy("settings.section.other"), "rows": other, "note": "" })
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
// The largest addon list a paste may hold (1n2p.37): what `sync -` reads from standard
// input (SYNC_STDIN_MAX_BYTES; tests/omarchy_widget.rs holds them equal). A larger paste
// is refused before anything runs.
var PASTE_MAX_BYTES = 64 * 1024
// How long reading the clipboard may take before the paste says it could not; a plain UI
// choice, as a copy's.
var WIDGET_PASTE_DEADLINE_MS = 3000

// ------------------------------------------------------ the templates (M3-D2)

// The settings sheet's (1n2p.31): `config` reads omaddons' settings whole, and each
// `config_*` sets one key named in its own argv, so no other key is reachable from the
// panel; its value is a `{value}` CONFIG_VALUE_PATTERN admits. A client's own settings
// (`config_client_*`) name the client by its key inside theirs (KEYED_SETTINGS), and its
// name is the one free text a change takes (`{name}`, one line); `targets_default`
// makes a client the default. Its GitHub section (omad-r0to) stores the token the gh login
// prints (`auth_from_gh`) or forgets the stored one (`auth_clear`); setting a token by
// hand stays in a terminal (standard input only), so no template takes one.
// BEGIN COMMANDS: checked by tests/omarchy_widget.rs; every entry must parse as an omaddons command line
// F1 ships the entries for commands that exist at F1 time; F8 adds errors_target and backup_auto (B6, D3);
// F6 adds errors, errors_raw and dismiss_errors (B6, B8); F9 adds backups, backup_target, backup_all,
// restore, restore_addon and prune_backup (D2, D4, D7, D8). G2 adds doctor_fix and doctor_fix_target
// (E6). A `preview` template runs only as its dry run; its confirmed run is `apply` of that preview's
// plan (M4-D28): `match` applies a routine plan that is what the view showed, `confirm` always asks
// first. `bind` runs first as its dry run too, and its confirmed run is the same argv bound to the
// preview's token (`--if-match`): a binding path that stores no plan (M4-D44). Sharing an
// addon set (1n2p.37): `export_*` read the set to copy (export without `--write` writes
// nothing, so they are reads), and `sync_*` preview a pasted list, which they take on
// standard input (`stdin: "list"`, checked by pastedList) and only as that preview: Yes
// applies its plan, which holds the text. RG-36 adds `skip_release` and `unskip_release`
// (the Updates view's Skip this release and Offer again), each with a `_rev` form for a
// release whose source numbers revisions: confirmed previews that name the row's release
// by its id (`{release_id}`) and revision (`{revision}`), never a version label.
var COMMANDS = {
  "status":         { "argv": ["status", "--cached", "-v"],                         "mutates": false, "lane": "local" },
  "status_full":    { "argv": ["status", "-v"],                                     "mutates": false, "lane": "local" },
  "list":           { "argv": ["list", "-v"],                                       "mutates": false, "lane": "local" },
  "changelog":      { "argv": ["info", "-t", "{target}", "--changelog", "--", "{addon}"], "mutates": false, "lane": "network" },
  "addon_info":     { "argv": ["info", "--changelog", "--", "{ref}"],               "mutates": false, "lane": "network" },
  "addon_releases": { "argv": ["info", "--releases", "--", "{ref}"],                "mutates": false, "lane": "network" },
  "search":         { "argv": ["search", "--limit", "20", "-t", "{target}", "--", "{query}"], "mutates": false, "lane": "network" },
  "check":          { "argv": ["outdated"],                                         "mutates": false, "lane": "network" },
  "update_all":     { "argv": ["update"],                                           "mutates": true, "preview": "match" },
  "update_all_auto": { "argv": ["update", "--auto"],                                "mutates": true, "preview": "match" },
  "update_target":  { "argv": ["update", "-t", "{target}"],                         "mutates": true, "preview": "match" },
  "update_addon":   { "argv": ["update", "-t", "{target}", "--", "{addon}"],        "mutates": true, "preview": "match" },
  "reinstall":      { "argv": ["install", "-t", "{target}", "--reinstall", "--", "{addon}"], "mutates": true, "preview": "confirm" },
  "remove":         { "argv": ["remove", "-t", "{target}", "--", "{addon}"],        "mutates": true, "preview": "confirm" },
  "pin":            { "argv": ["pin", "-t", "{target}", "--", "{addon}"],           "mutates": true },
  "unpin":          { "argv": ["unpin", "-t", "{target}", "--", "{addon}"],         "mutates": true },
  "skip_release":   { "argv": ["skip", "--release={release_id}", "-t", "{target}", "--", "{addon}"], "mutates": true, "preview": "confirm" },
  "skip_release_rev": { "argv": ["skip", "--release={release_id}", "--revision={revision}", "-t", "{target}", "--", "{addon}"], "mutates": true, "preview": "confirm" },
  "unskip_release": { "argv": ["unskip", "--release={release_id}", "-t", "{target}", "--", "{addon}"], "mutates": true, "preview": "confirm" },
  "unskip_release_rev": { "argv": ["unskip", "--release={release_id}", "--revision={revision}", "-t", "{target}", "--", "{addon}"], "mutates": true, "preview": "confirm" },
  "install":        { "argv": ["install", "-t", "{target}", "--", "{ref}"],         "mutates": true, "preview": "match" },
  "install_version": { "argv": ["install", "-t", "{target}", "--version={version}", "--", "{ref}"], "mutates": true, "preview": "confirm" },
  "update_channel": { "argv": ["update", "-t", "{target}", "--channel={channel}", "--", "{addon}"], "mutates": true, "preview": "confirm" },
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
  "config_min_age":    { "argv": ["config", "set", "update_min_age_hours", "--", "{value}"], "mutates": true },
  "config_client_name": { "argv": ["config", "set", "targets.{target}.name", "--", "{name}"], "mutates": true },
  "config_client_family": { "argv": ["config", "set", "targets.{target}.family", "--", "{value}"], "mutates": true },
  "targets_default":   { "argv": ["targets", "default", "--", "{target}"],         "mutates": true },
  "auth_from_gh":      { "argv": ["auth", "github", "--from-gh"],                    "mutates": true },
  "auth_clear":        { "argv": ["auth", "github", "clear"],                        "mutates": true },
  "export_all":        { "argv": ["export"],                                       "mutates": false, "lane": "local" },
  "export_target":     { "argv": ["export", "-t", "{target}"],                     "mutates": false, "lane": "local" },
  "sync_all":          { "argv": ["sync", "-v", "-"],                              "mutates": true, "preview": "confirm", "stdin": "list" },
  "sync_target":       { "argv": ["sync", "-v", "-", "-t", "{target}"],            "mutates": true, "preview": "confirm", "stdin": "list" }
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
  "skip":              { "mutates": true,  "destructive": true,  "binds": true },
  "unskip":            { "mutates": true,  "destructive": false, "binds": true },
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
  "install-completions": { "mutates": true, "destructive": false, "binds": false },
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
  "name": true, "version": true, "channel": true, "release_id": true, "revision": true }
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
  // Standard input (1n2p.37): a pasted addon list, for `sync -` alone and only as its
  // confirmed preview, so nothing the player pasted runs before Yes.
  if (template.hasOwnProperty("stdin")) {
    if (template.stdin !== "list" || path !== "sync" || argv.indexOf("-") === -1 || template.preview !== "confirm") {
      throw refuse("bad_template", name + " takes standard input other than as sync -'s confirmed preview")
    }
  }
  // An export copies a set and writes nothing: never `--write` (1n2p.37).
  if (path === "export" && (argv.indexOf("--write") !== -1 || argv.indexOf("--overwrite") !== -1)) {
    throw refuse("bad_template", name + " writes a file")
  }
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
  if (slot === "version" && !VERSION_PATTERN.test(value)) {
    throw refuse("bad_version", name + ": " + JSON.stringify(value) + " is no release version the panel can pass")
  }
  if (slot === "channel" && CHANNELS.indexOf(value) === -1) {
    throw refuse("bad_channel", name + ": " + JSON.stringify(value) + " is no channel")
  }
  if (slot === "release_id" && (!RELEASE_ID_PATTERN.test(value) || value.length > RELEASE_ID_MAX_CHARS)) {
    throw refuse("bad_release", name + ": " + JSON.stringify(value) + " is no release id the panel can pass")
  }
  if (slot === "revision" && (!REVISION_PATTERN.test(value) || Number(value) > 4294967295)) {
    throw refuse("bad_revision", name + ": " + JSON.stringify(value) + " is no revision")
  }
  if (slot === "query" && template.mutates) throw refuse("query_in_mutation", name)
}

function binaryOf(opts) {
  if (typeof opts.binary !== "string" || opts.binary.charAt(0) !== "/") {
    throw refuse("bad_binary", "the binary must be an absolute path")
  }
  return opts.binary
}

// `--reason`: "<button label> (bar widget)". A label is one line; past LABEL_MAX_CHARS it
// is cut to that many, ending in "…" (a client's own name may run to 200 characters, and
// a label that names it still runs).
function reasonOf(opts) {
  var label = opts.label
  if (typeof label !== "string" || label === "" || /[\r\n]/.test(label)) {
    throw refuse("bad_label", "a button label is one line, never empty")
  }
  if (label.length > LABEL_MAX_CHARS) label = label.slice(0, LABEL_MAX_CHARS - 1) + "…"
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
  // Standard input (1n2p.37): a template that takes a pasted list gets it checked again
  // here (pastedList), whoever built the request; every other template takes none.
  var stdin = null
  if (template.stdin === "list") {
    var list = pastedList(o.stdin)
    if (!list.ok) throw refuse("bad_input", name + ": the pasted text is no addon list (" + list.reason + ")")
    stdin = list.text
  } else if (o.stdin !== undefined && o.stdin !== null) {
    throw refuse("stdin_not_taken", name + " takes no standard input")
  }
  // Every change, and every read that asks the sources: the activity bar counts them, and
  // their progress keeps a network read's inactivity deadline alive (1n2p.18).
  var progress = template.mutates === true || template.lane === "network"
  var built = { argv: standardFlags(o, progress, preview, ifMatch, template.actor === false).concat(filled), env: contextOf(o.context) }
  if (stdin !== null) built.stdin = stdin
  return built
}

// --------------------------------------------------------- sharing a set (1n2p.37)

// A table header a pasted addon list may hold: a family's or a client's section, or the
// table form of one of its entries (`[[family.mainline.addon]]`).
var LIST_HEADER = /^\[\[?\s*(family|target)\s*\.\s*("[^"\\\n]*"|'[^'\n]*'|[A-Za-z0-9_-]+)(\s*\.\s*addon)?\s*\]\]?\s*(#.*)?$/

// A pasted addon list, checked before it reaches the CLI: `{ok, text, reason}`. The CLI
// reads it in full (`sync -`) and says what is wrong with one line of it; this refuses
// only what is plainly no addon list, with the reason a player is told: nothing to paste
// (`empty`), more than PASTE_MAX_BYTES (`too_large`), a control character (`not_text`),
// or no family or client section, a table of another kind, or a top-level key other
// than `schema_version` (`not_a_list`: a settings file, a link, a chat line).
function pastedList(text) {
  var refused = function(reason) { return { "ok": false, "text": "", "reason": reason } }
  if (typeof text !== "string" || text.trim() === "") return refused("empty")
  if (utf8Length(text) > PASTE_MAX_BYTES) return refused("too_large")
  if (/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(text)) return refused("not_text")
  var sections = 0
  var lines = text.split("\n")
  for (var i = 0; i < lines.length; i++) {
    var line = lines[i].trim()
    if (line === "" || line.charAt(0) === "#") continue
    // A table header (an addon list's arrays hold names and refs, never an array, so no
    // other line starts with a bracket).
    if (line.charAt(0) === "[") {
      if (!LIST_HEADER.test(line)) return refused("not_a_list")
      sections++
      continue
    }
    if (sections === 0 && !/^schema_version\s*=/.test(line)) return refused("not_a_list")
  }
  return sections > 0 ? { "ok": true, "text": text, "reason": "" } : refused("not_a_list")
}

// Why a paste ran nothing, in the player's words: the clipboard could not be read
// (`unreadable`: wl-paste is missing, gave no answer in time, or found no text), or
// pastedList's reason.
function pasteRefusal(reason) {
  var known = ["unreadable", "empty", "too_large", "not_text", "not_a_list"]
  return copy("share.paste." + (known.indexOf(reason) !== -1 ? reason : "not_a_list"), { "kb": Math.round(PASTE_MAX_BYTES / 1024) })
}

// Why the CLI refused a pasted list, from its code and typed details (`sync -` reads it
// as `<stdin>`, a name a player never sees): where and what kind of mistake; null for
// any other answer, whose own words stand.
function pasteFailure(env) {
  var error = env && env.error && typeof env.error === "object" ? env.error : null
  if (!error || error.code !== "desired_state_invalid") return null
  var details = error.details && typeof error.details === "object" ? error.details : {}
  var reason = str(details.reason)
  var line = num(details.line)
  if (reason === "newer_schema") return copy("share.paste.invalid.newer_schema")
  if (line <= 0) return copy("share.paste.invalid.nowhere")
  return copy(["unknown_family", "duplicate"].indexOf(reason) !== -1 ? "share.paste.invalid." + reason : "share.paste.invalid", { "line": line })
}

// The clipboard's reader: wl-paste's text, as it was copied (no newline added), of a
// text type only.
function pasteArgv() {
  return ["wl-paste", "--no-newline", "--type", "text"]
}

// The share line's requests for the chosen client (`filter`), or every client: the
// export a copy reads, and the sync a paste previews. `targets` name the client.
function exportRequest(filter, targets) {
  if (filter) {
    return { "template": "export_target", "slots": { "target": filter },
      "label": copy("share.copy.label.client", { "owner": possessive(sheetClient(targets, filter, "")) }) }
  }
  return { "template": "export_all", "slots": {}, "label": copy("share.copy.label") }
}

function syncRequest(filter, targets) {
  if (filter) {
    return { "template": "sync_target", "slots": { "target": filter },
      "label": copy("share.paste.label.client", { "client": sheetClient(targets, filter, "") }) }
  }
  return { "template": "sync_all", "slots": {}, "label": copy("share.paste.label") }
}

// An export's answer as what a copy puts on the clipboard: `{ok, text, entries, line}`,
// `line` the flash's words before wl-copy answers (copyFlash says whether it took the
// text). Nothing to copy, or an export that failed, is `ok: false` with why.
function exportedList(env, filter, targets) {
  var client = filter ? sheetClient(targets, filter, "") : ""
  var out = { "ok": false, "text": "", "entries": 0, "line": copy("share.copy.failed") }
  if (!envelopeOk(env) || !env.ok || !env.data || typeof env.data !== "object") return out
  var entries = num(env.data.entries)
  if (typeof env.data.content !== "string" || entries <= 0) {
    out.line = filter ? copy("share.copy.nothing.client", { "client": client }) : copy("share.copy.nothing")
    return out
  }
  // A list no paste could take (pastedList's bound) is not copied: wl-copy takes it as
  // one argument, and the paste on the other side would refuse it.
  if (utf8Length(env.data.content) > PASTE_MAX_BYTES) {
    out.line = copy("share.copy.too_large", { "kb": Math.round(PASTE_MAX_BYTES / 1024) })
    return out
  }
  out.ok = true
  out.text = env.data.content
  out.entries = entries
  out.line = filter ? copy("share.copy.done.client", { "owner": possessive(client), "n": entries }) : copy("share.copy.done", { "n": entries })
  return out
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

// The clipboard's argv for `text` (a command a row shows, an error report, an addon
// list): wl-copy, never through a shell, with the text on its standard input, which the
// copier writes and closes (Service.qml), never as an argument: a process's arguments
// are readable from /proc by every local account (omad-widget-plugin-3bcbh.5). The type
// is named, as wl-copy names it for text it is given as arguments, so it never guesses
// one from the content (an addon list would read as TOML); null for nothing to copy or a
// NUL byte.
function copyArgv(text) {
  if (typeof text !== "string" || text === "" || text.indexOf("\u0000") !== -1) return null
  return ["wl-copy", "--type", "text/plain"]
}

// What a copy's flash says once wl-copy answered (UX-36): `copied`, the site's own
// words for what went on the clipboard, when wl-copy took the text (`ok` true), else
// that nothing was copied. Never "Copied" for a copy that did not happen.
function copyFlash(ok, copied) {
  return ok === true ? str(copied) : copy("errors.copy_failed")
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
  // The fix button says what it does in its label, and has no tooltip (omad-vqkd).
  if (way) out.buttons.push({ "name": "fix", "text": way.label, "tooltip": "" })
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
    "updates": { "available": num(updates.available), "targets": stringList(updates.targets),
      // How many of them `update --auto` is expected to take now (RG-63): what the
      // automatic update starts on. A binary that does not report it waits for nothing.
      "autoReady": typeof updates.auto_ready === "number" ? num(updates.auto_ready) : num(updates.available) },
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
    // Unmanaged folders a take-over would adopt (the catalog's, and those their TOCs
    // identify, omad-3oru), and dependencies nothing needs: what the Issues view's
    // suggestions and the first run's card count (1n2p.28).
    "adoptable": num(target.adoptable),
    "orphans": num(target.orphans),
    "updates": updates ? {
      "available": num(updates.available), "pinned": num(updates.pinned), "blocked": num(updates.blocked),
      "failed": num(updates.failed), "unchecked": num(updates.unchecked),
      // Addons whose newest release is skipped (RG-31): never counted as available.
      "skipped": num(updates.skipped),
      // Of `available`, those an automatic update waits on (RG-63): first seen less than
      // `update_min_age_hours` ago.
      "tooNew": num(updates.too_new),
      "checkedAt": strOrNull(updates.checked_at), "stale": updates.stale === true
    } : null,
    "issues": issues,
    "errors": errors ? {
      // "buggrabber", or "none" with no recorder installed (the first run's card, 1n2p.32).
      "capture": str(errors.capture),
      "state": str(errors.state), "coverage": str(errors.coverage), "new": num(errors["new"]),
      "total": num(errors.total), "addons": num(errors.addons),
      "top": Array.isArray(errors.top_addons) ? errors.top_addons.map(function(t) {
        return { "addon": str(t.addon), "folder": str(t.folder), "new": num(t["new"]), "count": num(t.count),
          // Its strongest current link, validated by status before it shows (RG-17).
          "afterChange": linkBriefOf(t.after_change) }
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

// `status --cached -v`'s envelope as the widget's model: `{failed, targets, summary,
// journal, lastChange, widget, commands, warnings, issues}`. A run that failed (an
// envelope with `ok: false`) keeps no data and is `failed`; null (no answer yet) is
// not. `journal` is `{pending, inProgress}`: interrupted changes, and whether another
// omaddons process is committing now. The bar button's model is barState's, over this.
function reduceStatus(env) {
  if (!envelopeOk(env) || !env.ok || !env.data || typeof env.data !== "object") {
    return {
      "failed": !!env && typeof env === "object",
      "targets": [], "summary": null, "journal": null, "lastChange": null, "widget": null, "github": null, "commands": [], "warnings": [],
      "issues": []
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
    // The GitHub token's state (the settings sheet's GitHub section, omad-r0to): never
    // the token, which `status` never reads into its answer.
    "github": data.github && typeof data.github === "object"
      ? { "configured": data.github.configured === true, "source": str(data.github.source) || "none" } : null,
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
      // The ids of the clients it is about (omad-5cg5); empty from an older omaddons.
      "targets": Array.isArray(w.targets) ? w.targets.filter(function(t) { return typeof t === "string" && t !== "" }) : [],
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

// The bar button's name for a screen reader (1n2p.40): the widget, then its tooltip's
// first line, what the count counts or what keeps the bar from saying ("WoW addons: 2
// new Lua errors · 1 update"). The rest of the tooltip is its description.
function barAccessibleName(bar) {
  var tip = listOf(bar && typeof bar === "object" ? bar.tooltip : null)
  var head = tip.length ? str(tip[0]) : ""
  return head === "" ? copy("bar.accessible") : copy("bar.accessible.summary", { "summary": head })
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

// Whether the widget reinstalls itself now, after its binary was upgraded (1n2p.35):
// `{run, version, why}`, `why` the first reason it does not ("" when it runs). It runs
// only when another omaddons version answered (`incompatible`, incompatibleOf's, of kind
// "version" with the version read), from the binary that installed it (no `binary`
// setting: a build the player pointed it at may be a test build), whose status (`env`)
// says the installed widget is outdated and serves this state (as `status` recommends
// the reinstall), with no client running (unknown counts as running), the
// `autoUpdateWidget` setting On, and not tried for that version yet in this service's
// life (`tried`). Never for the same version with other files: a reinstall from that
// binary leaves the widget at its version, so a fresh service, which remembers nothing,
// never meets the "version" case again until the binary changes, and no reload loop can
// start. Otherwise the phase card offers the update.
function selfUpdate(input) {
  var i = input && typeof input === "object" ? input : {}
  var answer = function(why, version) { return { "run": why === "", "version": str(version), "why": why } }
  var incompatible = i.incompatible && typeof i.incompatible === "object" ? i.incompatible : null
  if (i.phase !== "incompatible" || !incompatible) return answer("compatible")
  if (incompatible.kind !== "version") return answer("same_version")
  var version = str(incompatible.theirs)
  if (version === "") return answer("unreadable")
  if (setting(i.settings, "autoUpdateWidget") !== "On") return answer("setting_off", version)
  if (setting(i.settings, "binary") !== "") return answer("binary_set", version)
  if (str(i.tried) === version) return answer("tried", version)
  var data = i.env && i.env.data && typeof i.env.data === "object" ? i.env.data : {}
  var widget = data.widget && typeof data.widget === "object" ? data.widget : null
  if (!widget || widget.installed !== true || widget.outdated !== true) return answer("not_outdated", version)
  if (widget.context_matches === false) return answer("other_state", version)
  var summary = data.summary && typeof data.summary === "object" ? data.summary : null
  var running = summary && Array.isArray(summary.running) ? summary.running : null
  if (running === null || running.length > 0) return answer("running", version)
  return answer("", version)
}

// Whether an automatic update starts now (1n2p.36): `{run, drop, why}`, `why` the first
// reason it does not ("" when it runs). One starts only when a background check asked
// for it (`pending`), with the `autoUpdate` setting On and not held off by a failure
// in this service's life (`held`), none already underway (`stage`), the binary
// answering this widget (`canAct`), no client running (`playing`: unknown counts as
// running), no change cut off (`interrupted`: the widget never finishes one by
// itself), nothing else underway (`busy`: a change running or queued, a plan sheet
// open), and updates `update --auto` is expected to take (`ready`, status's
// `auto_ready`, RG-63). `drop` says the request is spent: it runs, the setting is off,
// or there is nothing to update. A running game or a busy panel keeps it for a later
// status, so it runs once the game stops; so do updates that wait to settle (`waiting`,
// the clients' `too_new`), so it runs once one has, not at the next background check.
function autoUpdateDue(input) {
  var i = input && typeof input === "object" ? input : {}
  var answer = function(why, drop) { return { "run": why === "", "drop": drop === true, "why": why } }
  if (i.pending !== true) return answer("not_asked")
  if (setting(i.settings, "autoUpdate") !== "On") return answer("setting_off", true)
  if (i.held === true) return answer("held", true)
  if (str(i.stage) !== "") return answer("underway")
  if (i.canAct !== true) return answer("cannot_act")
  if (i.playing !== false) return answer("running")
  if (i.interrupted === true) return answer("interrupted")
  if (i.busy === true) return answer("busy")
  if (num(i.ready) <= 0 && num(i.waiting) > 0) return answer("waiting")
  if (num(i.ready) <= 0) return answer("no_updates", true)
  return answer("", true)
}

// How many updates wait to settle on `targets` (`status`'s clients, targetOf's): the sum
// of their `too_new` (RG-63).
function updatesWaiting(targets) {
  return (Array.isArray(targets) ? targets : []).reduce(function(sum, t) {
    return sum + (t && t.updates ? num(t.updates.tooNew) : 0)
  }, 0)
}

// What an automatic update does with its preview (1n2p.36), from the preview's lane
// `outcome`, its envelope and `decision`, Model.previewDecision's answer for it as a
// `match` of what the Updates view shows: "run" the plan only when the panel itself
// would apply it with no sheet (`direct`: routine, exactly the rows, nothing
// destructive); "leave" what needs the player (any sheet: a new dependency, local edits
// replaced, another source or channel, a downgrade, a release that moved) or may pass by
// itself; "none" when nothing is left to update; "stop" an error that would only come
// back.
function autoUpdatePreviewAct(outcome, env, decision) {
  var kind = decision && typeof decision === "object" ? decision.kind : ""
  if (outcome === "ok" && kind === "direct") return "run"
  if (outcome === "ok" && kind === "refresh") return "none"
  if (outcome === "ok" && kind === "dialog") return "leave"
  return autoUpdateFailureAct(outcome === "ok" ? "failed" : outcome, env)
}

// What an automatic update's apply came to (1n2p.36): "done" when every client
// committed; "leave" a plan gone stale or gone before it changed anything (the next
// check tries again) or a failure that may pass by itself; "stop" a failure, or a run
// that changed some clients and not the rest.
function autoUpdateApplyAct(outcome, env) {
  if (outcome === "ok" && envelopeOk(env) && env.ok === true && env.partial !== true) return "done"
  if (envelopeOk(env) && Object.keys(changedRows(env)).length === 0 && needsPreviewAgain(env)) return "leave"
  return autoUpdateFailureAct(outcome === "ok" ? "failed" : outcome, env)
}

// An automatic update's answer that is no success: "leave" what may pass by itself (no
// envelope at all: no answer in time, a child let go; or an error the CLI calls
// retryable: the lock, the network), else "stop".
function autoUpdateFailureAct(outcome, env) {
  if (outcome !== "failed") return "leave"
  var error = envelopeOk(env) && env.error && typeof env.error === "object" ? env.error : null
  return error && error.retryable === true ? "leave" : "stop"
}

// What an automatic update changed (1n2p.36), from its apply's envelope: `{count,
// targets}`, how many addons and their clients' names, for its toast.
function autoUpdateFacts(env, targets) {
  var keys = Object.keys(changedRows(env))
  var ids = []
  keys.forEach(function(k) {
    var id = k.slice(0, k.indexOf("/"))
    if (ids.indexOf(id) === -1) ids.push(id)
  })
  return { "count": keys.length, "targets": ids.map(function(id) { return sheetClient(targets, id, "") }) }
}

// Old panel setting names and the new ones that replace them (1n2p.35): none yet. A
// rename adds `{from, to}` here, so a player's choice survives it.
var SETTING_RENAMES = []

// `settings` as this widget names them: each renamed key's value moved to its new name
// (unless that is set too), the old one dropped; a copy, never the shell's own object.
// `renames` defaults to SETTING_RENAMES (a test passes its own).
function migrateSettings(settings, renames) {
  var given = settings && typeof settings === "object" ? settings : {}
  var out = {}
  for (var key in given) if (given.hasOwnProperty(key)) out[key] = given[key]
  var list = Array.isArray(renames) ? renames : SETTING_RENAMES
  list.forEach(function(r) {
    if (!r || !out.hasOwnProperty(r.from)) return
    if (!out.hasOwnProperty(r.to)) out[r.to] = out[r.from]
    delete out[r.from]
  })
  return out
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
    // Only folders a take-over would adopt (`status`'s `adoptable`, omad-3oru): a card
    // whose Take over… finds nothing is never shown.
    if (num(t.addons) === 0 && num(t.adoptable) > 0 && !skipped("adopt", t.id)) {
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
            // current, update_available, pinned, skipped_release, ahead, blocked, failed,
            // unchecked, static.
            "status": str(latest.status),
            "updateAvailable": latest.status === "update_available",
            // Its newest release is skipped on this client (RG-31): a mark holds it.
            "skipped": latest.status === "skipped_release",
            // The skip marks the record keeps (`-v`), as `{version, origin}`.
            "skippedReleases": Array.isArray(a.skipped_releases) ? a.skipped_releases.map(function(m) {
              var mark = m && typeof m === "object" ? m : {}
              return { "version": str(mark.version), "origin": str(mark.origin) }
            }) : [],
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
            // Its folders gone from disk (an addon page's Folders marks them, omad-z3ur).
            "missingFolders": stringList(a.missing_folders),
            // The release `latest` names, as an update preview's approval names it: what
            // the plan runner compares with the plan (M4-D28).
            "channel": str(a.channel),
            "latestSource": strOrNull(latest.source),
            "latestReleaseId": strOrNull(latest.release_id),
            // Its source revision, where the source numbers them (a WowAce re-tag): with
            // the id, what Skip this release and Offer again name exactly (RG-36).
            "latestRevision": typeof latest.revision === "number" ? latest.revision : null,
            "latestAsset": strOrNull(latest.asset),
            // When that release was published, when its source says (1n2p.21).
            "publishedAt": strOrNull(latest.published_at),
            // When this machine first saw it, and when an automatic update would take it
            // (null: now, or no update) (RG-63): the Updates row's note.
            "firstSeenAt": strOrNull(latest.first_seen_at),
            "autoEligibleAt": strOrNull(latest.auto_eligible_at)
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

// What `errors` knows of when an error began (`first_seen`, RG-60): its basis, why, its
// times, and `words`, the COPY id of its sentence: the reason's own when it gives one,
// else the basis's. A missing field reads as null: older envelopes lack it.
function firstSeenOf(f) {
  if (!f || typeof f !== "object") return null
  var basis = str(f.basis)
  var reason = strOrNull(f.reason)
  return {
    "basis": basis,
    "reason": reason,
    "at": strOrNull(f.at),
    "observedAt": strOrNull(f.observed_at),
    "onsetAfterAt": strOrNull(f.onset_after_at),
    "onsetByAt": strOrNull(f.onset_by_at),
    "relatedFingerprint": strOrNull(f.related_fingerprint),
    "words": !basis ? null : reason ? "errors.first_seen.reason." + reason : "errors.first_seen." + basis
  }
}

// The tag a link puts on an addon's row: "after update" for an update, "after a change"
// for any other kind (design 7.7).
function changeTag(kind) {
  return "errors.tag." + (kind === "update" ? "update" : "change")
}

// An addon's strongest current link (`after_change` on an addon or a status top addon),
// with its row tag. Null when there is none.
function linkBriefOf(b) {
  if (!b || typeof b !== "object") return null
  var kind = str(b.kind)
  return {
    "tx": str(b.tx), "kind": kind, "at": str(b.at), "addon": str(b.addon),
    "relation": str(b.relation), "confidence": str(b.confidence),
    "tag": changeTag(kind),
    "confidenceWords": b.confidence ? "errors.confidence." + str(b.confidence) : null
  }
}

// One error's link to the change before it (`after_change`, design 7.3.1), as the error
// page shows it: what changed, the confidence word, the weakening facts, the words of
// each unknown and of a link that no longer stands, and the ways back the binary
// decided (`commands`, run as given: the widget holds none of the rules of 7.4).
function linkOf(l) {
  if (!l || typeof l !== "object") return null
  var evidence = l.evidence && typeof l.evidence === "object" ? l.evidence : {}
  var update = evidence.client_update && typeof evidence.client_update === "object" ? evidence.client_update : null
  var state = str(l.state)
  var kind = str(l.kind)
  var unknowns = stringList(evidence.unknowns)
  return {
    "tx": str(l.tx), "kind": kind, "at": str(l.at), "actor": str(l.actor),
    "relation": str(l.relation), "confidence": str(l.confidence),
    "confidenceWords": l.confidence ? "errors.confidence." + str(l.confidence) : null,
    "state": state,
    "current": state === "current",
    // A link that no longer stands is a sentence with no button.
    "stateWords": state && state !== "current" ? "errors.link." + state : null,
    "judgedAt": str(l.judged_at),
    "tag": changeTag(kind),
    "changed": (Array.isArray(l.changed) ? l.changed : []).map(function(c) {
      return {
        "addon": str(c.addon), "change": str(c.change),
        "fromVersion": strOrNull(c.from_version), "toVersion": strOrNull(c.to_version),
        "stillInstalled": c.still_installed === true
      }
    }),
    "sameSession": typeof evidence.same_session === "boolean" ? evidence.same_session : null,
    "cleanSessions": typeof evidence.clean_sessions === "number" ? evidence.clean_sessions : null,
    "exact": evidence.exact === true,
    "otherChanges": num(evidence.other_changes),
    "clientUpdate": update ? { "from": str(update.from_version), "to": str(update.to_version) } : null,
    "unknowns": unknowns,
    "unknownWords": unknowns.map(function(u) { return "errors.unknown." + u }),
    // One per way back, best first: `rollback` (Go back), `undo` (Undo that change),
    // `update` (Update); `run` is the classified argv the page's button runs.
    "commands": (Array.isArray(l.commands) ? l.commands : []).filter(function(c) {
      return c && Array.isArray(c.argv)
    }).map(function(c) {
      return { "action": str(c.action), "run": cliAction(c.command, c.argv, c.destructive) }
    })
  }
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
              "raw": raw ? { "message": strOrNull(raw.message), "stack": stringList(raw.stack), "locals": strOrNull(raw.locals) } : null,
              // When it began, and the change before it (RG-60).
              "firstSeen": firstSeenOf(e.first_seen),
              "afterChange": linkOf(e.after_change),
              "judgment": e.judgment && typeof e.judgment === "object"
                ? { "state": str(e.judgment.state), "gap": strOrNull(e.judgment.gap) } : null
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
            // Its strongest current link: the row's "after update" tag (RG-60).
            "afterChange": linkBriefOf(a.after_change),
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
// run; a row that changed says so this long (design doc §2.1).
var ACTIVITY_STALL_MS = 30000
var ACTIVITY_ELAPSED_MS = 5000
var ACTIVITY_ROW_DONE_MS = 4000
// How long the corner toast holds each kind of message (omad-92d5, amending §2.1 and
// §5.5), counted only while a panel is open and paused while the pointer is over it:
// the last result with its Undo (or until the next change); a refusal or a key's note,
// a sentence to read; a copy's flash ("Copied", UX-36).
var RESULT_HOLD_MS = 8000
var NOTE_HOLD_MS = 4000
var FLASH_HOLD_MS = 1500

var PROGRESS_PHASES = ["fetch", "download", "verify", "stage", "commit", "copy"]

// What a change does, by its template (a change without a plan to name it).
var TEMPLATE_OPS = {
  "update_all": "update", "update_all_auto": "update", "update_target": "update", "update_addon": "update", "install": "install",
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
  // A pasted list applied (1n2p.37), in History's word for a sync.
  if (command === "sync") return syncResultLine(data)
  // The GitHub section's two changes (omad-r0to), in its words.
  if (command === "auth github") {
    return copy(data && data.configured === true ? "settings.github.result.stored" : "settings.github.result.cleared")
  }
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

// A pasted list applied (1n2p.37): how many addons it changed, on how many clients; an
// addon installed here from another source was kept as its sheet said (never a
// failure); any other failure is named with its message, as every result's is.
function syncResultLine(data) {
  var targets = Array.isArray(data.targets) ? data.targets : []
  var changed = 0
  var touched = 0
  var kept = 0
  var failed = []
  for (var i = 0; i < targets.length; i++) {
    var items = targets[i] && Array.isArray(targets[i].items) ? targets[i].items : []
    var before = changed
    for (var j = 0; j < items.length; j++) {
      var item = items[j]
      if (isChange(item)) changed++
      else if (item && item.status === "failed") {
        var error = item.error && typeof item.error === "object" ? item.error : {}
        if (error.code === "ownership_conflict") kept++
        else failed.push({ "name": shareName(item), "error": error })
      }
    }
    if (changed > before) touched++
  }
  var line = changed === 0 ? copy("share.result.none")
    : touched > 1 ? copy("share.result.clients", { "n": changed, "m": touched }) : copy("share.result", { "n": changed })
  if (kept > 0) line += " · " + copy("share.result.kept", { "n": kept })
  if (failed.length) {
    line += " · " + copy("share.result.failed", { "addon": failed[0].name, "why": str(failed[0].error.message) })
    if (failed.length > 1) line += " " + copy("share.result.more", { "n": failed.length - 1 })
  }
  return line
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

// The word a failure says to search for (omad-z5il): when the CLI answered `not_found`
// with `omaddons search <word>` as its fix (an addon page's Install it of a name no source
// is known for), that word, which Search for it puts in Search's box; "" for any other
// failure. Typed fields only, never the message; the word is TOC text, shown as text.
function searchFixOf(env) {
  var error = env && env.error && typeof env.error === "object" ? env.error : null
  if (!error || error.code !== "not_found" || !Array.isArray(error.fix_argv)) return ""
  var argv = error.fix_argv
  var word = argv.length === 3 && argv[0] === "omaddons" && argv[1] === "search" ? argv[2] : ""
  return typeof word === "string" && word.trim() !== "" && !/[\u0000-\u001f]/.test(word) ? word : ""
}

// A failure's fix buttons, but the one Search for it stands for (omad-z5il): Search for it
// is the fix then, so what is left is marked an alternative (never labelled "Fix").
function fixButtonsBesideSearch(env) {
  var word = searchFixOf(env)
  var fixes = fixButtons(env)
  if (word === "") return fixes
  return fixes.filter(function(fix) {
    return !(fix.argv && fix.argv.length === 3 && fix.argv[1] === "search" && fix.argv[2] === word)
  }).map(function(fix) { return Object.assign({}, fix, { "alternative": true }) })
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

// ------------------------------------------------ the key map's commands (design doc §3)
//
// The key map is in View.js; these three of its functions read the classifier's verdict
// or build an argv the security scans hold to its tool, so they stay here.

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

// `o`: the selected target's AddOns folder for the desktop's opener (M3-D2: a literal
// tool, an argv); null without a selected target or an absolute client directory.
function addonsDirArgv(target) {
  if (!target || !target.clientDir || target.clientDir.charAt(0) !== "/") return null
  return ["xdg-open", target.clientDir.replace(/\/+$/, "") + "/Interface/AddOns"]
}

// The settings sheet's "Everything else: config.toml" (omad-r0to): the config file
// `config` reported, in the player's editor through Omarchy's own launcher (a literal
// tool, an argv); null for a path that is not absolute and one line.
function configEditorArgv(path) {
  var p = str(path)
  if (!PATH_PATTERN.test(p) || utf8Length(p) > PATH_MAX_BYTES) return null
  return ["omarchy-launch-config-editor", p]
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
// its own (all good is a check), and so does one client's empty view while another
// client has rows (`filteredEmpty`): its sentence is the same good news, naming the
// client (omad-prpj).
var STATE_GLYPHS = { "firstLoad": 0xF04E6, "failed": 0xF0028 }
var EMPTY_GLYPHS = { "updates": 0xF05E0, "installed": 0xF0431, "search": 0xF0349, "errors": 0xF05E0, "issues": 0xF05E0, "backups": 0xF003C,
  "history": 0xF02DA }

// The last `outdated` run (Check now, or the background check), as the Updates view
// qualifies its rows: when it ran, and whether a source did not answer, from the codes
// of its warnings, its failed items and its error. `sources` are the source kinds
// (`sourceKind`) whose answers it showed from memory because they did not answer, and
// `staleAt` the oldest of those answers' fetch times, from its typed `stale_answers`
// (omad-vsi8); an older omaddons gives none, and the notice then names no source.
function reduceCheck(env, outcome) {
  var out = { "at": "", "trouble": false, "failed": 0, "outcome": str(outcome), "sources": [], "staleAt": "" }
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
    var stale = Array.isArray(env.data.stale_answers) ? env.data.stale_answers : []
    for (var k = 0; k < stale.length; k++) {
      var answer = stale[k] && typeof stale[k] === "object" ? stale[k] : {}
      var kind = sourceKind(answer.ref)
      if (out.sources.indexOf(kind) === -1) out.sources.push(kind)
      var at = str(answer.fetched_at)
      if (!isNaN(instantMs(at)) && (!out.staleAt || instantMs(at) < instantMs(out.staleAt))) out.staleAt = at
    }
  } else if (env.error && typeof env.error === "object") {
    codes.push(str(env.error.code))
  }
  out.trouble = out.failed > 0 || codes.some(function(c) { return SOURCE_TROUBLE.indexOf(c) !== -1 })
  return out
}

// The Updates view's offline notice for a check with trouble: how many addons could not
// be checked; else which sources did not answer and how old the answer shown is
// ("Couldn't reach GitHub; showing what it said 2 hours ago."); else, without typed
// sources (an older omaddons, or the check itself failed), that some did not answer.
function checkTroubleText(check, nowMs) {
  if (num(check.failed) > 0) return copy("state.notice.check_failed", { "n": num(check.failed) })
  var sources = Array.isArray(check.sources) ? check.sources : []
  var age = sinceText(str(check.staleAt), nowMs)
  if (sources.length === 0 || !age) return copy("state.notice.check_unreachable")
  var names = []
  sources.forEach(function(kind) {
    var name = sourceWords(kind)
    if (names.indexOf(name) === -1) names.push(name)
  })
  return copy("state.notice.check_unreachable_named", { "n": names.length, "sources": joinNames(names), "age": age })
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
  // A failed read over rows says what is shown: an earlier read's, or, when the rows come
  // from elsewhere (Backups' cards from `status`, Issues'), that nothing could be read.
  if (failed) {
    out.notices.push(stateNotice("failed", viewHas(view, s, query, filter)
      ? copy("state.notice.failed", { "what": copy("state.what." + view), "age": readAge(view, s, nowMs) })
      : copy("state.failed." + view), stateAction("retry")))
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
      out.notices.push(stateNotice("offline", checkTroubleText(check, nowMs), stateAction("check")))
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

// A row's action, as a template run: Enter's, and `p`'s pin or unpin.
function rowAction(row, which) {
  var slots = { "target": row.target, "addon": row.name }
  if (which === "pin") return row.pinned ? { "template": "unpin", "slots": slots, "label": "Unpin " + row.name }
    : { "template": "pin", "slots": slots, "label": "Pin " + row.name }
  if (which === "update") return { "template": "update_addon", "slots": slots, "label": "Update " + row.name, "shown": [shownOf(row)] }
  // Skip this release on an update row, Offer again on a skipped one (RG-36): the row's
  // own release by its id, and its revision where it has one; nothing otherwise.
  if (which === "skip" && (row.kind === "update" || row.kind === "skipped")) {
    var release = rowRelease(row)
    if (!release) return null
    var verb = row.kind === "update" ? "skip_release" : "unskip_release"
    for (var slot in release) slots[slot] = release[slot]
    var id = row.kind === "update" ? "updates.reason.skip" : "updates.reason.offer"
    var label = copy(id, { "addon": row.name, "version": str(row.latest) })
    return { "template": release.revision ? verb + "_rev" : verb, "slots": slots,
      "label": label.length <= LABEL_MAX_CHARS ? label : copy(id, { "addon": row.name, "version": "" }).replace(/ +/g, " ").trim() }
  }
  if (which === "reinstall") return { "template": "reinstall", "slots": slots, "label": "Reinstall " + row.name }
  if (which === "remove") return { "template": "remove", "slots": slots, "label": "Remove " + row.name }
  return null
}

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
// agent skill is for agents, and a look at unmanaged folders is what `status` says when
// none of them can be taken over, which the panel leaves unsaid (omad-3oru: the take-over
// is `adopt`'s suggestion). (A plan's apply, which runs only from its own preview, is
// neither runnable nor a command to copy, so it is never listed either.)
var SUGGESTION_SILENT = ["install_skill", "list_unmanaged"]
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
// A warning's words in the Issues view: `issues.note.<code>.named` with the clients it
// is about by their player names (`{client}` the first, `{clients}` all of them joined),
// when it names any and this build has such words; else `issues.note.<code>`; else, a
// code from a newer omaddons, its message.
function noteWords(warning, about, targets) {
  var code = str(warning.code)
  var names = about.map(function(id) { return sheetClient(targets, id, "") })
  if (names.length > 0 && COPY.hasOwnProperty("issues.note." + code + ".named")) {
    return copy("issues.note." + code + ".named", { "client": names[0], "clients": joinNames(names) })
  }
  return COPY.hasOwnProperty("issues.note." + code) ? copy("issues.note." + code) : str(warning.message)
}

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

  // The warnings this view shows, by code: a code this build has words for gets them,
  // naming the clients its typed `targets` gives by their player names (omad-5cg5), or
  // none when it gives none; a code from a newer omaddons shows its message. Under one
  // client, only the notes about that client show. A prose fix is not shown (it names
  // commands and config keys); a fix the classifier accepts is a named button, one it
  // refuses a command to copy.
  var notes = []
  var shown = Array.isArray(warnings) ? warnings : []
  for (var w = 0; w < shown.length; w++) {
    var warning = shown[w]
    if (!warning || typeof warning !== "object" || NOTE_ELSEWHERE.indexOf(warning.code) !== -1) continue
    var about = Array.isArray(warning.targets) ? warning.targets : []
    if (filter && about.indexOf(filter) === -1) continue
    var words = noteWords(warning, about, targets)
    if (words === "") continue
    var note = fixFields(issueFix(warning.cli, targets), null, warning.cli, false, false)
    note.prose = ""
    var single = about.length === 1 ? about[0] : ""
    notes.push(Object.assign({
      "kind": "note", "key": keyOf("n/" + str(warning.code) + "/" + about.join(",")), "target": single,
      "client": single ? sheetClient(targets, single, "") : "", "severity": "", "severityWord": "",
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
  // The addons whose new errors came after their own change, by the status brief's
  // `high` link (RG-61): their names only.
  var after = []
  var list = Array.isArray(targets) ? targets : []
  for (var i = 0; i < list.length; i++) {
    var errors = list[i].errors
    if (!errors || errors["new"] <= 0) continue
    where.push(str(list[i].displayName) || str(list[i].id))
    for (var j = 0; j < errors.top.length; j++) {
      if (errors.top[j]["new"] <= 0) continue
      names.push(errors.top[j].addon)
      counts.push(errors.top[j]["new"])
      var link = errors.top[j].afterChange
      if (link && link.confidence === "high" && after.indexOf(errors.top[j].addon) === -1) after.push(errors.top[j].addon)
    }
  }
  return { "names": names, "counts": counts, "targets": where, "after": after }
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

// One word of a command line a shell will run (never `shellWord`, which quotes for
// display): as it is when it holds only characters no shell reads, else in single
// quotes, inside which a shell expands nothing ('' for the empty word).
function shellQuoted(text) {
  var s = String(text)
  return /^[A-Za-z0-9_.\/:=@%+-]+$/.test(s) ? s : "'" + s.split("'").join("'\\''") + "'"
}

// The command a click on a toast runs: the panel of `pluginId` opened at `view` through
// its IPC (`show`, which opens, never toggles; "" opens it where it would open anyway).
// `omarchy-notification-send --exec` takes the command as one string, which the Omarchy
// shell runs when the card is clicked; it holds the plugin's own id and a view's name
// only, each one shell word.
function toastClickCommand(pluginId, view) {
  return ["omarchy-shell", shellQuoted(pluginId), "show", shellQuoted(view || "")].join(" ")
}

// A toast as an argv for `omarchy-notification-send`, under the widget's own app name
// (never one that passes Do Not Disturb), its click command the one argument after
// `--exec`: the tool reads every word after that as an option of `notify-send`, which
// refuses the toast, so the command is never spread over several.
function toastArgv(title, body, urgency, pluginId, view) {
  return ["omarchy-notification-send", "--app-name", "omaddons", "-g", TOAST_GLYPH, "-u", urgency, title, body,
    "--exec", toastClickCommand(pluginId, view)]
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
  // The addons a `high` link ties to their own update (RG-61), by name: "after DBM's update".
  var after = Array.isArray(facts && facts.after) ? facts.after.map(String) : []
  var title = where.length ? copy("toast.errors.title.on", { "clients": joinNames(where.map(String)) }) : copy("toast.errors.title")
  var body = errorParts(names, counts).join(" · ")
  if (after.length) body += " · " + copy("toast.errors.after", { "n": after.length, "addons": joinNames(after) })
  return toastArgv(title, body, "normal", pluginId, "errors")
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

// "Addons updated" · "Updated 4 addons on Retail and Classic Era · Undo is in History":
// an automatic update's toast (1n2p.36), counts and client names only (`{count,
// targets}`, Model.autoUpdateFacts'). A click opens History, where its run is first.
function autoUpdateToastArgv(facts, pluginId) {
  var count = num(facts && facts.count)
  var where = Array.isArray(facts && facts.targets) ? facts.targets.map(String) : []
  var body = where.length ? copy("toast.auto_update.on", { "n": count, "clients": joinNames(where) }) : copy("toast.auto_update", { "n": count })
  return toastArgv(copy("toast.auto_update.title"), body, "low", pluginId, "history")
}

// "Automatic updates are off" · "An automatic update on Retail failed. Turn them on
// again in Settings.": the one toast for the failure that turned them off (1n2p.36),
// client names only (`{targets}`, Model.updateFacts'). A click opens the panel, whose
// card says what failed.
function autoUpdateStoppedToastArgv(facts, pluginId) {
  var where = Array.isArray(facts && facts.targets) ? facts.targets.map(String) : []
  var body = where.length ? copy("toast.auto_update.stopped.on", { "clients": joinNames(where) }) : copy("toast.auto_update.stopped")
  return toastArgv(copy("toast.auto_update.stopped.title"), body, "normal", pluginId, "")
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

// Whether the last result leaves the corner when its hold ends (omad-92d5): a partial
// result whose failed items have Fix buttons stays until the next result, since its
// fixes are offered nowhere else.
function resultTimed(result) {
  return !!result && !(Array.isArray(result.fixes) && result.fixes.length > 0)
}

// What the panel's corner toast shows (omad-92d5), `{kind, text, marked}` or null. A
// passing note (`{kind, text, marked}`: a copy's flash, a refusal, a key's note) shows
// while it lasts, over the last result. Otherwise the last result shows while the
// service holds it (`held`) and no activity shows. A failure the card already states
// is the card's alone (UX-28, omad-uuwz); a partial result keeps its line.
function toastView(note, result, held, activity, failure) {
  if (note && str(note.text) !== "") {
    return { "kind": str(note.kind) || "note", "text": str(note.text), "marked": note.marked === true }
  }
  if (!result || held !== true || activity) return null
  var line = str(result.line)
  if (line === "") return null
  if (result.ok === false && result.partial !== true && failure) return null
  return { "kind": "result", "text": line, "marked": result.ok === false }
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
var PLAN_PATHS = ["install", "update", "pin", "unpin", "skip", "unskip", "adopt", "sync", "copy", "remove", "undo", "backup restore", "backup prune"]
// END PLAN_PATHS
var PLAN_ID_PATTERN = /^[0-9A-HJKMNP-TV-Z]{26}$/
// The fields of an approval's selection item, as the widget keeps them.
var APPROVAL_FIELDS = ["target_key", "target_id", "addon", "source", "channel", "release_id", "asset", "from_version", "to_version", "op"]
// The fields a shown item and a plan item must both carry, and agree on, to match.
var MATCH_FIELDS = ["target_key", "addon", "source", "channel", "release_id", "asset", "from_version", "to_version"]

function storesPlan(path) {
  return PLAN_PATHS.indexOf(path) !== -1
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

// The glyph of each effect kind (design doc §5.4), and the kinds that cost the player
// something they have now: those show first, in the urgent colour.
var SHEET_GLYPHS = {
  "dependency_added": 0xF0431, "folders_removed": 0xF0258, "local_changes_replaced": 0xF0DC9,
  "source_changed": 0xF059F, "channel_changed": 0xF0093, "while_running": 0xF040A, "reinstall": 0xF006F,
  "force": 0xF0026, "downgrade": 0xF02DA, "still_required": 0xF0026, "changed_since": 0xF02DA,
  "restore_scope": 0xF006F, "kept_backup": 0xF003C, "verified": 0xF05E0, "undo_last": 0xF054C,
  "delegated": 0xF04E6, "pins": 0xF0403, "note": 0xF02FD
}
var SHEET_URGENT = ["folders_removed", "local_changes_replaced", "force", "still_required", "changed_since", "restore_scope"]
// The destructive verb's glyph, by what the sheet changes (design doc §5.4).
var SHEET_VERB_GLYPHS = { "remove": 0xF01B4, "prune": 0xF01B4, "forget": 0xF01B4, "undo": 0xF054C, "restore": 0xF006F }

function sheetEffect(kind, text) {
  return { "kind": kind, "severity": SHEET_URGENT.indexOf(kind) !== -1 ? "urgent" : "normal",
    "glyph": SHEET_GLYPHS.hasOwnProperty(kind) ? SHEET_GLYPHS[kind] : SHEET_GLYPHS.note, "text": text }
}

// A sheet item: its name (which elides) and its detail (versions, which never do).
function sheetItem(fields) {
  var f = fields && typeof fields === "object" ? fields : {}
  var item = {
    "op": str(f.op), "addon": str(f.addon), "client": str(f.client), "clientKey": str(f.clientKey) || str(f.client),
    "from": str(f.from), "to": str(f.to), "name": str(f.name), "detail": str(f.detail), "header": "",
    // The group it is listed under on its client ("New", a pasted list's sheet), or "".
    "group": str(f.group)
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

// A rollback's sheet item (RG-61): "Foo goes back to v1: the files that were installed
// before; v2 is skipped", the skipped versions from its client's `skips_marked`.
function rollbackItem(item, data) {
  var targets = data && Array.isArray(data.targets) ? data.targets : []
  var left = []
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i] && typeof targets[i] === "object" ? targets[i] : {}
    var target = t.target && typeof t.target === "object" ? t.target : {}
    if (str(target.key) !== item.clientKey && str(target.id) !== item.clientKey) continue
    var notes = Array.isArray(t.skips_marked) ? t.skips_marked : []
    for (var j = 0; j < notes.length; j++) {
      var note = notes[j] && typeof notes[j] === "object" ? notes[j] : {}
      if (asciiLower(str(note.addon)) === asciiLower(item.addon) && str(note.version)) left.push(str(note.version))
    }
  }
  var detail = copy("sheet.item.rollback", { "to": item.to || "?" })
  if (left.length) detail += copy("sheet.item.rollback.skipped", { "versions": joinNames(left) })
  return sheetItem({ "op": item.op, "addon": item.addon, "client": item.client, "clientKey": item.clientKey, "from": item.from,
    "to": item.to, "name": item.name, "detail": detail })
}

// A skip's or an unskip's sheet item (RG-36): the versions of the marks its client's
// `skips_marked` (skip) or `skips_cleared` (unskip) note for the addon, "v2 (skipped)";
// the item as it was when the run notes none.
function skipItem(item, data, kind) {
  var key = kind === "skip" ? "skips_marked" : "skips_cleared"
  var targets = data && Array.isArray(data.targets) ? data.targets : []
  var versions = []
  for (var i = 0; i < targets.length; i++) {
    var t = targets[i] && typeof targets[i] === "object" ? targets[i] : {}
    var target = t.target && typeof t.target === "object" ? t.target : {}
    if (str(target.key) !== item.clientKey && str(target.id) !== item.clientKey) continue
    var notes = Array.isArray(t[key]) ? t[key] : []
    for (var j = 0; j < notes.length; j++) {
      var note = notes[j] && typeof notes[j] === "object" ? notes[j] : {}
      if (asciiLower(str(note.addon)) === asciiLower(item.addon) && str(note.version)) versions.push(str(note.version))
    }
  }
  if (versions.length === 0) return item
  return sheetItem({ "op": item.op, "addon": item.addon, "client": item.client, "clientKey": item.clientKey, "from": item.from,
    "to": item.to, "name": item.name, "detail": copy("sheet.item." + kind, { "versions": joinNames(versions) }) })
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

// What an adopting preview says of its addons and of the folders it leaves (omad-kbtn),
// from its typed `identifications` and `left_unmanaged`: `sure`, each adopted addon's
// `{tier, reason}` by "<client id>/<addon>", and `left`, each client's groups left
// unmanaged (`{target, primary, reason}`), or null from an omaddons that does not say.
function adoptionOf(data) {
  var sure = {}
  var left = null
  var targets = Array.isArray(data.targets) ? data.targets : []
  targets.forEach(function(t) {
    if (!t || typeof t !== "object") return
    var id = t.target && typeof t.target === "object" ? str(t.target.id) : ""
    var identified = Array.isArray(t.identifications) ? t.identifications : []
    identified.forEach(function(i) {
      if (i && typeof i === "object") sure[id + "/" + str(i.addon)] = { "tier": str(i.tier), "reason": str(i.reason) }
    })
    if (!Array.isArray(t.left_unmanaged)) return
    left = left || []
    t.left_unmanaged.forEach(function(g) {
      if (g && typeof g === "object") left.push({ "target": id, "primary": str(g.primary), "reason": str(g.reason) })
    })
  })
  return { "sure": sure, "left": left }
}

// An adopted addon's line: its version, then how sure omaddons is of it, in words from
// its typed tier and reason, never from a message.
function adoptItem(item, sure) {
  if (!sure) return item
  var words = ""
  if (sure.tier === "exact") words = copy("sheet.adopt.exact")
  else if (sure.tier === "identified") {
    words = COPY.hasOwnProperty("sheet.adopt.identified." + sure.reason) ? copy("sheet.adopt.identified." + sure.reason)
      : copy("sheet.adopt.identified")
  } else if (sure.tier === "guess") words = copy("sheet.adopt.guess")
  if (!words) return item
  return sheetItem(Object.assign({}, item, { "detail": item.detail ? item.detail + " · " + words : words }))
}

// The folders a take-over leaves as they are, each by name with why: one line, or one
// per client when it spans several; none when it leaves nothing. From an omaddons that
// does not say, the general sentence.
function adoptStays(left, targets) {
  if (left === null) return [sheetEffect("note", copy("sheet.effect.adopt_unknown"))]
  var order = []
  var named = {}
  left.forEach(function(g) {
    if (!named.hasOwnProperty(g.target)) {
      named[g.target] = []
      order.push(g.target)
    }
    var why = COPY.hasOwnProperty("sheet.adopt.why." + g.reason) ? copy("sheet.adopt.why." + g.reason) : copy("sheet.adopt.why.other")
    named[g.target].push(copy("sheet.adopt.stays_one", { "folder": g.primary, "why": why }))
  })
  return order.map(function(id) {
    var slots = { "n": named[id].length, "folders": joinNames(named[id]), "client": sheetClient(targets, id, "") }
    return sheetEffect("note", copy(order.length > 1 ? "sheet.effect.adopt_stays.client" : "sheet.effect.adopt_stays", slots))
  })
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
  var requested = requestedItems(approval)
  var items = requested.map(function(it) { return selectionItem(it, targets, folders) })
  var reinstall = approval.reasons.indexOf("reinstall") !== -1
  var kinds = { "update": "update", "install": "install", "remove": "remove", "adopt": "adopt", "pin": "pin", "unpin": "unpin",
    "skip": "skip", "unskip": "unskip" }
  var kind = reinstall ? "reinstall" : (kinds.hasOwnProperty(path) ? kinds[path] : "change")
  // A rollback (`skip --back`, RG-61): its client's change is an update, back to the
  // files installed before, and the release it leaves is skipped.
  var dataTargets = Array.isArray(data.targets) ? data.targets : []
  if (kind === "skip" && dataTargets.some(function(t) { return !!t && t.kind === "update" })) kind = "rollback"
  // A skip or an unskip moves marks, never the installed release (RG-36): each item names
  // the release it marks or offers again, from the run's own notes.
  if (kind === "skip" || kind === "unskip") items = items.map(function(item) { return skipItem(item, data, kind) })
  if (kind === "rollback") items = items.map(function(item) { return rollbackItem(item, data) })
  var effects = reasonEffects(approval, items, targets, data)
  // A take-over (1n2p.32): the player's main worry first. Nothing moves; each addon says
  // how sure omaddons is of it, and the folders it leaves say why (omad-kbtn).
  if (kind === "adopt") {
    var adoption = adoptionOf(data)
    items = items.map(function(item, n) {
      return adoptItem(item, adoption.sure[str(requested[n].target_id) + "/" + str(requested[n].addon)])
    })
    effects = [sheetEffect("note", copy("sheet.effect.adopt_in_place"))].concat(adoptStays(adoption.left, targets)).concat(effects)
  }
  return { "title": countedTitle(kind, items), "items": items, "effects": effects.concat(commonEffects(env, items, effects)),
    "confirmLabel": countedConfirm(kind, items), "glyph": kind === "remove" ? SHEET_VERB_GLYPHS.remove : 0 }
}

// The groups of a pasted list's sheet, in the order a player reads them: what it would
// change of addons already there first, then what it installs, what is already as the
// list has it, and what it leaves out.
var SHARE_GROUPS = ["conflict", "new", "unchanged", "left"]

// An entry of a pasted list by the name a player knows: the addon's name when the
// preview found it, else what the list wrote, a GitHub ref by its repository's name
// (`gh:o/qux` is "qux").
function shareName(it) {
  var name = str(it.name)
  if (name) return name
  var request = str(it.request)
  var gh = /^gh:[^\/]+\/([^\/]+)$/.exec(request)
  return gh ? gh[1] : request
}

// One `sync` item of a pasted list's preview as `{group, item}`, on `client`: its group
// and its line. `fields` are the record fields an edit changes (from the approval).
// `sync -v` lists the addons already as the list has them (`unchanged`), too.
function shareItem(it, client, clientKey, fields) {
  var status = str(it.status)
  var name = shareName(it)
  var from = str(it.from_version)
  var to = str(it.to_version)
  var code = it.error && typeof it.error === "object" ? str(it.error.code) : ""
  var group = "left"
  var detail = ""
  if (status === "installed") {
    group = "new"
    detail = copy("sheet.item.install", { "version": to }).replace(/^ /, "")
  } else if (status === "unchanged") {
    group = "unchanged"
    detail = from || to
  } else if (status === "updated") {
    group = "conflict"
    detail = from && to && from !== to ? copy("share.item.updated", { "from": from, "to": to }) : (to || from)
  } else if (status === "pinned") {
    group = "conflict"
    detail = copy("share.item.pinned", { "version": to || from })
  } else if (status === "unpinned") {
    group = "conflict"
    detail = copy("share.item.unpinned", { "version": to || from })
  } else if (status === "edited") {
    group = "conflict"
    detail = fields.length ? copy("sheet.item.edit", { "fields": joinNames(fields.map(function(f) { return f.replace(/_/g, " ") })) })
      : copy("share.item.edited")
  } else if (status === "failed" && code === "ownership_conflict") {
    // Installed here from another source: the list's is not installed over it.
    group = "conflict"
    detail = copy("share.item.other_source")
  } else if (status === "failed") {
    detail = copy(["not_found", "no_compatible_release", "source_disabled", "ref_ambiguous"].indexOf(code) !== -1
      ? "share.item.failed." + code : "share.item.failed", { "client": client })
  } else if (status === "skipped") {
    detail = copy(str(it.skip_reason) === "duplicate" ? "share.item.duplicate" : "share.item.skipped")
  }
  return { "group": group, "item": sheetItem({ "op": status, "addon": name, "client": client, "clientKey": clientKey,
    "from": from, "to": to, "name": name, "detail": detail, "group": copy("share.group." + group) }) }
}

// A pasted list's sheet (1n2p.37): what `sync -` would do on each client the list
// reaches, its items grouped as SHARE_GROUPS (each client's under its name when there
// are several), the effects its approval names, and the notes the list's own warnings
// call for (by code, never their text). Addons the list does not have stay as they are:
// the panel never syncs with `--prune`, and the sheet says so.
function syncSheet(env, approval, targets) {
  var data = env.data && typeof env.data === "object" ? env.data : {}
  var list = Array.isArray(data.targets) ? data.targets : []
  var selection = Array.isArray(approval.selection) ? approval.selection : []
  var fieldsOf = function(key, id, addon) {
    for (var i = 0; i < selection.length; i++) {
      var s = selection[i]
      var here = (key && s.target_key === key) || (!key && s.target_id === id)
      if (s && here && asciiLower(str(s.addon)) === asciiLower(addon)) return stringList(s.fields)
    }
    return []
  }
  var items = []
  var changes = []
  for (var t = 0; t < list.length; t++) {
    var entry = list[t] && typeof list[t] === "object" ? list[t] : {}
    var target = entry.target && typeof entry.target === "object" ? entry.target : {}
    var key = str(target.key)
    var client = sheetClient(targets, str(target.id), key)
    var byGroup = { "conflict": [], "new": [], "unchanged": [], "left": [] }
    ;(Array.isArray(entry.items) ? entry.items : []).forEach(function(it) {
      if (!it || typeof it !== "object") return
      var one = shareItem(it, client, key || str(target.id), fieldsOf(key, str(target.id), shareName(it)))
      byGroup[one.group].push(one.item)
      if (isChange(it)) changes.push(one.item)
    })
    SHARE_GROUPS.forEach(function(g) { items = items.concat(byGroup[g]) })
  }
  var effects = reasonEffects(approval, changes, targets, data)
  var codes = Array.isArray(env.warnings) ? env.warnings.map(function(w) { return w && w.code }) : []
  if (codes.indexOf("desired_unknown_target") !== -1) effects.push(sheetEffect("note", copy("share.effect.unknown_target")))
  if (codes.indexOf("desired_unknown_key") !== -1) effects.push(sheetEffect("note", copy("share.effect.unknown_key")))
  effects.push(sheetEffect("note", copy("share.effect.kept")))
  var clients = sheetClients(items)
  return {
    "title": clients.length <= 1 ? copy("share.title.client", { "client": clients[0] || "" }) : copy("share.title.clients", { "m": clients.length }),
    "items": items, "effects": effects.concat(commonEffects(env, items, effects)),
    "confirmLabel": copy("share.confirm", { "n": changes.length }), "glyph": 0
  }
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
// when they span several (`header` on each group's first item), each client's items by
// their own `group` when they have one ("Retail · New", or "New" on one client), and the
// effects that cost something first, in their order otherwise.
function finishSheet(sheet) {
  var s = sheet && typeof sheet === "object" ? sheet : {}
  var items = Array.isArray(s.items) ? s.items : []
  var grouped = sheetClients(items).length > 1
  var last = null
  for (var i = 0; i < items.length; i++) {
    var group = str(items[i].group)
    var mark = items[i].clientKey + "\n" + group
    var title = group === "" ? (grouped ? items[i].client : "")
      : (grouped ? copy("sheet.group", { "client": items[i].client, "group": group }) : group)
    items[i].header = mark !== last ? title : ""
    last = mark
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
  } else if (path === "sync") {
    // The panel previews `sync` only for a pasted list (1n2p.37).
    sheet = syncSheet(env, approval, targets)
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
// shown, again, path, context, targets, pins}`: `match` compares the plan with what the
// view `shown`, `again` marks a preview run after a refused plan, `path` is the previewed
// command's, `context` and `targets` name clients and shorten paths, and `pins` says the
// change pins what it installs (`install --version`, 1n2p.24), which the plan does not
// name: the sheet says so. The decision
// is `{kind, plan, heading, sheet, line, fixText}`, its kind one of "failed", "refuse"
// (nothing ran, `line` says why), "refresh" (nothing to do: no line), "dialog" (ask, with the
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
    decision.line = copy("decision.unknown")
    return decision
  }
  if (data.plan_id === null && approval.selection.length === 0) {
    // Nothing to do: the view refreshes and nothing is said (omad-vqkd); the refreshed
    // view shows the state.
    decision.kind = "refresh"
    return decision
  }
  if (data.plan_id === null) {
    decision.line = copy("decision.not_kept")
    decision.fixText = warningFix(env, "plan_not_stored")
    return decision
  }
  var plan = planFrom(env)
  if (!plan) {
    decision.line = copy("decision.no_offer")
    return decision
  }
  decision.kind = "dialog"
  decision.plan = plan
  // Anything but a match is confirmed in the sheet: never applied directly.
  if (s.mode !== "match") {
    decision.sheet = previewSheet(s.path, env, approval, plan, s.context, s.targets)
    if (s.pins === true) decision.sheet.effects.push(sheetEffect("pins", copy("sheet.effect.pins")))
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
    // Nothing left to repair: the view refreshes, saying nothing (omad-vqkd).
    decision.kind = "refresh"
    return decision
  }
  decision.kind = "refuse"
  var mine = result.actions.concat(result.prunes)
  for (var i = 0; i < mine.length; i++) {
    if (str(mine[i].fm) !== str(slots.fm)) {
      decision.line = copy("decision.repair.more")
      return decision
    }
  }
  if (!result.precondition) {
    decision.line = copy("decision.repair.unbound")
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
    githubSettingRows: githubSettingRows,
    otherSettingRows: otherSettingRows,
    GITHUB_SET_COMMAND: GITHUB_SET_COMMAND,
    configEditorArgv: configEditorArgv,
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
    copyFlash: copyFlash,
    PASTE_MAX_BYTES: PASTE_MAX_BYTES,
    WIDGET_PASTE_DEADLINE_MS: WIDGET_PASTE_DEADLINE_MS,
    pastedList: pastedList,
    pasteRefusal: pasteRefusal,
    pasteFailure: pasteFailure,
    pasteArgv: pasteArgv,
    exportRequest: exportRequest,
    syncRequest: syncRequest,
    exportedList: exportedList,
    syncSheet: syncSheet,
    finishSheet: finishSheet,
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
    UPDATE_GLYPHS: UPDATE_GLYPHS,
    UPDATE_BANDS: UPDATE_BANDS,
    barStaleAfterMs: barStaleAfterMs,
    barState: barState,
    barTooltip: barTooltip,
    barAccessibleName: barAccessibleName,
    RETRY_BACKOFF_MS: RETRY_BACKOFF_MS,
    retryDelayMs: retryDelayMs,
    incompatibleOf: incompatibleOf,
    selfUpdate: selfUpdate,
    autoUpdateDue: autoUpdateDue,
    updatesWaiting: updatesWaiting,
    autoAfterText: autoAfterText,
    clockText: clockText,
    autoUpdatePreviewAct: autoUpdatePreviewAct,
    autoUpdateApplyAct: autoUpdateApplyAct,
    autoUpdateFacts: autoUpdateFacts,
    SETTING_RENAMES: SETTING_RENAMES,
    migrateSettings: migrateSettings,
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
    NOTE_HOLD_MS: NOTE_HOLD_MS,
    FLASH_HOLD_MS: FLASH_HOLD_MS,
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
    searchFixOf: searchFixOf,
    fixButtonsBesideSearch: fixButtonsBesideSearch,
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
    heroState: heroState,
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
    resultTimed: resultTimed,
    toastView: toastView,
    sourceKind: sourceKind,
    rgbOf: rgbOf,
    contrastRatio: contrastRatio,
    mixColor: mixColor,
    dimAlpha: dimAlpha,
    dimColor: dimColor,
    tintedSurface: tintedSurface,
    badgeColors: badgeColors,
    DIM_ALPHA_FLOOR: DIM_ALPHA_FLOOR,
    DIM_MIN_CONTRAST: DIM_MIN_CONTRAST,
    CURSOR_TINT_ALPHA: CURSOR_TINT_ALPHA,
    stableSort: stableSort,
    updateRows: updateRows,
    updateCount: updateCount,
    updateClients: updateClients,
    releaseAge: releaseAge,
    groupUpdateRequest: groupUpdateRequest,
    releaseNotes: releaseNotes,
    oldestCheck: oldestCheck,
    reduceCheck: reduceCheck,
    readingResources: readingResources,
    viewState: viewState,
    VIEW_RESOURCES: VIEW_RESOURCES,
    SOURCE_TROUBLE: SOURCE_TROUBLE,
    installedRows: installedRows,
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
    dependencyInstall: dependencyInstall,
    noteFirstLine: noteFirstLine,
    COMPAT_WORDS: COMPAT_WORDS,
    SEVERITY_GLYPHS: SEVERITY_GLYPHS,
    NOTE_ELSEWHERE: NOTE_ELSEWHERE,
    errorSections: errorSections,
    errorRows: errorRows,
    errorPage: errorPage,
    errorPageAction: errorPageAction,
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
    autoUpdateToastArgv: autoUpdateToastArgv,
    autoUpdateStoppedToastArgv: autoUpdateStoppedToastArgv,
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
    ERROR_WORDS: ERROR_WORDS,
    WARNING_WORDS: WARNING_WORDS,
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
