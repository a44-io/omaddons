// What the panel's views show (design doc §2, §7): History, the panel frame, the key
// map, the Updates and Installed rows, colours, the Addons view's row, the addon page,
// the Errors view with Search's rows, and Backups with the restore chooser. Pure
// functions over reduced envelopes, as in Model.js. Nothing here builds an argv: a
// request a view builds (`{template, slots}`) goes through Model.buildArgv like every
// other, and an argv the CLI printed is passed on only as the classifier in Model.js
// left it.
//
// The third of the model's four files (Model.js says how they fit): it imports
// Shared.js and names its helpers again, and Model.js names each of these again, so QML
// still reads `Model.addonPage`. Each section came here whole from Model.js
// (omad-widget-plugin-3bcbh.1), but for three of the key map's: forgetAction,
// addonsDirArgv and configEditorArgv stay in Model.js, beside the argv rules they use.
//
// No `.pragma`: each importer gets its own copy. Engine-neutral JavaScript only (no QML
// globals, ES2017 at most, `var`/`function`), so Qt's V4 and bun behave alike. bun loads
// it through tests/load.js, which gives its top-level names as QML's `.import` does.
.import "Shared.js" as Shared

// Shared.js's names, again.
var COPY = Shared.COPY
var ERROR_WORDS = Shared.ERROR_WORDS
var WARNING_WORDS = Shared.WARNING_WORDS
var copy = Shared.copy
var RELEASE_ID_PATTERN = Shared.RELEASE_ID_PATTERN
var RELEASE_ID_MAX_CHARS = Shared.RELEASE_ID_MAX_CHARS
var REVISION_PATTERN = Shared.REVISION_PATTERN
var VERSION_PATTERN = Shared.VERSION_PATTERN
var CHANNELS = Shared.CHANNELS
var LABEL_MAX_CHARS = Shared.LABEL_MAX_CHARS
var envelopeOk = Shared.envelopeOk
var plural = Shared.plural
var joinNames = Shared.joinNames
var num = Shared.num
var str = Shared.str
var strOrNull = Shared.strOrNull
var stringList = Shared.stringList
var INSTANT_PATTERN = Shared.INSTANT_PATTERN
var instantMs = Shared.instantMs
var sinceText = Shared.sinceText
var ageText = Shared.ageText
var isSafeHost = Shared.isSafeHost
var isSafeUrl = Shared.isSafeUrl
var dateText = Shared.dateText
var clockText = Shared.clockText
var SOURCE_WORDS = Shared.SOURCE_WORDS
var filterMatches = Shared.filterMatches
var NOTES_MAX_CHARS = Shared.NOTES_MAX_CHARS
var MORE_HOSTS = Shared.MORE_HOSTS
var nonEmpty = Shared.nonEmpty
var asciiLower = Shared.asciiLower
var shownOf = Shared.shownOf
var possessive = Shared.possessive
var sheetClient = Shared.sheetClient

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

// The local calendar day of an instant, as a day number for comparing days; NaN when it
// does not parse. Counted from the local date's own year, month and day, so two days in a
// row are always one apart: local midnight as an instant, divided and rounded, lands on
// either side of a half day wherever the zone is 12 hours or more from UTC, and a clock
// change across that line (New Zealand's, twice a year) made a day repeat or skip.
function localDay(ms) {
  var d = new Date(ms)
  return Date.UTC(d.getFullYear(), d.getMonth(), d.getDate()) / 86400000
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
//
// Three of its functions stay in Model.js, which this file cannot read: forgetAction
// (the classifier's verdict), and addonsDirArgv and configEditorArgv (argv the security
// scans hold to their tools).

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
  { "keys": ["S"], "context": "updates", "action": "skip", "label": "skip the release waiting, or offer a skipped one again", "hint": "S", "nothing": "Nothing to skip here" },
  { "keys": ["/"], "context": "updates", "action": "filter", "label": "filter the updates by name, source or state", "hint": "/", "nothing": "Nothing to filter here" },
  { "keys": ["Enter", "Space"], "context": "installed", "action": "enter", "label": "open the addon's page", "hint": "Enter" },
  { "keys": ["u"], "context": "installed", "action": "update", "label": "update this addon", "hint": "u", "nothing": "Nothing to update here" },
  { "keys": ["p"], "context": "installed", "action": "pin", "label": "pin or unpin it", "hint": "p", "nothing": "Nothing to pin here" },
  { "keys": ["x"], "context": "installed", "action": "remove", "label": "remove it, after a confirmation", "hint": "x", "nothing": "Nothing to remove here" },
  { "keys": ["r"], "context": "installed", "action": "restore", "label": "restore its settings from a backup", "hint": "r", "nothing": "No settings to restore here" },
  { "keys": ["/"], "context": "installed", "action": "filter", "label": "filter the list by name, source or state", "hint": "/", "nothing": "Nothing to filter here" },
  { "keys": ["S"], "context": "installed", "action": "sort", "label": "sort by name, by recently updated, or by source", "hint": "S", "nothing": "Nothing to sort here" },
  { "keys": ["y"], "context": "installed", "action": "copy", "label": "copy the chosen client's addon list (or every client's), to share it", "hint": "y", "nothing": "Nothing to copy here" },
  { "keys": ["Ctrl+V"], "context": "installed", "action": "paste", "label": "paste an addon list someone shared: see what it would change, then apply it", "hint": "Ctrl+V", "nothing": "Addon lists paste in Addons (2)" },
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
  { "keys": ["Enter", "Space"], "context": "page", "action": "enter", "label": "run the page's main action: a problem's fix, on an addon page the client line's repair, update or install (on a release note, show all of it or fold it), on a change's page the cursored addon's page, on an error page the first way back from the change before it (Go back), or on a backup's page the cursored addon's settings back", "hint": "Enter" },
  { "keys": ["u"], "context": "page", "action": "update", "label": "update the addon, when one is waiting (on an addon page, on the client line)", "hint": "u", "nothing": "Nothing to update here" },
  { "keys": ["i"], "context": "page", "action": "install", "label": "an addon page: install it on the client line's client", "hint": "i", "nothing": "Nothing to install here" },
  { "keys": ["x"], "context": "page", "action": "remove", "label": "remove the addon from the client line's client, or delete a backup's page's backup, after a confirmation", "hint": "x", "nothing": "Nothing to remove here" },
  { "keys": ["p"], "context": "page", "action": "pin", "label": "an addon page: pin or unpin it on the client line's client", "hint": "p", "nothing": "Nothing to pin here" },
  { "keys": ["r"], "context": "page", "action": "restore", "label": "restore the addon's settings from a backup, or choose what to restore from a backup's page's backup", "hint": "r", "nothing": "No settings to restore here" },
  { "keys": ["w"], "context": "page", "action": "web", "label": "an addon page: open its web page", "hint": "w", "nothing": "No web page here" },
  { "keys": ["v"], "context": "page", "action": "versions", "label": "an addon page: its versions and channels on the client line's client (again: close them)", "hint": "v", "nothing": "No versions to choose here" },
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

// The scope's name (§2.1), "All clients" or the client's player name: the Dropdown's
// closed text and the labels that name the client (Update, Forget, Search). The hero
// itself says only "omaddons" (omad-224l).
function heroTitle(targets, filter) {
  if (!filter) return copy("client.all")
  var t = targetById(targets, filter)
  return t ? (t.displayName || t.id) : filter
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
function updateRequest(filter, rows, targets, auto) {
  // The automatic update (RG-63): every client's, as `update --auto`, which takes only
  // what has settled; what it is expected to take leaves out each row that waits.
  var unattended = auto === true && !filter
  var request = filter ? { "template": "update_target", "slots": { "target": filter }, "label": "Update " + sheetClient(targets, filter, "") }
    : { "template": unattended ? "update_all_auto" : "update_all", "slots": {}, "label": "Update all" }
  if (Array.isArray(rows)) {
    request.shown = rows.filter(function(r) {
      return !!r && r.kind === "update" && (!filter || r.target === filter) && !(unattended && r.autoEligibleAt)
    }).map(shownOf)
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
var UPDATE_KINDS = { "update_available": "update", "pinned": "pinned", "skipped_release": "skipped", "blocked": "blocked", "failed": "failed" }

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
// The cursor row's tint: the kit's CursorSurface lays the foreground over the surface
// at its `hover-cursor-fill-alpha`, 0.08 by default. Dim text sits on it in every
// cursored row (the meta line), so it must stay readable there too (1n2p.40).
var CURSOR_TINT_ALPHA = 0.08

// The alpha `dimColor` lays the foreground at: the smallest from 0.55 up, in steps of
// 0.01, at which it reaches 4.5:1 on `bg` and on `bg` under the cursor's tint; 1 when
// even the foreground does not.
function dimAlpha(fg, bg) {
  var cursor = mixColor(fg, bg, CURSOR_TINT_ALPHA)
  for (var step = Math.round(DIM_ALPHA_FLOOR * 100); step <= 100; step++) {
    var dim = mixColor(fg, bg, step / 100)
    if (contrastRatio(dim, bg) >= DIM_MIN_CONTRAST && contrastRatio(dim, cursor) >= DIM_MIN_CONTRAST) return step / 100
  }
  return 1
}

// Secondary text (§5.1): the foreground over the surface it sits on (the popup
// background, or a card's tint: tintedSurface), dimmed as far as 4.5:1 allows there and
// under the cursor. Unlike Qt.darker it dims on a light theme too, where darker would
// stand out more than the foreground (UX-44).
function dimColor(fg, bg) {
  return mixColor(fg, bg, dimAlpha(fg, bg))
}

// A card's surface: `tint` laid over `bg` at `alpha`, opaque, as the card draws it (a
// failure's urgent at 0.10, a success's accent at 0.08, §5.1), for the dim text on it.
function tintedSurface(tint, bg, alpha) {
  return mixColor(tint, bg, alpha)
}

// The bar badge's fill and figures (§5.5, 1n2p.40): `{fill, text}`. The figures are in
// `bg` (the theme's background), or in `fg` where that reads better on the fill; when
// neither reaches 4.5:1 (catppuccin-latte's accent: 4.34), the fill moves toward the
// other colour in steps of 0.05 until the figures do, so the badge keeps the theme's
// accent or urgent as nearly as it can.
function badgeColors(fill, bg, fg) {
  var text = contrastRatio(bg, fill) >= contrastRatio(fg, fill) ? bg : fg
  var away = text === bg ? fg : bg
  var shown = rgbOf(fill) ? mixColor(fill, fill, 1) : fill
  for (var step = 1; step <= 20 && contrastRatio(text, shown) < DIM_MIN_CONTRAST; step++) {
    shown = mixColor(away, fill, step * 0.05)
  }
  return { "fill": shown, "text": text }
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
var UPDATE_GLYPHS = { "update": 0xF0CE1, "done": 0xF012C, "pinned": 0xF0403, "skipped": 0xF04AD, "blocked": 0xF073A, "failed": 0xF04E7 }
var UPDATE_BANDS = { "update": "accent", "done": "accent", "pinned": "", "skipped": "", "blocked": "", "failed": "urgent" }
var UPDATE_RANK = { "update": 0, "done": 0, "pinned": 1, "skipped": 2, "blocked": 3, "failed": 4 }

// The release a row names for Skip this release or Offer again (RG-36): its `latest`
// id, and its revision when the source numbers them, as `--release=` and `--revision=`
// take them; null when either is not one safe argument, and the row then offers neither
// (design 7.7: never a bare version label).
function rowRelease(row) {
  var id = str(row && row.latestReleaseId)
  if (!RELEASE_ID_PATTERN.test(id) || id.length > RELEASE_ID_MAX_CHARS) return null
  var revision = row.latestRevision
  if (revision === null || revision === undefined) return { "release_id": id }
  var text = typeof revision === "number" && isFinite(revision) ? String(revision) : ""
  if (!REVISION_PATTERN.test(text) || Number(text) > 4294967295) return null
  return { "release_id": id, "revision": text }
}

// When an automatic update takes a release that waits (RG-63), as its row says it:
// "00:52" within a day, else "Mon 00:52"; "" when it does not wait.
function autoAfterText(iso, nowMs) {
  var at = instantMs(str(iso))
  if (isNaN(at)) return ""
  if (typeof nowMs !== "number" || at - nowMs < 86400000) return clockText(iso)
  return copy("time.weekday", { "day": wordAt("time.weekdays.short", new Date(at).getDay()), "time": clockText(iso) })
}

// One Updates row (1n2p.21, design doc §5.3): an AddonRow's fields, and what the plan
// runner compares (shownOf's).
function updateRow(t, a, kind, status, client, nowMs, doneWord) {
  var meta = []
  var running = !!status && status.running === true
  if (kind === "update") {
    var age = releaseAge(a.publishedAt, nowMs)
    if (age) meta.push(age)
    // Design 7.7: "Automatic update after 00:52". Update takes it at once.
    var after = autoAfterText(a.autoEligibleAt, nowMs)
    if (after) meta.push(copy("updates.meta.auto_after", { "time": after }))
    if (running) meta.push(copy("updates.meta.running", { "client": client }))
  } else if (kind === "done") {
    meta.push(str(doneWord))
  } else if (kind === "pinned") {
    meta.push(copy("updates.meta.pinned", { "version": a.version || "?", "latest": a.latest || "" }))
  } else if (kind === "skipped") {
    meta.push(copy("updates.meta.skipped", { "version": a.version || "?", "latest": a.latest || "" }))
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
  // Skip this release on an update; Offer again on a skipped one (RG-36): one key, the
  // row's own release, and nothing when that release cannot be named exactly. Offer again
  // is a skipped row's main action, first, as Update is an update row's: a narrow row
  // shows only its first.
  if (kind === "update" && rowRelease(a)) actions.push(act("skip", 0xF04AD, "updates.action.skip"))
  if (kind === "skipped" && rowRelease(a)) actions.unshift(act("skip", 0xF054C, "updates.action.offer"))
  return {
    "key": t.id + "/" + a.name, "target": t.id, "client": client, "name": a.name, "ref": a.ref, "version": a.version, "latest": a.latest,
    "kind": kind, "pinned": a.pinned, "reload": running, "group": "", "groupCount": "", "groupTarget": "", "groupUpdates": 0,
    "targetKey": t.key, "channel": a.channel, "latestSource": a.latestSource, "latestReleaseId": a.latestReleaseId,
    "latestRevision": a.latestRevision === undefined ? null : a.latestRevision,
    // When an automatic update would take this update (null: now): such a row is not
    // in what the automatic update's preview expects (updateRequest's `auto`).
    "autoEligibleAt": kind === "update" ? strOrNull(a.autoEligibleAt) : null,
    "latestAsset": a.latestAsset, "sourceWord": SOURCE_WORDS[a.source] || copy("installed.source.unknown"),
    "fromVersion": kind === "done" ? "" : (a.version || "?"),
    "toVersion": kind === "update" ? str(a.latest) : (kind === "done" ? str(a.version) : ""),
    "meta": meta.join(" · "), "chips": chips, "glyph": UPDATE_GLYPHS[kind], "band": UPDATE_BANDS[kind], "actions": actions
  }
}

// The Updates view's rows, in the filter (1n2p.21, design doc §2.3): per client, its
// updates, then pinned addons with a newer release, blocked releases and failed checks,
// by name within each; each client's header (its player name, whether it runs, its
// updates) above its first row, with what "Update {client}" updates there.
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
    // The client's header above its first row: its player name, whether it runs, and its
    // updates (design doc §2.3). Under one client too, so its Update {client} stays where
    // it is under All when that client is chosen (omad-224l).
    if (rows.length > 0) {
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

// What `u` would update in the filter: the "Update all (N)" header's N.
function updateCount(rows) {
  return rows.filter(function(r) { return r.kind === "update" }).length
}

// How many clients have an update among `rows` (omad-sj3y): under All, Update all shows
// only past one, since a single client's header already updates exactly that.
function updateClients(rows) {
  var seen = {}
  var n = 0
  ;(Array.isArray(rows) ? rows : []).forEach(function(r) {
    if (!r || r.kind !== "update" || seen.hasOwnProperty(r.target)) return
    seen[r.target] = true
    n++
  })
  return n
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

// ----------------------------------------------------- the Addons view's row (1n2p.22)
//
// The addon page reads it too, so it lives below both; the Addons view's list, its sort
// and its filter stay with the views' states in Model.js.

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
    "changelogWhy": d.changelog_unavailable && typeof d.changelog_unavailable === "object" ? str(d.changelog_unavailable.reason) : "",
    // `--releases` (an addon page's Versions, 1n2p.24): newest first, each with its
    // date, channel and a hint per client; null when the source listed none
    // (`releasesWhy` says why) or the read did not ask.
    "releases": Array.isArray(d.releases) ? d.releases.map(function(r) {
      var hints = r && r.compat && typeof r.compat === "object" ? r.compat : {}
      var compatOf = {}
      for (var id in hints) if (hints.hasOwnProperty(id)) compatOf[id] = str(hints[id])
      return { "version": str(r.version), "publishedAt": str(r.published_at), "channel": str(r.channel) || "stable",
        "compat": compatOf }
    }).filter(function(r) { return r.version !== "" }) : null,
    "releasesWhy": d.releases_unavailable && typeof d.releases_unavailable === "object" ? str(d.releases_unavailable.reason) : ""
  }
}

// How likely a release loads on a client, in words (design doc §6.3): `info`'s compat.
var COMPAT_WORDS = { "verified": "page.compat.works", "declared": "page.compat.works", "likely": "page.compat.likely",
  "unlikely": "page.compat.not", "blocked": "page.compat.not" }

// The addon page (1n2p.23, design doc §2.4), from what the panel holds: `name` and `ref`
// (the row it opened from; `ref` may be null), `lists` (`list -v`, reduced: the local
// facts, there at once), `info` (the page's own `info` read, reduced, null until it
// answers), `reading` and `failed` (that read's state), `errors` (`errors`, reduced),
// `statusTargets` (`status`'s clients, for their names), `nowMs`, and for its Versions
// (1n2p.24) `versionsFor` (the client they are open on) and `releasesRead`. The answer:
//   - the header: `title` (its TOC's title, else its name), `name`, `summary`, `facts`
//     ("by …", popularity, when it last changed), `source` (a word), `homepage` (a safe
//     URL or "");
//   - `clients`: one line per client, where it is installed first (each an Addons-view
//     row, `row`, so every action runs as it does there), then where it is not, with how
//     likely the newest release loads there; each line's `actions` (named, with their
//     key: the cursored line's) and `primary` (Enter);
//   - `versions` (addonVersions) and `lines`, what the cursor walks: `clients`, then the
//     Versions' lines, then `notes`;
//   - `notes`: the release notes newer than the oldest installed version, newest first,
//     each a line (`kind: "note"`) folded to its first line but the newest, unless
//     `notesOpen` (`{version: bool}`, what Enter or a click changed) says otherwise
//     (omad-z3ur);
//   - `folders`: what it owns on each client, what that client leaves out, which of
//     its folders are gone from disk (`missing`, `missingText`), and the clients that
//     share that client's AddOns folder (`sharedText`), from `statusWarnings` (status's
//     `target_addons_shared` and its typed `targets`, omad-5cg5): a shared directory
//     shares every folder in it, so the client is marked, never a folder guessed;
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
    if (ref) act("versions", "page.versions.hint", 0xF03D6)
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
    var actions = installable ? [{ "name": "install", "text": copy("page.install_here"), "glyph": String.fromCodePoint(0xF0415),
      "tooltip": withKey(copy("page.install_here"), "install", "page") }] : []
    // A version of its own choosing, here too (1n2p.24).
    if (ref && name) actions.push({ "name": "versions", "text": copy("page.versions.hint"), "glyph": String.fromCodePoint(0xF03D6),
      "tooltip": withKey(copy("page.versions.hint"), "versions", "page") })
    clients.push({
      "key": "c/" + t.id, "target": t.id, "client": t.displayName || t.id, "installed": false, "row": null,
      "meta": copy("page.not_installed") + (COMPAT_WORDS.hasOwnProperty(verdict) ? " · " + copy(COMPAT_WORDS[verdict]) : ""),
      "chips": [], "band": "", "glyph": 0, "primary": installable ? "install" : "", "actions": actions
    })
  })

  var moreUrl = info && info.changelog ? info.changelog.moreUrl : ""

  // The clients sharing `id`'s AddOns folder, by their names: status's warning about
  // each group names its clients typed (`targets`), never in its message.
  var warnings = Array.isArray(i.statusWarnings) ? i.statusWarnings : []
  var sharingWith = function(id) {
    var others = []
    warnings.forEach(function(w) {
      var ids = w && w.code === "target_addons_shared" && Array.isArray(w.targets) ? w.targets : []
      if (ids.indexOf(id) === -1) return
      ids.forEach(function(t) { if (t !== id && others.indexOf(t) === -1) others.push(t) })
    })
    return others.map(function(t) { return sheetClient(statusTargets, t, "") })
  }
  var folders = records.map(function(rec) {
    var record = installedOn(rec.t.id)
    var client = sheetClient(statusTargets, rec.t.id, rec.t.key)
    var missing = rec.a.missingFolders || []
    var others = sharingWith(rec.t.id)
    return { "target": rec.t.id, "client": client, "folders": rec.a.folders,
      "leftOut": record ? record.leftOut : [],
      "missing": missing,
      "missingText": missing.length ? copy("page.folders.missing", { "n": missing.length, "folders": missing.join(", "), "client": client }) : "",
      "sharedText": others.length ? copy("page.folders.shared", { "client": client, "others": joinNames(others) }) : "" }
  })

  // What its TOCs need, and what needs it (`required_by`), each a page of its own.
  // A TOC requires folders: one an installed addon owns is that addon (DBM-Core is
  // DBM's, omad-z3ur), so it is neither listed as missing nor offered Install it. An
  // addon's own name wins over another's folder of the same name.
  var installedNames = {}
  listTargets.forEach(function(t) { t.addons.forEach(function(a) { installedNames[a.name.toLowerCase()] = a.name }) })
  listTargets.forEach(function(t) {
    t.addons.forEach(function(a) {
      (a.folders || []).forEach(function(f) {
        if (!installedNames.hasOwnProperty(f.toLowerCase())) installedNames[f.toLowerCase()] = a.name
      })
    })
  })
  var needNames = []
  var tocs = info ? info.installed.map(function(r) { return r.toc }).filter(function(t) { return !!t }) : []
  tocs.forEach(function(toc) {
    toc.dependencies.forEach(function(d) { if (needNames.indexOf(d) === -1) needNames.push(d) })
  })
  var needs = []
  needNames.forEach(function(d) {
    var known = installedNames.hasOwnProperty(d.toLowerCase()) ? installedNames[d.toLowerCase()] : ""
    var entry = { "name": known || d, "installed": !!known }
    if (!needs.some(function(n) { return n.name === entry.name })) needs.push(entry)
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

  // Its Versions on one client, when `v` opened them: lines after the client lines.
  var versions = addonVersions(i, name, ref, records, statusTargets, nowMs, clients.length)

  // The release notes the page's read brought, newest first, lines after the Versions'
  // (omad-z3ur): each version, its age, its text cut, and folded to its first line but
  // the newest, unless Enter or a click changed it (`notesOpen`, by version).
  var notesOpen = i.notesOpen && typeof i.notesOpen === "object" ? i.notesOpen : {}
  var notesFirst = clients.length + versions.lines.length
  var notes = info && info.changelog ? info.changelog.entries.map(function(e, n) {
    var age = e.publishedAt ? (sinceText(e.publishedAt, nowMs) || dateText(e.publishedAt)) : ""
    var text = e.text.length > NOTES_MAX_CHARS ? e.text.slice(0, NOTES_MAX_CHARS - 1) + "…" : e.text
    var first = noteFirstLine(text)
    var foldable = first !== text.trim()
    var open = notesOpen.hasOwnProperty(e.version) ? notesOpen[e.version] === true : n === 0
    return { "kind": "note", "key": "n/" + n, "index": notesFirst + n, "version": e.version, "when": age, "text": text,
      "title": [e.version, age].filter(function(p) { return p !== "" }).join(" · "),
      "foldable": foldable, "folded": foldable && !open, "shown": foldable && !open ? first + " …" : text,
      "hint": foldable ? withKey(copy(open ? "page.notes.fold" : "page.notes.unfold"), "enter", "page") : "" }
  }) : []

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
    "versions": versions,
    // What the cursor walks: the client lines, then the Versions' lines, then the notes.
    "lines": clients.concat(versions.lines).concat(notes),
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

// A release note folded (omad-z3ur): its first line that says anything, trimmed.
function noteFirstLine(text) {
  var lines = str(text).split("\n")
  for (var i = 0; i < lines.length; i++) if (lines[i].trim() !== "") return lines[i].trim()
  return ""
}

// Install it, beside a dependency an addon page lists as not installed (omad-z3ur): on
// the cursored client line's client, and only a line where the addon itself is
// installed, never a guessed one. The name is what its TOC requires, and `install`
// resolves it as it resolves any name (the catalog, Tukui, WoWInterface). The view showed
// no release, so its preview always asks. `{text, action, note}`: the button's words
// (naming the client it would act on), the template run, or null and the note that
// says why nothing runs.
function dependencyInstall(line, name) {
  var dep = str(name)
  var ok = !!line && typeof line === "object" && line.installed === true && !line.kind && str(line.target) !== "" && dep !== ""
  if (!ok) return { "text": copy("page.needs.install.any"), "action": null, "note": copy("page.needs.choose") }
  return { "text": copy("page.needs.install", { "client": str(line.client) }),
    "action": { "template": "install", "slots": { "target": line.target, "ref": dep }, "label": "Install " + dep, "shown": [] },
    "note": "" }
}

// An addon page's Versions on one client (1n2p.24): `v` on a client line opens them,
// from the page's own `info --releases` read, asked for only then (GitHub's anonymous
// budget is small). `input` is addonPage's (`versionsFor`, the client they are open on,
// "" when closed; `releasesRead`, the service's `{selection, data, failed}`, whose
// `data` reduceInfo reduced, used only when it is this ref's; `history`); `records` its
// installed records (`{t, a}`); `first` the cursor index of the first line. The lines
// the cursor walks after the client lines:
//   - the undo of the addon's last update on that client, first, while it stands;
//   - one line per channel where it is installed (`update --channel`, previewed): the
//     one it follows marked, each with the newest release listed for it;
//   - each release listed, newest first, with its age, channel and how likely it loads
//     there; the installed one marked, and its pin with why; any other offers
//     `install --version`, which pins it, previewed.
// Each offered line has `request` ({template, slots, label}) for Enter.
function addonVersions(input, name, ref, records, statusTargets, nowMs, first) {
  var target = str(input.versionsFor)
  var none = { "open": false, "target": "", "client": "", "state": "", "failed": "", "why": "", "lines": [] }
  if (target === "" || name === "") return none
  var client = sheetClient(statusTargets, target, "")
  var rec = records.filter(function(r) { return r.t.id === target })[0] || null
  var record = rec ? rec.a : null
  var read = input.releasesRead && typeof input.releasesRead === "object" && input.releasesRead.selection
    && input.releasesRead.selection.ref === ref ? input.releasesRead : null
  var info = read && read.data && typeof read.data === "object" ? read.data : null
  var releases = info && Array.isArray(info.releases) ? info.releases : []
  var failed = read ? str(read.failed) : ""
  var state = failed !== "" ? "failed" : (!info ? "reading" : "")
  var why = ""
  if (state === "" && info && info.releases === null) {
    why = COPY.hasOwnProperty("page.versions.why." + info.releasesWhy) ? copy("page.versions.why." + info.releasesWhy)
      : copy("page.versions.why.other")
  }
  var lines = []
  var push = function(line) {
    line.index = first + lines.length
    line.target = target
    lines.push(line)
  }
  var offer = function(text, glyph) {
    return [{ "name": "enter", "text": text, "glyph": String.fromCodePoint(glyph), "tooltip": withKey(text, "enter", "page") }]
  }
  // A change's label is its `--reason`: one line of at most LABEL_MAX_CHARS.
  var labelOf = function(text) { return text.length > LABEL_MAX_CHARS ? text.slice(0, LABEL_MAX_CHARS - 1) + "…" : text }

  // Its newest change on this client: an update that stands is undone first; a pin's
  // why comes from it (`pin`: the player's; `install` of a fixed version).
  var history = input.history || null
  var run = historyRuns(history, statusTargets).filter(function(r) {
    return r.targets.indexOf(target) !== -1 && r.items.some(function(item) { return item.addon === name })
  })[0] || null
  if (run && run.kind === "update" && run.state === "undoable") {
    var others = []
    var entries = history && Array.isArray(history.entries) ? history.entries : []
    entries.forEach(function(e) {
      if (e.run !== run.run || !e.targets.some(function(t) { return t.id === target })) return
      e.items.forEach(function(item) {
        if (item.addon && item.addon !== name && others.indexOf(item.addon) === -1) others.push(item.addon)
      })
    })
    var undo = historyUndo(run, target)
    push({ "kind": "undo", "key": "u/" + run.key, "title": copy("page.versions.undo", { "when": whenText(run.at, nowMs) }),
      "meta": [run.title].concat(others.length ? [copy("page.versions.undo.others", { "n": others.length })] : []).join(" · "),
      "chips": [], "band": "", "glyph": 0xF054C, "request": undo, "actions": offer(copy("history.action.undo"), 0xF054C) })
  }
  // A pin is the player's (`pin`), or came with a version installed by name (`install
  // --version`, a sync entry's `version`), which History records as the install, update
  // or sync that put it there.
  var pinWhy = ""
  if (record && record.pinned && run) {
    pinWhy = run.kind === "pin" ? copy("page.versions.pinned.by_you")
      : (["install", "update", "sync"].indexOf(run.kind) !== -1 ? copy("page.versions.pinned.fixed") : "")
  }

  // The channels, where it is installed.
  if (record) {
    CHANNELS.forEach(function(channel) {
      var newest = releases.filter(function(r) { return r.channel === channel })[0] || null
      var current = (record.channel || "stable") === channel
      var text = copy("page.versions.channel." + channel)
      push({ "kind": "channel", "key": "ch/" + channel, "channel": channel, "title": text,
        "meta": newest ? copy("page.versions.newest", { "version": newest.version }) : (releases.length ? copy("page.versions.none_listed") : ""),
        "chips": current ? [{ "text": copy("page.versions.following") }] : [], "band": current ? "accent" : "", "glyph": 0xF04E1,
        "request": current ? null : { "template": "update_channel", "slots": { "target": target, "channel": channel, "addon": name },
          "label": labelOf(copy("page.versions.channel.label", { "channel": channel, "addon": name, "client": client })) },
        "actions": current ? [] : offer(text, 0xF04E1) })
    })
  }

  // The releases, newest first.
  releases.forEach(function(r) {
    var here = !!record && record.version === r.version
    var meta = []
    // When it came out, in a player's words (design doc §6.9): "yesterday 21:40", "9 Nov".
    var age = r.publishedAt ? whenText(r.publishedAt, nowMs) : ""
    if (age) meta.push(age)
    if (r.channel !== "stable") meta.push(copy("page.channel", { "channel": r.channel }))
    var verdict = r.compat.hasOwnProperty(target) ? r.compat[target] : ""
    if (COMPAT_WORDS.hasOwnProperty(verdict)) meta.push(copy(COMPAT_WORDS[verdict]))
    var passable = VERSION_PATTERN.test(r.version)
    if (!passable) meta.push(copy("page.versions.unsafe"))
    var chips = []
    if (here) chips.push({ "text": copy("page.versions.installed") })
    if (here && record.pinned) chips.push({ "text": pinWhy || copy("page.versions.pinned") })
    var offered = passable && !here && ref !== ""
    push({ "kind": "release", "key": "r/" + r.version, "title": r.version, "meta": meta.join(" · "), "chips": chips,
      "band": here ? "accent" : "", "glyph": 0xF03D6,
      "request": offered ? { "template": "install_version", "slots": { "target": target, "version": r.version, "ref": ref },
        "label": labelOf(copy("page.versions.install.label", { "addon": name, "version": r.version, "client": client })) } : null,
      "actions": offered ? offer(copy("page.versions.install"), 0xF0415) : [] })
  })
  return { "open": true, "target": target, "client": client, "state": state, "failed": failed, "why": why, "lines": lines }
}

// -------------------------------------------------------------- the Errors view (1n2p.27)

// Who most likely threw an addon's errors, in words (design doc §6.6), from its kind,
// its strongest error's confidence and whether a blocked call named it.
function errorSourceText(a) {
  var errors = Array.isArray(a.errors) ? a.errors : []
  var blocked = errors.some(function(e) { return e.method === "protected_call" })
  if (blocked) return copy("errors.source.blocked", { "addon": a.addon })
  // The game's own taint report named it (omad-8u49); taint spreads, so a likelihood.
  if (a.kind === "addon" && errors.some(function(e) { return e.method === "taint" })) return copy("errors.source.taint")
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
      // The tag of its strongest current link (RG-61): "after update", "after a change".
      if (a.afterChange) chips.unshift({ "text": copy(a.afterChange.tag), "urgent": false })
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
        "stack": e.stack.map(function(f) { return { "text": f, "own": ownFrame(f, entry.addon) } }),
        // When it began, and the change before it (RG-61).
        "firstSeen": e["new"] ? firstSeenText(e.firstSeen) : "",
        "change": changeSection(e.afterChange, nowMs)
      }
    })
  }
}

// When a new error began, in words (RG-61): its basis's or its reason's sentence, with
// the date it names. Empty when the binary says nothing.
function firstSeenText(f) {
  if (!f || !f.words) return ""
  var date = f.basis === "observed" ? (f.onsetByAt || f.at) : f.at
  return copy(f.words, { "date": date ? dateText(date) : copy("sheet.date.unknown") })
}

// The section "What changed before this" of one error (design 5.1, 7.7): one line per
// addon the change moved, the confidence word (always printed: a grade is never a colour
// alone), each weakening fact and unknown, and one button per way back the binary gave.
// A `low` link, or one that no longer stands, is words only: the binary gives it no
// command, and the page adds none. Null with no link.
function changeSection(link, nowMs) {
  if (!link) return null
  var facts = []
  if (link.sameSession === true && !link.exact) facts.push(copy("errors.change.same_session"))
  else if (typeof link.cleanSessions === "number" && link.cleanSessions > 0) facts.push(copy("errors.change.clean", { "n": link.cleanSessions }))
  if (link.clientUpdate) facts.push(copy("errors.change.game_updated", { "from": link.clientUpdate.from, "to": link.clientUpdate.to }))
  facts = facts.concat(link.unknownWords.map(function(id) { return copy(id) }))
  var strong = link.current && (link.confidence === "high" || link.confidence === "medium")
  return {
    "heading": copy("errors.change.heading"),
    "when": copy("errors.change.when", { "kind": link.kind, "when": link.at ? (sinceText(link.at, nowMs) || dateText(link.at)) : "" }),
    "lines": link.changed.map(function(c) {
      return copy("errors.change.moved", { "addon": c.addon, "from": c.fromVersion || "?", "to": c.toVersion || "?" })
    }),
    "word": link.confidenceWords ? copy(link.confidenceWords) : "",
    "facts": facts,
    "state": link.stateWords ? copy(link.stateWords, { "date": link.judgedAt ? dateText(link.judgedAt) : "" }) : "",
    "buttons": strong ? link.commands.filter(function(c) { return !!c.run && c.run.runnable }).map(function(c) {
      return { "action": c.action, "label": copy("errors.change.action." + c.action), "run": c.run }
    }) : []
  }
}

// The page's main action (Enter, RG-61): the first way back of its first error that has
// one, the binary's best first. Null when none.
function errorPageAction(page) {
  var errors = page && Array.isArray(page.errors) ? page.errors : []
  for (var i = 0; i < errors.length; i++) {
    var change = errors[i].change
    if (change && change.buttons.length) return change.buttons[0]
  }
  return null
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
    // A card from `status` alone (the list unread) shows none, so it says no "0 shown".
    lines.push(dotted([copy("backups.count", { "n": t.total }), typeof t.bytes === "number" ? bytesText(t.bytes) : "",
      t.backups.length ? copy("backups.newest", { "when": backupWhen(t.backups[0], nowMs) }) : "",
      t.shown < t.total && !t.unlisted ? copy("backups.shown", { "shown": t.shown, "total": t.total }) : ""]))
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
  // ever made. Before the list answers (or when it never did), a card counts what
  // `status` counts, never "none yet" for a client that has some (omad-4h0d.20).
  var targets = listed.slice()
  for (var s = 0; s < known.length; s++) {
    var st = known[s]
    if (!st.backups || targetById(listed, st.id)) continue
    targets.push({ "id": st.id, "total": backups ? 0 : num(st.backups.count), "shown": 0, "unlisted": !backups,
      "bytes": st.backups.bytes, "freeBytes": st.backups.freeBytes, "auto": st.backups.auto, "backups": [] })
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
