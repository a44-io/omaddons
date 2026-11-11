import QtQuick
import Quickshell
import Quickshell.Hyprland
import Quickshell.Io
import "Model.js" as Model
import "Store.js" as Store
import "Build.js" as Build

// The widget's one point of contact with the CLI (M3-D4, M3-D5, M3-D11, M3-D42).
// Omarchy mounts it once per shell; every bar copy (one per monitor) reads its model
// and calls its functions. It owns every omaddons process, the doorbell watcher, the
// fallback poll and the IPC target, and decides nothing itself: Model.js's pure
// functions choose what runs next, which answers apply and what they mean.
//
// Every omaddons child runs under the stamped context (M3-D7), with argv only from
// Model.buildArgv (templates) or Model.cliArgv (CLI argv the classifier accepted).
// The service writes no file (M3-D3).
Item {
  id: root

  // Injected by Omarchy. Services receive no settings: the bar copies push theirs.
  property var shell: null
  property var manifest: null
  property var settings: ({})

  // ---- the model the views render; each property is reassigned with a fresh object,
  // because QML skips the change signal when a var gets its own reference back.

  // "starting", "ok", "missing" (no binary), "unreachable" (no envelope from status),
  // "incompatible" (another omaddons build: read-only).
  property string phase: "starting"
  // The bar button's model (Model.barState, 1n2p.16), recomputed by refreshBar.
  property var bar: Model.barState(null, { "phase": "starting" })
  // The last status run failed (an envelope with `ok: false`): the bar says omaddons
  // ran into a problem until one answers.
  property bool statusFailed: false
  // The card shown when omaddons cannot be used (Model.phaseCard, 1n2p.34), null when
  // the phase has none; recomputed with the bar.
  property var phaseCard: null
  // The last status read that gave no envelope, `{label, argv, exitCode, outcome,
  // tail}` (Model.failureDetails); null once one answers.
  property var statusFailure: null
  // Why another build answered (Model.incompatibleOf), null otherwise.
  property var incompatible: null
  // When Update the panel last ran (ms since the epoch; 0 for never).
  property double reinstalledAtMs: 0
  // The omaddons version the widget reinstalled itself for (1n2p.35): once per version
  // in this service's life.
  property string selfUpdatedFor: ""
  // The retries while the binary is missing or gave no answer (Model.retryDelayMs).
  property var retryDelaysMs: Model.RETRY_BACKOFF_MS
  property int retryStep: 0
  // `status`'s journal (`{pending, inProgress}`), and the interrupted change it reports
  // (Model.interruptedChange), null for none.
  property var journal: null
  property var interrupted: null
  // The silent checkpoints held back while a change waits to be finished (afterStatus).
  property var heldCheckpoints: []
  // The first run (1n2p.32): whether a bar copy's panel was opened by mouse (the
  // keybinding tip follows it), and the cards put off with Not now, `{kind: [client
  // ids]}`, both for the shell's lifetime (Model.onboardingCard).
  property bool openedByMouse: false
  property var onboardingDismissed: ({})
  property var targets: []
  property var summary: null
  property var lastChange: null
  property var widgetInfo: null
  // The GitHub token's state from `status` (omad-r0to): `{configured, source}`, never the
  // token; null until a status says.
  property var github: null
  // `status`'s recommendations, as buttons where the classifier accepts them.
  property var commands: []
  property var lists: null
  property var errors: null
  property var backups: null
  // History's `history -v` (Model.reduceHistory), read locally when its view opens and
  // after every change (1n2p.30).
  property var history: null
  // omaddons' own settings (`config`, Model.reduceConfig), read when the settings sheet
  // opens and after each change it makes (1n2p.31); the read's failure, or "".
  property var config: null
  property string configFailed: ""
  property var search: null
  // The built-in catalog per client scope ("" for All), reduced as a search's answer
  // (`search --catalog`, local and offline: Search's empty box, 1n2p.26), kept for the
  // service's life; `catalogAsked` the scopes asked for and not failed.
  property var catalogs: ({})
  property var catalogAsked: ({})
  property var changelog: null
  // The open addon page's own `info --changelog` read (1n2p.23), view-bound to the ref it
  // was asked for: `{selection, data, failed}`, `data` Model.reduceInfo's.
  property var addonInfo: null
  // Its Versions' `info --releases` read (1n2p.24), asked for only when `v` opens them:
  // the same shape.
  property var addonReleases: null
  // The running mutation: {name, label, current, total, message}.
  property var action: null
  // The label of the queued user mutation ("next: …").
  property string queuedAction: ""
  // What each lane runs that the player waits on (Model.activityStart, fed by every
  // progress event, 1n2p.18): `{mutation, network, local}`, each null when none.
  property var activities: ({ "mutation": null, "network": null, "local": null })
  // The one the activity bar shows: a change first, then a check or a read.
  readonly property var activity: Model.shownActivity(activities)
  // What waits behind it, in words ("Next: Update Retail"), "" when nothing does.
  property string queuedText: ""
  // The rows a finished change changed, `{<key>: word}`, for rowDoneMs.
  property var doneRows: ({})
  // Whether the last result still shows in the pinned area: resultHoldMs after it, or
  // until the next one (design doc §2.1).
  property bool resultShown: false
  // The last finished action: {ok, line, partial, commands}.
  property var lastResult: null
  // The last failure: {message, code, fixes}.
  property var lastError: null
  // The read whose failure `lastError` is ("" for an action's): a later success of that
  // same read takes the card away.
  property string lastErrorResource: ""
  // The open confirmation: {sheet, heading, stale}, `sheet` the plan sheet Model built
  // (PlanSheet.qml draws it), `heading` what changed since the view, `stale` why the
  // preview ran again.
  property var confirm: null
  // What the last plan preview decided (M4-D28), for state(): `{resource, decision,
  // planId}`, null before the first.
  property var lastPreview: null
  property string incompatibleText: ""
  property string unreachableText: ""
  // The last outcome per resource, for the views and state().
  property var outcomes: ({})
  property string requestedView: ""
  // What the open panel shows, for state(): `{scope, view, stackDepth, sheet, zoomed,
  // textZoom}` (design doc §2.1), set by the panel while it is open; null otherwise.
  property var panelState: null
  // The scope a panel showed last ("" is All), so the next one to open, on any bar,
  // starts there unless exactly one client runs (design doc §2.2). In memory only: it
  // survives closing the panel, not a shell restart.
  property string scope: ""
  // A check for updates is running (the hero's Check now spins, UX-01).
  readonly property bool checking: !!networkLane.request && networkLane.request.resource === "check"
  // The resources being read or queued to be (Model.viewState's `refreshing`).
  readonly property var reading: Model.readingResources([localLane.request, networkLane.request], queues)
  // The last update check, as the Updates view qualifies its rows: `{at, trouble, failed,
  // outcome}` (Model.reduceCheck), null before the first this service ran.
  property var lastCheck: null
  // The view the open panel shows, and when its data was read; a doorbell makes every
  // view's data stale (by resource: list, errors).
  property string currentView: ""
  property double listsAt: 0
  property double errorsAt: 0
  property var staleReads: ({})
  // `status`'s run warnings, with their fixes (the Issues view's third section).
  property var statusWarnings: []
  // `status`'s problems of no client (`data.issues`), with the clients' in Problems.
  property var machineIssues: []
  // The raw details an Issues row asked for, view-bound: `{selection, data}`.
  property var rawErrors: null
  property var rawSelection: null
  // The doctor's explanation an Issues row asked for (`doctor explain`), view-bound to
  // that row: `{selection, lines, failed}`.
  property var explanation: null
  property var explainSelection: null
  // The dismissals `d` still has to send, one client at a time.
  property var pendingDismissals: []
  // An install on several clients (omad-3rre): the previews made so far while they run
  // (`{addon, specs, done}`), then, after Yes, the plans still to apply and what the
  // applies made so far (`{addon, committed}`).
  property var eachInstall: null
  property var pendingInstalls: []
  property var eachOutcome: null
  property double backupsAt: 0
  property double historyAt: 0
  // The restore chooser's `backup list -v -t`, view-bound: `{selection, data}`.
  property var backupsDetail: null
  property var detailSelection: null
  // The spec of the last preview the CLI refused (a restore's `ref_ambiguous` choices
  // rerun it for one folder).
  property var failedPreview: null
  property int viewMaxAgeMs: 10000
  property int contextGeneration: 0
  property bool panelOpen: false
  property double lastRefreshMs: 0
  // What the desktop triggers saw at the last compatible status (F8):
  // `{generation, targets, updates, errorsNew}`, null before the first.
  property var observed: null
  // What the toasts hold back while a game runs, and when each kind last spoke
  // (Model.toastPlan, 1n2p.33).
  property var toastState: Model.emptyToastState()
  property double lastAutoCheckMs: 0
  property int followUpStep: 0
  readonly property bool anyRunning: targets.some(function(t) { return t.running === true })
  readonly property bool canAct: phase === "ok"
  readonly property string binary: {
    var chosen = Model.setting(root.settings, "binary")
    return chosen !== "" ? chosen : Build.BINARY
  }
  readonly property string ipcTarget: ipc.target

  // A request finished (applied or not), with its resource; F8 and the tests listen.
  // Signals carry no envelopes: a `var` argument turns arrays into Qt sequences, which
  // Model.js's Array.isArray rejects. Listeners read the model properties instead.
  signal requestFinished(string resource, string outcome)
  // A compatible status was applied; F8's triggers hang off it.
  signal statusApplied()
  // A setting the service changes by itself (automatic updates turned off by a failure,
  // 1n2p.36): the bar copies save it as they save the panel's.
  signal settingRequested(string key, var value)

  // ---- automatic updates (1n2p.36), Off by default: a background check that ends asks
  // for one (`pending`); it runs once Model.autoUpdateDue allows it, through `stage`
  // "list" (the rows `U` would compare with), "preview" and "plan". A failure holds it
  // off (`held`, and the setting turned Off) until the player turns it on again, and the
  // settings sheet says so (`autoUpdateStopped`).
  property bool autoUpdatePending: false
  property string autoUpdateStage: ""
  property int autoUpdateListAfter: 0
  property bool autoUpdateHeld: false
  property bool autoUpdateStopped: false

  // ---- timing (the tests shrink these)
  property int bellDebounceMs: 150
  property int bellRetryMs: 5000
  property int bellMissingRetryMs: 30000
  property int panelPollMs: 15000
  property int runningPollMs: 30000
  property int windowDebounceMs: 500
  // The open's full scan (1n2p.38): when the last finished, and how old it may grow
  // before an open asks for another (Model.FULL_SCAN_MAX_AGE_MS).
  property double lastFullScanAt: 0
  property int fullScanMaxAgeMs: Model.FULL_SCAN_MAX_AGE_MS
  // An open waits for its cached status, then asks for the rest (afterOpen).
  property bool openFollowUp: false
  // The addresses of the windows that may be WoW's, from their `openwindow`: their
  // `closewindow` names only the address (1n2p.38). The newest WINDOW_MEMORY.
  property var wowWindows: []
  // After a window closed while a client ran: refreshes this long after the close.
  property var followUpDelaysMs: [2000, 5000, 15000]
  property int autoCheckIntervalMs: Model.AUTO_CHECK_INTERVAL_MS
  property int localDeadlineMs: Model.WIDGET_LOCAL_DEADLINE_SEC * 1000
  property int networkDeadlineMs: Model.WIDGET_NETWORK_DEADLINE_SEC * 1000
  property int killGraceMs: Model.WIDGET_KILL_GRACE_SEC * 1000
  property int streamGraceMs: Model.WIDGET_STREAM_GRACE_SEC * 1000
  property int resultHoldMs: Model.RESULT_HOLD_MS
  property int rowDoneMs: Model.ACTIVITY_ROW_DONE_MS
  // How long a copy waits for wl-copy's exit before it says nothing was copied (UX-36).
  property int copyDeadlineMs: 3000
  // How long a paste waits for wl-paste before it says the clipboard could not be read
  // (1n2p.37).
  property int pasteDeadlineMs: Model.WIDGET_PASTE_DEADLINE_MS

  // The template table: Model.COMMANDS; the tests add fixture templates.
  property var templates: Model.COMMANDS

  // ---- scheduler state
  property var queues: Model.emptyQueues()
  property var lastApplied: ({})
  property int lastRequestId: 0
  property var openPanels: ({})
  property var searchSelection: null
  property var changelogSelection: null
  property var addonInfoSelection: null
  property var addonReleasesSelection: null
  // What the open confirmation runs on Yes, and the generation it was previewed in.
  property var pendingConfirm: null
  property bool bellStarted: false

  Component.onCompleted: {
    Store.set(root)
    startBell()
    refresh()
  }
  Component.onDestruction: Store.clear(root)
  onStatusApplied: afterStatus()
  // A new result shows for resultHoldMs, then the pinned area keeps the footer only.
  onLastResultChanged: {
    resultShown = lastResult !== null
    if (resultShown) resultTimer.restart()
    else resultTimer.stop()
  }

  // ------------------------------------------------------------- settings

  // The bar copies push their settings. A new binary is a new context: answers from
  // the old one no longer apply.
  function applySettings(next) {
    var before = root.binary
    var autoBefore = Model.setting(settings, "autoUpdate")
    // A setting renamed since the player chose it keeps its value (1n2p.35).
    settings = Model.migrateSettings(next)
    // Automatic updates a failure turned off: held until the player turns them on again.
    if (autoUpdateHeld && autoBefore === "Off" && Model.setting(settings, "autoUpdate") === "On") {
      autoUpdateHeld = false
      autoUpdateStopped = false
    }
    if (root.binary !== before) {
      contextGeneration++
      autoUpdateStage = ""
      refresh()
    }
    // `refreshIntervalSec` sets when an answer goes stale.
    refreshBar()
  }

  // One bar copy's panel opened or closed; `key` is its module and screen.
  function setPanelOpen(key, open) {
    var next = {}
    for (var k in openPanels) if (k !== key) next[k] = true
    if (open) next[key] = true
    openPanels = next
    var any = Object.keys(next).length > 0
    var opened = any && !panelOpen
    panelOpen = any
    // The open (1n2p.38): the cached status first, in milliseconds; the shown view asks
    // its own read next (PanelView); once the status answers, quietly, what the frame
    // needs that was never read and a full scan when one is due (afterOpen). A view
    // never visited reads nothing.
    if (opened) {
      openFollowUp = true
      request({ "resource": "status", "template": "status", "label": "Open panel" })
    }
  }

  // After an open's cached status: Lua errors (the first-run card asks whether a client
  // records them) and the list (Search and addon pages mark what is installed) when
  // never read, then a full scan when Model.fullScanDue says so, all in the background,
  // behind the shown view's read.
  function afterOpen(commandsNow) {
    openFollowUp = false
    // Never in place of the shown view's own read of the same data (one per key).
    if (!errors && reading.indexOf("errors") === -1) {
      request({ "resource": "errors", "template": "errors", "label": "Issues", "priority": "background" })
    }
    if (!lists && reading.indexOf("list") === -1) {
      request({ "resource": "list", "template": "list", "label": "Installed", "priority": "background" })
    }
    if (Model.fullScanDue(lastFullScanAt, Date.now(), fullScanMaxAgeMs, commandsNow)) {
      request({ "resource": "status", "template": "status_full", "label": "Scan for clients", "priority": "background" })
    }
  }

  // ------------------------------------------------------------- requests

  function request(fields) {
    lastRequestId++
    var r = {
      "id": lastRequestId, "contextGeneration": contextGeneration, "target": "", "resource": "",
      "kind": "data", "priority": "user", "selection": null, "lane": "local",
      "template": "", "slots": {}, "cli": null, "purpose": "read", "label": "",
      "preview": false, "token": "", "confirmed": false, "spec": null, "stale": "",
      "mode": "", "shown": null, "again": false, "plan": null, "continuation": false, "deadlineMs": -1, "inactivity": false
    }
    for (var key in fields) if (fields.hasOwnProperty(key)) r[key] = fields[key]
    var queued = Model.enqueue(queues, r)
    queues = queued.queues
    if (!queued.accepted) console.warn("omaddons: the network lane is full; dropped", r.resource)
    pump()
    return queued.accepted
  }

  function refresh() {
    return request({ "resource": "status", "template": "status", "label": "Refresh" })
  }

  // A missing client's Rescan clients (design doc §2.2): `status` with its scan, not
  // the cached clients, so a client installed again comes back.
  function rescan() {
    return request({ "resource": "status", "template": "status_full", "label": "Rescan clients" })
  }

  // Whether the check was taken: false when the network lane's queue is full (UX-42).
  function check() {
    return request({ "resource": "check", "template": "check", "lane": "network", "label": "Check now" })
  }

  // The activity bar's Cancel: the read it shows stops (SIGTERM, then SIGKILL after the
  // grace); a change is never stopped, since it ends as a transaction (1n2p.18).
  // Whether a read was stopped.
  function cancelActivity() {
    var a = activity
    if (!a || a.cancellable !== true || a.lane === "mutation") return false
    var lane = laneNamed(a.lane)
    if (!lane.request || lane.request.id !== a.id) return false
    lane.cancel()
    return true
  }

  // A failure card's Dismiss: it stays until then, or until what failed succeeds.
  function dismissError() {
    lastError = null
    lastErrorResource = ""
  }

  function readList() {
    return request({ "resource": "list", "template": "list", "label": "Installed" })
  }

  function readErrors() {
    return request({ "resource": "errors", "template": "errors", "label": "Issues" })
  }

  // An Issues row's Raw details button: one addon's raw details (`--raw-details`),
  // view-bound to that row.
  function readRawErrors(target, addon) {
    var selection = { "target": String(target || ""), "addon": String(addon || "") }
    rawSelection = selection
    return request({
      "resource": "errors_raw", "template": "errors_raw", "kind": "view", "target": selection.target,
      "selection": selection, "slots": { "target": selection.target, "addon": selection.addon }, "label": "Raw details"
    })
  }

  // The raw details expander closed: a late answer is dropped.
  function clearRawErrors() {
    rawSelection = null
    rawErrors = null
  }

  // An Issues row's explanation: the row's own explain argv from `status`, read only when
  // Model.explainArgv accepts it, view-bound to the row's key. A read: it runs while a
  // change runs, on the local lane. Whether it was asked.
  function readExplanation(key, argv) {
    var cli = Model.explainArgv(argv)
    if (!cli) return false
    var selection = { "key": String(key || "") }
    explainSelection = selection
    return request({
      "resource": "explain", "cli": cli, "kind": "view", "lane": "local", "selection": selection, "label": "Explain"
    })
  }

  // The explanation closed: a late answer is dropped.
  function clearExplanation() {
    explainSelection = null
    explanation = null
  }

  // Puts a command the widget shows instead of running (a purge, a forced repair), or an
  // error report, on the clipboard: wl-copy with the text as one argument
  // (Model.copyArgv), never a shell, in a process of its own on no lane, so a copy never
  // waits behind a read and never holds one up. `done(ok)`, when given, hears whether
  // the text went on the clipboard: wl-copy's own exit code 0, never a probe (UX-36). A
  // missing wl-copy, any other exit, or none within copyDeadlineMs is false. Whether
  // there was anything to copy.
  function copyCommand(text, done) {
    if (!Model.copyArgv(text)) return false
    var callback = typeof done === "function" ? done : null
    var copier = copyComponent.createObject(root, { "text": text, "done": callback })
    if (copier) copier.start()
    else if (callback) callback(false)
    return true
  }

  // ---- sharing an addon set (1n2p.37)

  // Copy addon list: the chosen client's set (`filter`), or every client's, read with
  // `export` on the local lane (it writes nothing) and put on the clipboard as the TOML
  // it printed (copyCommand). `done(ok, line)`, when given, hears what to flash: what was
  // copied, once wl-copy took it, or why nothing was (Model.exportedList). Whether the
  // read was asked.
  function copyAddonList(filter, done) {
    var asked = Model.exportRequest(filter || "", targets)
    return request({ "resource": "export", "template": asked.template, "slots": asked.slots, "label": asked.label,
      "lane": "local", "purpose": "copy", "target": filter || "", "done": typeof done === "function" ? done : null })
  }

  // The export a copy read: its text to the clipboard, or why there is none. A failure
  // is the copy's to say beside its button, never a failure card.
  function copiedList(r, outcome, env) {
    var done = typeof r.done === "function" ? r.done : null
    var answer = function(ok, line) {
      if (!done) return
      // The view that asked may have gone before the answer came.
      try {
        done(ok, line)
      } catch (e) {
        console.warn("omaddons: a copy's answer reached no view:", e)
      }
    }
    var list = Model.exportedList(outcome === "ok" ? env : null, String(r.target || ""), targets)
    if (!list.ok) {
      answer(false, list.line)
      return
    }
    if (!copyCommand(list.text, function(ok) { answer(ok, Model.copyFlash(ok, list.line)) })) answer(false, Model.copy("share.copy.failed"))
  }

  // Paste addon list: the clipboard read (pasteComponent: wl-paste, on no lane), checked
  // (Model.pastedList), then previewed as `sync -` on the chosen client (`filter`) or
  // every client, the text on its standard input: the plan sheet says what it would
  // change before anything does, and its Yes applies the stored plan. `done(refusal)`
  // hears "" once the preview is asked, or why nothing ran, in the player's words.
  // Whether the clipboard is being read.
  function pasteAddonList(filter, done) {
    var answer = typeof done === "function" ? done : function(refusal) {}
    if (!canAct) {
      answer(incompatibleText || "omaddons is not ready")
      return false
    }
    var scope = String(filter || "")
    var reader = pasteComponent.createObject(root, { "done": function(ok, text) {
      if (!ok) {
        answer(Model.pasteRefusal("unreadable"))
        return
      }
      var list = Model.pastedList(text)
      if (!list.ok) {
        answer(Model.pasteRefusal(list.reason))
        return
      }
      var asked = Model.syncRequest(scope, targets)
      answer(run(asked.template, asked.slots, asked.label, [], list.text))
    } })
    if (!reader) {
      answer(Model.pasteRefusal("unreadable"))
      return false
    }
    reader.start()
    return true
  }

  function readBackups() {
    return request({ "resource": "backups", "template": "backups", "label": "Backups" })
  }

  // History's changes (1n2p.30): local, never the network.
  function readHistory() {
    return request({ "resource": "history", "template": "history", "label": "History" })
  }

  // omaddons' own settings, for the settings sheet (1n2p.31): local, never the network.
  function readConfig() {
    return request({ "resource": "config", "template": "config", "label": "Settings" })
  }

  // The restore chooser opened for `target`: each backup's addon versions, view-bound.
  function readBackupsDetail(target) {
    var selection = { "target": String(target || "") }
    detailSelection = selection
    return request({
      "resource": "backups_detail", "template": "backups_detail", "kind": "view", "target": selection.target,
      "selection": selection, "slots": { "target": selection.target }, "label": "Backups"
    })
  }

  // The chooser closed: a late answer is dropped.
  function clearBackupsDetail() {
    detailSelection = null
    backupsDetail = null
  }

  // A `ref_ambiguous` choice: the refused restore again, for one folder (`folder:X`),
  // through the preview flow.
  function restoreFolder(addon) {
    var spec = failedPreview
    if (!spec || !spec.slots || !spec.slots.backup) return "nothing to restore"
    return run("restore_addon", { "target": spec.slots.target, "addon": String(addon || ""), "backup": spec.slots.backup },
      "Restore " + addon)
  }

  // The panel switched to `view`: read its data when it is older than viewMaxAgeMs or
  // a doorbell rang since. Local reads only: opening and switching never touch the
  // network.
  function readView(view) {
    currentView = String(view || "")
    // Issues reads nothing of its own: `status`, which the frame keeps current, is its data.
    var resource = currentView === "updates" || currentView === "installed" ? "list"
      : (currentView === "errors" ? "errors" : (currentView === "backups" ? "backups"
      : (currentView === "history" ? "history" : "")))
    if (resource === "") return false
    var have = resource === "list" ? lists : (resource === "errors" ? errors : (resource === "backups" ? backups : history))
    var at = resource === "list" ? listsAt : (resource === "errors" ? errorsAt : (resource === "backups" ? backupsAt : historyAt))
    if (have && staleReads[resource] !== true && Date.now() - at <= viewMaxAgeMs) return false
    var next = {}
    for (var key in staleReads) if (key !== resource) next[key] = staleReads[key]
    staleReads = next
    return resource === "list" ? readList() : (resource === "errors" ? readErrors()
      : (resource === "backups" ? readBackups() : readHistory()))
  }

  // A view's Try again (Model.viewState's actions): its data read again now, whatever
  // its age.
  function readAgain(view) {
    var resource = Model.VIEW_RESOURCES[String(view || "")]
    if (!resource || resource === "search") return false
    var next = {}
    for (var key in staleReads) next[key] = staleReads[key]
    next[resource] = true
    staleReads = next
    return readView(view)
  }

  // Something changed (the doorbell, or the widget's own action): the bar first, then
  // the open panel's view.
  function changed() {
    refresh()
    staleReads = { "list": true, "errors": true, "backups": true, "history": true }
    if (panelOpen && currentView !== "") readView(currentView)
  }

  // A search, view-bound to its query and target filter; All ("") searches every client
  // in one request. A newer query replaces a queued search (Model.enqueue) and stops one
  // still running for another query or client (1n2p.25): its answer would be dropped as
  // late, and the newer one would wait behind it.
  function searchFor(query, target) {
    var selection = { "query": String(query || ""), "target": String(target || "") }
    searchSelection = selection
    var shape = Model.searchRequest(selection.query, selection.target)
    var taken = request({
      "resource": "search", "template": shape.template, "lane": "network", "kind": "view", "target": selection.target,
      "selection": selection, "slots": shape.slots, "label": "Search"
    })
    var running = networkLane.request
    if (taken && running && running.resource === "search" && running.selection
        && (running.selection.query !== selection.query || running.selection.target !== selection.target)) {
      networkLane.cancel()
    }
    return taken
  }

  // The catalog for `target` ("" for All), once: a background read on the local lane,
  // whose failure stays out of the panel (the view says what to type instead) and is
  // asked again the next time. Whether a read was asked for now.
  function readCatalog(target) {
    var t = String(target || "")
    if (catalogs.hasOwnProperty(t) || catalogAsked.hasOwnProperty(t)) return false
    var asked = {}
    for (var key in catalogAsked) asked[key] = catalogAsked[key]
    asked[t] = true
    catalogAsked = asked
    var shape = Model.catalogRequest(t)
    return request({ "resource": "catalog", "template": shape.template, "lane": "local", "target": t,
      "priority": "background", "slots": shape.slots, "label": "Catalog" })
  }

  // `d`: one bound dismissal per client in `plan` (`[{target, token}]`, the tokens of the
  // errors read the panel showed), one after another: the mutation lane keeps a single
  // queued user change. The first that does not succeed stops the rest.
  function dismissErrors(plan) {
    if (!canAct) return incompatibleText || "omaddons is not ready"
    if (!Array.isArray(plan) || plan.length === 0) return "no new errors to dismiss"
    pendingDismissals = plan.slice(1)
    return dismissOne(plan[0])
  }

  function dismissOne(entry) {
    return runBound("dismiss_errors", { "target": entry.target }, "Dismiss errors on " + entry.target, entry.token)
  }

  // Search's "Every client it works on" (omad-3rre): `ref` installed on each client of
  // `targetIds`. `install` lands on one client per command, so each client's preview runs
  // in turn and one sheet shows every plan; Yes applies them in turn, each bound to its
  // own preview (`apply <plan_id>`), the first that does not commit stopping the rest.
  function installEach(ref, addon, targetIds) {
    if (!canAct) return incompatibleText || "omaddons is not ready"
    var ids = Array.isArray(targetIds) ? targetIds : []
    if (ids.length < 2) return "choose two clients or more"
    var specs = []
    for (var i = 0; i < ids.length; i++) {
      var spec = { "template": "install", "slots": { "target": String(ids[i]), "ref": String(ref || "") },
        "label": "Install " + String(addon || ref), "priority": "user", "mode": "match", "shown": [], "each": true }
      var refusal = trialBuild(spec)
      if (refusal) return refusal
      specs.push(spec)
    }
    eachInstall = { "addon": String(addon || ref), "specs": specs, "done": [] }
    previewOf(specs[0], "", false)
    return ""
  }

  // One client's preview of an install on several: kept with its plan and sheet (or why
  // it plans nothing there), then the next client's; after the last, the one sheet. A
  // preview that fails stops the rest, and nothing runs.
  function previewedEach(r, outcome, env) {
    var each = eachInstall
    var client = Model.sheetClient(targets, r.spec.slots.target, "")
    if (outcome !== "ok") {
      eachInstall = null
      failedPreview = r.spec
      lastError = errorOf(env, r.label + " on " + client + ": " + outcome)
      return
    }
    var decision = Model.previewDecision({ "mode": "match", "shown": [], "again": false, "path": "install",
      "context": Build.CONTEXT, "targets": targets }, env)
    // Each client's plan is exactly its own preview's (M4-D28): applied later, in turn.
    var entry = { "spec": r.spec, "client": client, "plan": decision.plan, "sheet": decision.sheet || null,
      "why": decision.plan && decision.sheet ? "" : String(decision.line || "") }
    var done = each.done.concat([entry])
    if (done.length < each.specs.length) {
      eachInstall = { "addon": each.addon, "specs": each.specs, "done": done }
      previewOf(each.specs[done.length], "", false)
      return
    }
    eachInstall = null
    var withPlans = done.filter(function(e) { return !!e.plan && !!e.sheet })
    var notes = done.filter(function(e) { return !e.plan || !e.sheet })
      .map(function(e) { return Model.copy("each.skipped", { "client": e.client, "why": e.why }) })
    if (withPlans.length === 0) {
      lastResult = { "ok": true, "line": notes.join("; "), "partial": false, "commands": [], "fixes": [] }
      changed()
      return
    }
    confirm = { "sheet": Model.eachSheet(each.addon, withPlans.map(function(e) { return e.sheet }), notes), "heading": "", "stale": "" }
    pendingConfirm = { "each": withPlans, "addon": each.addon, "contextGeneration": r.contextGeneration }
  }

  // One client's apply of an install on several: the next client's plan when it
  // committed, else the end: the result names the clients installed on, the one that
  // failed (a plan gone stale there is previewed again, that client alone) and the ones
  // never reached.
  function appliedEach(r, outcome, env) {
    var o = eachOutcome
    var client = Model.sheetClient(targets, r.spec.slots.target, "")
    var stale = !!env && Model.needsPreviewAgain(env)
    var ok = outcome === "ok" && !stale
    var committed = ok ? o.committed.concat([client]) : o.committed
    var failure = ok ? null : { "client": client, "why": stale ? Model.copy("each.stale") : (lastError ? lastError.message : outcome) }
    if (ok && pendingInstalls.length > 0) {
      var next = pendingInstalls[0]
      pendingInstalls = pendingInstalls.slice(1)
      eachOutcome = { "addon": o.addon, "committed": committed }
      runPlan(next.plan, next.spec, r.contextGeneration)
      return
    }
    var notRun = pendingInstalls.map(function(p) { return p.client })
    pendingInstalls = []
    eachOutcome = null
    lastResult = { "ok": failure === null, "line": Model.eachInstallLine(o.addon, committed, failure, notRun),
      "partial": committed.length > 0 && failure !== null, "commands": [], "fixes": [] }
  }

  // An addon page opened on `ref` (its ref, else its name): what its source says of it,
  // its records and release notes, on the network lane, view-bound to that ref, so a
  // late answer for another page never lands. Read once per page open.
  function readAddonInfo(ref) {
    var selection = { "ref": String(ref || "") }
    if (selection.ref === "") return false
    addonInfoSelection = selection
    addonInfo = { "selection": selection, "data": null, "failed": "" }
    return request({
      "resource": "addon_info", "template": "addon_info", "lane": "network", "kind": "view",
      "selection": selection, "slots": { "ref": selection.ref }, "label": "About " + selection.ref
    })
  }

  // The page closed: a late answer is dropped.
  function clearAddonInfo() {
    addonInfoSelection = null
    addonInfo = null
    addonReleasesSelection = null
    addonReleases = null
  }

  // An addon page's Versions (1n2p.24): its source's recent releases, with how likely
  // each loads on each client; asked once per ref while the page stays open.
  function readAddonReleases(ref) {
    var selection = { "ref": String(ref || "") }
    if (selection.ref === "") return false
    if (addonReleases && addonReleases.selection.ref === selection.ref && !addonReleases.failed) return true
    addonReleasesSelection = selection
    addonReleases = { "selection": selection, "data": null, "failed": "" }
    return request({
      "resource": "addon_releases", "template": "addon_releases", "lane": "network", "kind": "view",
      "selection": selection, "slots": { "ref": selection.ref }, "label": "Releases of " + selection.ref
    })
  }

  function readChangelog(target, addon) {
    var selection = { "target": String(target || ""), "addon": String(addon || "") }
    changelogSelection = selection
    return request({
      "resource": "changelog", "template": "changelog", "lane": "network", "kind": "view", "target": selection.target,
      "selection": selection, "slots": { "target": selection.target, "addon": selection.addon }, "label": "Changelog"
    })
  }

  function mutation(fields) {
    var base = { "kind": "mutation", "lane": "mutation", "purpose": "action" }
    for (var key in fields) if (fields.hasOwnProperty(key)) base[key] = fields[key]
    if (!base.target && base.slots && base.slots.target) base.target = base.slots.target
    return request(base)
  }

  // ---- the previewed-action runner (M3-D9, M4-D28): previewOf runs an action's dry run,
  // applyPreview carries out what Model.previewDecision makes of it, runPlan applies the
  // plan it offered, and applyAction reads that `apply` and previews again when it was
  // refused. `again` marks a preview after a refused plan: it goes first, and is never
  // applied without the dialog.
  function previewOf(spec, stale, again) {
    var fields = {
      "resource": spec.template || "cli", "template": spec.template || "", "slots": spec.slots || {},
      "cli": spec.cli || null, "label": spec.label, "priority": spec.priority || "user",
      "preview": true, "purpose": "preview", "spec": spec, "stale": stale || "",
      "mode": spec.mode || "", "shown": spec.shown || null, "again": again === true, "continuation": again === true
    }
    // A pasted list goes with its preview, and again with a preview made after its plan
    // was refused; never with the plan's apply, which holds the text (1n2p.37).
    if (typeof spec.stdin === "string") fields.stdin = spec.stdin
    // A match preview may ask the sources: the network deadline, kept alive by progress.
    // A confirm preview and every apply run under the mutation lane's (none).
    if (spec.mode === "match") {
      fields.deadlineMs = networkDeadlineMs
      fields.inactivity = true
    }
    return mutation(fields)
  }

  // The plan a preview offered, applied: first on the mutation lane, replacing nothing,
  // in the generation it was previewed in.
  function runPlan(plan, spec, generation) {
    return mutation({
      "resource": spec.template || "cli", "cli": plan.argv.slice(), "plan": plan, "label": spec.label,
      "priority": spec.priority || "user", "purpose": "planned", "spec": spec, "continuation": true,
      "contextGeneration": generation
    })
  }

  // A preview's answer. On a plan-storing path, Model.previewDecision says what it means:
  // apply the plan, ask in the dialog, refresh, or run nothing and say why. Elsewhere
  // (`targets rm`), M3's dialog: Yes runs the command bound to its token, or confirmed.
  function applyPreview(r, outcome, env) {
    confirm = null
    pendingConfirm = null
    // One client's preview of an install on several (omad-3rre).
    if (r.spec && r.spec.each === true && eachInstall) {
      previewedEach(r, outcome, env)
      return
    }
    // A bound repair (M4-D44): the doctor's envelope has no `data` (M4-D34) and a dry run
    // with something to repair exits 1, so Model.doctorPreviewDecision reads it before
    // anything generic does. Yes runs the same argv bound to the preview's token.
    if ((outcome === "ok" || outcome === "failed") && r.spec && r.spec.mode === "bind") {
      var repair = Model.doctorPreviewDecision(r.spec, env, Build.CONTEXT, targets)
      lastPreview = { "resource": r.resource, "decision": repair.kind, "planId": null }
      if (repair.kind === "dialog") {
        confirm = { "sheet": Model.withStale(repair.sheet, r.stale), "heading": "", "stale": r.stale }
        pendingConfirm = { "spec": r.spec, "token": repair.token, "binds": true, "plan": null, "contextGeneration": r.contextGeneration }
      } else if (repair.kind === "refresh") {
        lastResult = { "ok": true, "line": repair.line, "partial": false, "commands": [], "fixes": [] }
        changed()
      } else if (repair.kind === "refuse") {
        lastError = { "message": repair.line, "code": "not_previewed", "fixes": [], "fixText": repair.fixText || "" }
        changed()
      } else {
        failedPreview = r.spec
        lastError = errorOf(env, r.label + ": " + (outcome === "ok" ? "no repair report" : outcome))
      }
      return
    }
    if (outcome !== "ok") {
      failedPreview = r.spec
      lastError = errorOf(env, r.label + ": " + outcome)
      // A pasted list the CLI refused (1n2p.37): where its mistake is, in the panel's words.
      var pasted = typeof r.stdin === "string" ? Model.pasteFailure(env) : null
      if (pasted) lastError = { "message": pasted, "code": "desired_state_invalid", "fixes": [], "fixText": "", "choices": [], "folder": "" }
      return
    }
    var path = r.cli ? Model.classifyCliArgv(r.cli).path : Model.templateShape(r.template, templates).path
    if (!Model.storesPlan(path)) {
      var token = env.data && typeof env.data.precondition === "string" ? env.data.precondition : ""
      confirm = { "sheet": Model.withStale(Model.confirmSheet(r.template || path, env, Build.CONTEXT, targets), r.stale),
        "heading": "", "stale": r.stale }
      pendingConfirm = { "spec": r.spec, "token": token, "binds": Model.CLI_PATHS[path].binds, "plan": null, "contextGeneration": r.contextGeneration }
      return
    }
    // The context and clients shorten the paths a repair's undo lists (`<config>/…`).
    var decision = Model.previewDecision({ "mode": r.mode, "shown": r.shown, "again": r.again === true, "path": path,
      "context": Build.CONTEXT, "targets": targets, "pins": r.template === "install_version" }, env)
    lastPreview = { "resource": r.resource, "decision": decision.kind, "planId": decision.plan ? decision.plan.planId : null }
    if (decision.kind === "direct") {
      runPlan(decision.plan, r.spec, r.contextGeneration)
    } else if (decision.kind === "dialog") {
      confirm = { "sheet": Model.withStale(decision.sheet, r.stale), "heading": decision.heading, "stale": r.stale }
      pendingConfirm = { "spec": r.spec, "plan": decision.plan, "contextGeneration": r.contextGeneration }
    } else if (decision.kind === "refresh") {
      lastResult = { "ok": true, "line": decision.line, "partial": false, "commands": [], "fixes": [] }
      changed()
    } else {
      lastError = { "message": decision.line || r.label + ": no answer to preview", "code": "not_previewed", "fixes": [], "fixText": decision.fixText || "" }
      changed()
    }
  }

  // A mutating template by name: a `preview` template (and any destructive one) runs its
  // dry run first (M3-D9, M4-D28), silent ones run in the background. `shown` is what
  // the view showed of the change, for a `match` preview. `input` is a pasted addon list
  // for a template that takes one on standard input (1n2p.37); buildArgv checks it again.
  // Returns "" or why it was refused.
  function run(name, slots, label, shown, input) {
    if (!canAct) return incompatibleText || "omaddons is not ready"
    var template = templates.hasOwnProperty(name) ? templates[name] : null
    if (!template || template.mutates !== true) return "no such action: " + name
    if (template.bind === true) return name + " runs with its view's token (runBound)"
    var spec = { "template": name, "slots": slots || {}, "label": String(label || name), "priority": template.silent === true ? "background" : "user" }
    if (typeof input === "string") spec.stdin = input
    var refusal = trialBuild(spec)
    if (refusal) return refusal
    var path = Model.templateShape(name, templates).path
    if (template.preview || Model.CLI_PATHS[path].destructive) {
      // `bind` (a repair): Yes runs the same argv bound to the preview's token.
      spec.mode = template.preview === "match" ? "match" : (template.preview === "bind" ? "bind" : "confirm")
      spec.shown = Array.isArray(shown) ? shown : []
      previewOf(spec, "", false)
    } else {
      mutation({ "resource": name, "template": name, "slots": spec.slots, "label": spec.label, "priority": spec.priority })
    }
    return ""
  }

  // A `bind` template (`dismiss_errors`): no dialog, the token of the view it acts on.
  function runBound(name, slots, label, token) {
    if (!canAct) return incompatibleText || "omaddons is not ready"
    var template = templates.hasOwnProperty(name) ? templates[name] : null
    if (!template || template.bind !== true) return "no such action: " + name
    var spec = { "template": name, "slots": slots || {}, "label": String(label || name), "token": String(token || "") }
    var refusal = trialBuild(spec)
    if (refusal) return refusal
    mutation({ "resource": name, "template": name, "slots": spec.slots, "label": spec.label, "token": spec.token, "purpose": "bind", "spec": spec })
    return ""
  }

  // An argv from CLI output (a fix, a recommendation, an undo): runnable only when the
  // classifier accepts it; destructive ones preview first.
  function runCliArgv(argv, label) {
    if (!canAct) return incompatibleText || "omaddons is not ready"
    var verdict = Model.classifyCliArgv(argv)
    if (!verdict.ok) return "not a command the widget runs (" + verdict.reason + ")"
    var spec = { "cli": argv.slice(), "label": String(label || verdict.path), "priority": "user" }
    // A destructive command, an install or an update the CLI offered (Install BugGrabber,
    // a failure's fix, 1n2p.32): its dry run first, then the plan sheet, as README
    // promises for every install and update; the view never showed what it would bring.
    if (verdict.destructive || verdict.path === "install" || verdict.path === "update") {
      spec.mode = "confirm"
      spec.shown = []
      previewOf(spec, "", false)
    } else if (verdict.mutates) {
      mutation({ "resource": "cli:" + verdict.path, "cli": spec.cli, "label": spec.label })
    } else {
      // A read that asks the sources runs on the network lane, as its templates do:
      // its deadline then follows its progress, and status refreshes never wait on it.
      var network = verdict.path === "outdated" || verdict.path === "search"
        || (verdict.path === "info" && argv.indexOf("--changelog") !== -1)
      request({ "resource": "cli:" + verdict.path, "cli": spec.cli, "label": spec.label, "purpose": "action",
        "lane": network ? "network" : "local" })
    }
    return ""
  }

  // Yes on the open confirmation, in the generation the preview was made in: the plan
  // the preview offered, applied; else (no plan-storing path) the real run, bound to its
  // preview's token when the path binds.
  function confirmYes() {
    if (!confirm || !pendingConfirm) return "nothing to confirm"
    if (!canAct) return incompatibleText || "omaddons is not ready"
    var pending = pendingConfirm
    confirm = null
    pendingConfirm = null
    // An install on several clients: the first client's plan; the rest follow it.
    if (pending.each) {
      eachOutcome = { "addon": pending.addon, "committed": [] }
      pendingInstalls = pending.each.slice(1)
      runPlan(pending.each[0].plan, pending.each[0].spec, pending.contextGeneration)
      return ""
    }
    var spec = pending.spec
    if (pending.plan) {
      runPlan(pending.plan, spec, pending.contextGeneration)
      return ""
    }
    mutation({
      "resource": spec.template || "cli", "template": spec.template || "", "slots": spec.slots || {},
      "cli": spec.cli || null, "label": spec.label, "purpose": "confirmed", "spec": spec,
      "token": pending.binds ? pending.token : "", "confirmed": !pending.binds,
      "contextGeneration": pending.contextGeneration
    })
    return ""
  }

  function confirmNo() {
    confirm = null
    pendingConfirm = null
  }

  function trialBuild(spec) {
    try {
      argvOf({ "template": spec.template || "", "slots": spec.slots || {}, "cli": spec.cli || null, "label": spec.label,
        "preview": true, "token": "", "confirmed": false, "stdin": typeof spec.stdin === "string" ? spec.stdin : null })
    } catch (e) {
      return String(e.message || e)
    }
    return ""
  }

  function argvOf(r) {
    var opts = {
      "binary": root.binary, "label": r.label || r.template || "omaddons", "context": Build.CONTEXT,
      "templates": root.templates, "preview": r.preview === true, "token": r.token || "", "confirmed": r.confirmed === true,
      "plan": r.plan || null, "stdin": typeof r.stdin === "string" ? r.stdin : null
    }
    return r.cli ? Model.cliArgv(r.cli, opts) : Model.buildArgv(r.template, r.slots, opts)
  }

  // ------------------------------------------------------------- the lanes

  function laneNamed(name) {
    return name === "mutation" ? mutationLane : (name === "network" ? networkLane : localLane)
  }

  // Starts what the free lanes may run next. A mutation is rechecked just before it
  // starts (M3-D42): a compatible binary, and the generation it was asked in.
  function pump() {
    var busy = { "mutation": mutationLane.busy, "local": localLane.busy, "network": networkLane.busy }
    var next = Model.nextRequest(queues, busy)
    queues = next.queues
    var again = false
    for (var name in next.start) {
      var r = next.start[name]
      if (!r) continue
      if (r.kind === "mutation" && (phase === "incompatible" || r.contextGeneration !== contextGeneration)) {
        lastError = { "message": incompatibleText || "omaddons changed since this was asked; nothing ran", "code": "incompatible", "fixes": [] }
        again = true
        continue
      }
      var built = null
      try {
        built = argvOf(r)
      } catch (e) {
        lastError = { "message": String(e.message || e), "code": "refused", "fixes": [] }
        again = true
        continue
      }
      // A silent change (M3-D11) shows no progress and leaves the panel's changes on: one
      // asked meanwhile queues behind it ("next: …").
      if (r.kind === "mutation" && r.priority !== "background") startAction(r)
      setActivity(name, Model.activityStart(r, Date.now(), lists))
      laneNamed(name).run(r, built.argv, built.env, built.stdin)
    }
    queuedAction = queuedLabel()
    queuedText = Model.queuedText(queues)
    refreshBar()
    if (again) Qt.callLater(pump)
  }

  // Lane `name`'s activity (null for none), as a fresh object so the change signal fires.
  function setActivity(name, value) {
    var next = { "mutation": activities.mutation, "network": activities.network, "local": activities.local }
    next[name] = value
    activities = next
  }

  // The activity of request `r` on its lane, or null when the lane shows another's.
  function activityOf(r) {
    var a = activities[Model.laneOf(r)]
    return a && a.id === r.id ? a : null
  }

  // The bar button's model (Model.barState) from what the service knows now: the last
  // status, the phase, whether a change the player asked for runs or a check runs or
  // waits (Model.barWorking, so a right click spins at once), and how old the answer
  // is. Replaced only when it changes, so the bar copies redraw only then.
  // The phase card (Model.phaseCard) follows the same inputs, so it is refreshed here too.
  function refreshBar() {
    var now = Date.now()
    var status = { "failed": statusFailed, "summary": summary, "targets": targets, "issues": machineIssues }
    var next = Model.barState(status, { "phase": phase, "working": Model.barWorking(activities, queues),
      "nowMs": now, "answeredAtMs": lastRefreshMs, "staleAfterMs": Model.barStaleAfterMs(root.settings) })
    if (!Model.jsonEqual(bar, next)) bar = next
    var card = Model.phaseCard(phase, { "binary": root.binary, "home": Build.CONTEXT.HOME,
      "outcome": statusFailure ? statusFailure.outcome : "", "failure": statusFailure,
      "deadlineSec": Math.max(1, Math.round(localDeadlineMs / 1000)), "mine": Build.TOOL_VERSION, "incompatible": incompatible,
      "reinstalledAtMs": reinstalledAtMs, "nowMs": now })
    if (!Model.jsonEqual(phaseCard, card)) phaseCard = card
  }

  // A request that ended with no envelope, as the failure card shows it: its words
  // (Model.failureWords) and the details a bug report needs (Model.failureDetails).
  function failureOf(r, outcome, tail) {
    return { "message": Model.failureWords(r.label, outcome), "code": outcome, "fixes": [], "fixText": "", "choices": [],
      "folder": "", "details": Model.failureDetails(failureFacts(r, outcome, tail), Build.CONTEXT, Build.TOOL_VERSION) }
  }

  function failureFacts(r, outcome, tail) {
    return { "label": r.label, "argv": Model.listOf(r.argv), "exitCode": typeof r.exitCode === "number" ? r.exitCode : -1,
      "outcome": outcome, "tail": Model.listOf(tail) }
  }

  // Asks `status` again after a missing or silent binary, sooner first (5 s, 15 s, then
  // every 60 s), with the panel open or closed; an answer stops it (applyStatus).
  function scheduleRetry() {
    if (retryTimer.running) return
    retryTimer.interval = Model.retryDelayMs(retryStep, retryDelaysMs)
    retryStep++
    retryTimer.restart()
  }

  function queuedLabel() {
    var list = queues.mutation
    for (var i = 0; i < list.length; i++) if (list[i].priority === "user") return list[i].label
    return ""
  }

  function startAction(r) {
    action = { "name": r.template || r.resource, "label": r.label, "current": 0, "total": 0,
      "message": !r.preview ? "" : r.mode === "match" ? "Preparing…"
        : r.mode === "bind" ? "Checking what it would repair…" : "Checking what it would do…" }
  }

  // A progress event of any lane's request: its activity follows it (reads too, UX-34);
  // a change's `action` keeps its counts for state().
  function laneProgress(r, event) {
    var a = activityOf(r)
    if (a) setActivity(Model.laneOf(r), Model.activityProgress(a, event, Date.now()))
    if (r.kind !== "mutation" || !action) return
    action = { "name": action.name, "label": action.label,
      "current": typeof event.current === "number" ? event.current : 0,
      "total": typeof event.total === "number" ? event.total : 0, "message": Model.progressLine(event) }
  }

  function selectionFor(r) {
    if (r.resource === "search") return searchSelection
    if (r.resource === "changelog") return changelogSelection
    if (r.resource === "addon_info") return addonInfoSelection
    if (r.resource === "addon_releases") return addonReleasesSelection
    if (r.resource === "errors_raw") return rawSelection
    if (r.resource === "backups_detail") return detailSelection
    if (r.resource === "explain") return explainSelection
    return null
  }

  // `rows` (`{<key>: word}`) say what happened to them for rowDoneMs; none leaves the
  // last ones as they are.
  function markDone(rows) {
    if (!rows || Object.keys(rows).length === 0) return
    doneRows = rows
    doneTimer.restart()
  }

  function noteOutcome(r, outcome) {
    var next = {}
    for (var key in outcomes) next[key] = outcomes[key]
    next[r.resource] = outcome
    outcomes = next
  }

  function errorOf(env, fallback) {
    if (env && env.error) {
      return { "message": String(env.error.message || fallback), "code": String(env.error.code || ""),
        "fixes": Model.fixButtonsBesideSearch(env), "fixText": Model.searchFixOf(env) !== "" ? "" : Model.fixText(env),
        // A restore's ownership choices, and the backup's folder for a copy by hand.
        "choices": Model.ambiguityChoices(env), "folder": Model.backupFolderOf(env),
        // A name nothing is known for: Search for it, in place of its command (omad-z5il).
        "search": Model.searchFixOf(env) }
    }
    return { "message": fallback, "code": "", "fixes": [], "fixText": "", "choices": [], "folder": "", "search": "" }
  }

  // Called by a lane (directly, so the values stay JavaScript) when a request ends.
  // `meta` is `{argv, exitCode}`, kept on the request for its failure's details.
  function laneFinished(r, outcome, stdout, tail, meta) {
    if (meta) {
      r.argv = meta.argv
      r.exitCode = meta.exitCode
    }
    var env = outcome === "ok" || outcome === "failed" || outcome === "incompatible" ? Model.parseEnvelope(stdout) : null
    // A child that outlived its SIGKILL: the lane let it go (the watchdog), and says so
    // whatever it was running, a background read included.
    if (outcome === "stuck") {
      lastError = failureOf(r, outcome, tail)
      lastErrorResource = r.resource
    }
    if (activityOf(r)) setActivity(Model.laneOf(r), null)
    if (r.kind === "mutation") {
      action = null
      // The rows it changed say so a moment (Model.changedRows), then read as usual.
      if (env && r.priority !== "background" && r.purpose !== "preview") markDone(Model.changedRows(env))
    }
    refreshBar()
    noteOutcome(r, outcome)
    requestFinished(r.resource, outcome)
    // A catalog read with no answer is asked again the next time the view needs it.
    if (r.resource === "catalog" && outcome !== "ok") {
      var asked = {}
      for (var scope in catalogAsked) if (scope !== String(r.target || "")) asked[scope] = catalogAsked[scope]
      catalogAsked = asked
    }
    if (outcome === "missing") {
      pendingDismissals = []
      // An install on several clients goes no further without its binary.
      eachInstall = null
      pendingInstalls = []
      eachOutcome = null
      phase = "missing"
      statusFailure = failureFacts(r, outcome, tail)
      scheduleRetry()
      refreshBar()
      if (r.kind === "mutation") lastError = failureOf(r, outcome, tail)
      // An automatic update goes no further without its binary (1n2p.36).
      if (r.spec && r.spec.auto === true) autoUpdateStage = ""
    } else if (outcome === "incompatible") {
      enterIncompatible(env)
    } else if (Model.acceptResult(r, lastApplied, selectionFor(r), contextGeneration)) {
      if (r.kind !== "mutation") lastApplied = Model.markApplied(lastApplied, r)
      apply(r, outcome, env, tail)
    } else {
      console.log("omaddons: dropped a late", r.resource, "answer")
      if (r.spec && r.spec.auto === true) autoUpdateStage = ""
    }
    pump()
  }

  function apply(r, outcome, env, tail) {
    // A copy's export (1n2p.37): its answer goes to the clipboard, or says why not.
    if (r.purpose === "copy") {
      copiedList(r, outcome, env)
      return
    }
    var read = r.resource !== "status" && r.kind !== "mutation" && r.purpose !== "action"
    // A read the player stopped (the activity bar's Cancel, the only one that cancels):
    // no failure card, and the last check stands. Notes waiting for it say they stopped.
    if (outcome === "cancelled" && r.kind !== "mutation") {
      if (r.resource === "changelog") {
        changelog = { "selection": r.selection, "data": null, "failed": Model.copy("activity.cancelled", { "label": r.label }) }
      }
      if (r.resource === "addon_info") {
        addonInfo = { "selection": r.selection, "data": null, "failed": Model.copy("activity.cancelled", { "label": r.label }) }
      }
      if (r.resource === "addon_releases") {
        addonReleases = { "selection": r.selection, "data": null, "failed": Model.copy("activity.cancelled", { "label": r.label }) }
      }
      return
    }
    // Every check, answered or not: whether a source did not answer.
    if (r.resource === "check") lastCheck = Model.reduceCheck(env, outcome)
    // A read that failed before and answers now: its failure card goes.
    if (read && outcome === "ok" && lastError !== null && lastErrorResource === r.resource) {
      lastError = null
      lastErrorResource = ""
    }
    if (r.resource === "status") {
      applyStatus(r, outcome, env, tail)
    } else if (r.kind === "mutation" || r.purpose === "action") {
      var before = lastError
      applyAction(r, outcome, env, tail)
      if (lastError !== before) lastErrorResource = ""
    } else if (outcome !== "ok") {
      // The list an automatic update waited for did not answer: it waits for the next
      // background check (1n2p.36).
      if (r.resource === "list" && autoUpdateStage === "list" && r.id >= autoUpdateListAfter) autoUpdateStage = ""
      // A background read's failure stays out of the panel; the next status says how
      // the read went.
      if (r.priority === "background") console.warn("omaddons:", r.label + ":", outcome)
      else {
        lastError = env ? errorOf(env, r.label + ": " + outcome) : failureOf(r, outcome, tail)
        lastErrorResource = r.resource
        // A row waiting for its notes says they could not be read, never "Reading…".
        if (r.resource === "changelog") changelog = { "selection": r.selection, "data": null, "failed": lastError.message }
        if (r.resource === "addon_info") addonInfo = { "selection": r.selection, "data": null, "failed": lastError.message }
        if (r.resource === "addon_releases") addonReleases = { "selection": r.selection, "data": null, "failed": lastError.message }
        if (r.resource === "explain") explanation = { "selection": r.selection, "lines": null, "failed": lastError.message }
        // A backup's page waiting for its addons says they could not be read.
        if (r.resource === "backups_detail") backupsDetail = { "selection": r.selection, "data": null, "failed": lastError.message }
        // The settings sheet's omaddons section says so too.
        if (r.resource === "config") configFailed = lastError.message
      }
    } else if (r.resource === "list") {
      lists = Model.reduceList(env)
      listsAt = Date.now()
      if (autoUpdateStage === "list" && r.id >= autoUpdateListAfter) autoUpdatePreview()
    } else if (r.resource === "errors") {
      errors = Model.reduceErrors(env)
      errorsAt = Date.now()
    } else if (r.resource === "errors_raw") {
      rawErrors = { "selection": r.selection, "data": Model.reduceErrors(env) }
    } else if (r.resource === "backups") {
      backups = Model.reduceBackups(env)
      backupsAt = Date.now()
    } else if (r.resource === "history") {
      history = Model.reduceHistory(env)
      historyAt = Date.now()
    } else if (r.resource === "backups_detail") {
      backupsDetail = { "selection": r.selection, "data": Model.reduceBackups(env) }
    } else if (r.resource === "config") {
      config = Model.reduceConfig(env)
      configFailed = config ? "" : Model.copy("settings.omaddons.failed")
    } else if (r.resource === "search") {
      var found = Model.reduceSearch(env)
      found.selection = r.selection
      search = found
    } else if (r.resource === "catalog") {
      var kept = {}
      for (var scope in catalogs) kept[scope] = catalogs[scope]
      kept[String(r.target || "")] = Model.reduceSearch(env)
      catalogs = kept
    } else if (r.resource === "changelog") {
      changelog = { "selection": r.selection, "data": env.data }
    } else if (r.resource === "addon_info") {
      addonInfo = { "selection": r.selection, "data": Model.reduceInfo(env), "failed": "" }
    } else if (r.resource === "addon_releases") {
      addonReleases = { "selection": r.selection, "data": Model.reduceInfo(env), "failed": "" }
    } else if (r.resource === "explain") {
      explanation = { "selection": r.selection, "lines": Model.explainLines(env, Build.CONTEXT, targets), "failed": "" }
    } else if (r.resource === "check") {
      // A background check asks for an automatic update (1n2p.36); the status after it
      // says whether there is one to make (afterStatus). A list read asked for the last
      // one has long answered.
      if (r.priority === "background") {
        autoUpdatePending = true
        if (autoUpdateStage === "list") autoUpdateStage = ""
      }
      refresh()
    }
  }

  function applyStatus(r, outcome, env, tail) {
    if (outcome === "ok" || outcome === "failed") {
      var model = Model.reduceStatus(env)
      statusFailed = model.failed
      targets = model.targets
      summary = model.summary
      lastChange = model.lastChange
      widgetInfo = model.widget
      // A failed status keeps the token's state it last knew.
      if (model.github) github = model.github
      commands = model.commands
      statusWarnings = model.warnings
      machineIssues = model.issues
      journal = model.journal
      interrupted = Model.interruptedChange(model)
      phase = "ok"
      incompatibleText = ""
      unreachableText = ""
      // An answer: no card, no retry, another build no more.
      statusFailure = null
      incompatible = null
      retryStep = 0
      retryTimer.stop()
      lastRefreshMs = Date.now()
      // A failed status says why (a broken config.toml, a corrupt state file), never
      // "no clients found"; the next one that answers takes it away.
      if (outcome === "failed") {
        lastError = errorOf(env, "omaddons status failed")
        lastErrorResource = "status"
      } else if (lastErrorResource === "status") {
        lastError = null
        lastErrorResource = ""
      }
      // A repair's result offers its Undo (the bound Undo last) only once a status asked
      // after it finished names it as the last change: one already running read the
      // history before the repair committed. A fresh object, so the change signal fires.
      if (outcome === "ok" && lastResult && lastResult.repair === true && lastResult.statusSeen !== true
          && r.id > lastResult.statusAfter) {
        var seen = {}
        for (var key in lastResult) if (lastResult.hasOwnProperty(key)) seen[key] = lastResult[key]
        seen.statusSeen = true
        lastResult = seen
      }
      refreshBar()
      if (outcome === "ok" && r.template === "status_full") lastFullScanAt = Date.now()
      if (outcome === "ok") statusApplied()
      if (openFollowUp && r.label === "Open panel") afterOpen(model.commands)
      return
    }
    // malformed, oversized, timeout, cancelled or stuck: no answer to show. The card says
    // which (Model.phaseCard) and the status read is asked again by itself; the bar keeps
    // the last answer until it goes stale (Model.barState).
    phase = "unreachable"
    unreachableText = outcome === "malformed" ? (tail.length ? tail.join("\n") : "omaddons gave no answer") : "omaddons status: " + outcome
    statusFailure = failureFacts(r, outcome, tail)
    scheduleRetry()
    refreshBar()
  }

  function applyAction(r, outcome, env, tail) {
    // An automatic update's preview and apply have their own rules (1n2p.36): never a
    // sheet, never the player's confirmation touched.
    if (r.purpose === "preview" && r.spec && r.spec.auto === true) {
      autoUpdatePreviewed(r, outcome, env)
      return
    }
    if (r.purpose === "planned" && r.spec && r.spec.auto === true) {
      autoUpdateApplied(r, outcome, env, tail)
      return
    }
    if (r.purpose === "preview") {
      applyPreview(r, outcome, env)
      return
    }
    if (r.priority === "background" && outcome === "ok") {
      // Silent (M3-D11): a background change shows nothing unless it fails.
      changed()
      return
    }
    if (outcome === "ok" || outcome === "failed") {
      var commandsOf = []
      if (env && Array.isArray(env.commands)) {
        for (var i = 0; i < env.commands.length; i++) {
          var c = env.commands[i]
          var verdict = Array.isArray(c.argv) ? Model.classifyCliArgv(c.argv) : null
          commandsOf.push({ "action": String(c.action || ""), "command": String(c.command || ""),
            "argv": verdict && verdict.ok ? c.argv.slice() : null, "runnable": verdict !== null && verdict.ok })
        }
      }
      var repair = Model.doctorResult(env)
      if (repair) {
        // A repair's report is an answer whatever its exit (1, 2 and 3 are findings left,
        // a partial repair and a rolled-back one, M4-D34): its line says what happened, and
        // only its error (a refusal, the lock, a changed subject) is the failure card.
        // `statusAfter`: the last request asked before it, for its Undo (applyStatus).
        lastResult = { "ok": repair.exitCode === 0, "line": Model.resultLine(env, Build.CONTEXT, targets),
          "partial": repair.state === "DONE_PARTIAL", "commands": commandsOf, "fixes": [],
          "repair": repair.repaired, "statusSeen": false, "statusAfter": lastRequestId }
        lastError = repair.error ? errorOf(env, r.label + " failed") : null
      } else {
        // A plan's apply reads as the command it ran ("Updated 1 addon").
        var view = r.purpose === "planned" ? (Model.applyResult(env) || env) : env
        lastResult = { "ok": outcome === "ok", "line": Model.resultLine(view, Build.CONTEXT, targets), "partial": !!env && env.partial === true,
          "commands": commandsOf, "fixes": Model.itemFixes(view) }
        lastError = outcome === "ok" ? null : errorOf(env, r.label + " failed")
        // A client added by its folder (1n2p.32): the clients read with their scan, which
        // also puts the new client into the discovery cache the cached reads start from.
        if (r.template === "targets_add" && outcome === "ok") rescan()
        // A setting the settings sheet changed (1n2p.31): omaddons' settings read again,
        // whatever came of it, so the sheet shows what config.toml holds.
        if (String(r.template).indexOf("config_") === 0) readConfig()
        // A plan gone stale or gone, or a client's part gone stale: the action is previewed
        // again (first in line), never applied again (M4-D28).
        if (r.purpose === "planned" && r.spec && Model.needsPreviewAgain(env)) previewOf(r.spec, Model.staleText(env, targets), true)
      }
      var stale = outcome === "failed" && env.error && env.error.code === "plan_stale"
      // A preview gone stale is previewed again, never rerun with the same token.
      if (stale && r.purpose === "confirmed" && r.spec) previewOf(r.spec, Model.staleText(env, targets), false)
      if (stale && r.purpose === "bind") {
        lastError = { "message": Model.boundStaleText(r.template, env), "code": "plan_stale", "fixes": [], "fixText": "" }
      }
    } else {
      // No envelope (a timeout, an unreadable answer, a child let go): the failure in
      // words, its output only in Copy details.
      lastError = failureOf(r, outcome, tail)
      lastResult = { "ok": false, "line": lastError.message, "partial": false, "commands": [] }
    }
    // One client's apply of an install on several (omad-3rre): the next, or the end and
    // its result. Only the end speaks.
    if (r.purpose === "planned" && r.spec && r.spec.each === true && eachOutcome) appliedEach(r, outcome, env)
    var chaining = eachOutcome !== null
    // A change asked in the panel that finished after every panel closed: its result
    // line as a toast; a click opens the panel.
    if (!chaining && r.kind === "mutation" && r.priority !== "background" && !panelOpen && canAct) {
      Quickshell.execDetached(Model.resultToastArgv(lastResult.line, lastResult.ok, Build.PLUGIN_ID))
    }
    changed()
    // `d` across clients: the next dismissal, after this one succeeded.
    if (r.template === "dismiss_errors") {
      var rest = pendingDismissals
      pendingDismissals = []
      if (outcome === "ok" && rest.length > 0) {
        pendingDismissals = rest.slice(1)
        dismissOne(rest[0])
      }
    }
  }

  // Another omaddons build answered (M3-D42): the views keep the last numbers, the bar
  // turns broken; clear the confirmation; drop every queued mutation; refuse new ones
  // until a compatible status. Retry and Reinstall widget stay available.
  function enterIncompatible(env) {
    phase = "incompatible"
    incompatibleText = Model.incompatibleText(env, Build)
    incompatible = Model.incompatibleOf(env, Build)
    confirm = null
    pendingConfirm = null
    pendingDismissals = []
    eachInstall = null
    pendingInstalls = []
    eachOutcome = null
    queues = { "mutation": [], "local": queues.local.slice(), "network": queues.network.slice() }
    // An automatic update's queued step went with them (1n2p.36).
    autoUpdateStage = ""
    queuedAction = ""
    queuedText = Model.queuedText(queues)
    // After its binary was upgraded, the widget reinstalls itself once from it (1n2p.35,
    // Model.selfUpdate says when): a toast says so, and the bar reloads with the new
    // widget. Otherwise the phase card offers it.
    var update = Model.selfUpdate({ "phase": phase, "incompatible": incompatible, "env": env, "settings": root.settings,
      "tried": selfUpdatedFor })
    if (update.run) {
      selfUpdatedFor = update.version
      Quickshell.execDetached(Model.resultToastArgv(Model.copy("widget.self_update", { "version": update.version }), true, Build.PLUGIN_ID))
      reinstallWidget()
    }
    refreshBar()
  }

  // ------------------------------------------------------------- desktop triggers (F8)

  // Each of these only adds requests to the scheduler above, and every one waits for a
  // compatible answer (M3-D42).

  // Hyprland's window events ring like the doorbell (M3-D13): a client starting or
  // exiting shows within a second. Only a window that may be WoW's rings
  // (Model.windowEventAction, 1n2p.38): a terminal or a stream overlay opening runs no
  // `status`. A Wine game outlives its window, so a close while a client ran is followed
  // by refreshes at each of followUpDelaysMs.
  function windowEvent(name, data) {
    var verdict = Model.windowEventAction(name, data, wowWindows, anyRunning)
    if (!verdict) return
    if (verdict.remember) wowWindows = wowWindows.concat([verdict.remember]).slice(-Model.WINDOW_MEMORY)
    if (verdict.forget) wowWindows = wowWindows.filter(function(a) { return a !== verdict.forget })
    if (!verdict.refresh) return
    windowDebounce.restart()
    if (verdict.followUp) {
      followUpStep = 0
      followUpTimer.interval = followUpDelaysMs[0]
      followUpTimer.restart()
    }
  }

  function followUp() {
    refresh()
    followUpStep++
    if (followUpStep < followUpDelaysMs.length) {
      followUpTimer.interval = followUpDelaysMs[followUpStep] - followUpDelaysMs[followUpStep - 1]
      followUpTimer.restart()
    }
  }

  // After each compatible status: for a client that stopped, its post-session settings
  // checkpoint and then its Lua errors (M3-D31); at the first status, a checkpoint for
  // every stopped client (the CLI skips an unchanged one); errors read again where
  // stale or unchecked (M3-D20); a due update check; and the toasts, never on the first
  // status (M3-D14). A new binary starts over, as the service's start does.
  function afterStatus() {
    if (phase !== "ok") return
    var first = !observed || observed.generation !== contextGeneration
    var stopped = first ? [] : Model.stoppedTargets(observed.targets, targets)
    var checkpoints = first ? Model.stoppedNow(targets) : stopped
    // A change cut off (Model.interruptedChange): `backup create` recovers it before it
    // copies anything, and the widget never finishes a change without the player
    // (1n2p.34). The checkpoints wait until a status reports it settled.
    var due = heldCheckpoints.slice()
    for (var c = 0; c < checkpoints.length; c++) if (due.indexOf(checkpoints[c]) === -1) due.push(checkpoints[c])
    heldCheckpoints = interrupted ? due : []
    if (!interrupted) {
      for (var i = 0; i < due.length; i++) {
        run("backup_auto", { "target": due[i] }, first ? "Settings checkpoint at widget start" : "Settings checkpoint after play")
      }
    }
    // A client that records no Lua errors: one `errors` read (local) says whether a
    // BugGrabber build is known for it, for the first run's card (1n2p.32).
    if (Model.needsCaptureRead(targets, errors)) {
      request({ "resource": "errors", "template": "errors", "priority": "background", "label": "Read Lua errors" })
    }
    var reads = stopped.slice()
    var stale = Model.errorsToRefresh(targets)
    for (var j = 0; j < stale.length; j++) if (reads.indexOf(stale[j]) === -1) reads.push(stale[j])
    for (var k = 0; k < reads.length; k++) readErrorsOf(reads[k])
    var updatesNow = summary ? summary.updates.available : 0
    var errorsNow = summary && summary.errors ? summary.errors["new"] : 0
    var before = first ? { "updates": 0, "errorsNew": 0 } : observed
    // Toasts are argv (M3-D14), and the errors in them are only names, counts and clients
    // (M3-D21). Nothing speaks while a game may run: what rose waits for one digest after
    // the session, unless notifyWhilePlaying (1n2p.33).
    var plan = Model.toastPlan(toastState, { "first": first, "before": before, "errorsNew": errorsNow, "updates": updatesNow,
      "playing": Model.playing(targets), "notifyErrors": switchOn("notifyErrors"), "notifyUpdates": switchOn("notifyUpdates"),
      "whilePlaying": switchOn("notifyWhilePlaying"), "nowMs": Date.now() })
    toastState = plan.state
    if (canAct && plan.toast === "errors") {
      Quickshell.execDetached(Model.errorToastArgv(Model.errorFacts(targets), Build.PLUGIN_ID))
    }
    if (canAct && plan.toast === "updates") {
      Quickshell.execDetached(Model.updateToastArgv(Model.updateFacts(targets, summary), Build.PLUGIN_ID))
    }
    if (canAct && plan.toast === "digest") {
      Quickshell.execDetached(Model.digestToastArgv(Model.errorFacts(targets), Model.updateFacts(targets, summary), plan.kinds, Build.PLUGIN_ID))
    }
    observed = { "generation": contextGeneration, "targets": targets, "updates": updatesNow, "errorsNew": errorsNow }
    autoCheck()
    autoUpdate()
  }

  // One client's Lua errors, in the background, queued once per client. The read rings
  // the doorbell when it changes what `status` reports.
  function readErrorsOf(target) {
    request({ "resource": "errors_target", "template": "errors_target", "target": target, "slots": { "target": target },
      "priority": "background", "label": "Read Lua errors" })
  }

  // An update check when `status` says one is due (M3-D12), at most once per
  // autoCheckIntervalMs; Check now runs regardless.
  function autoCheck() {
    if (phase !== "ok") return
    var now = Date.now()
    if (!Model.autoCheckDue(switchOn("autoCheck"), summary, lastAutoCheckMs, now, autoCheckIntervalMs)) return
    lastAutoCheckMs = now
    request({ "resource": "check", "template": "check", "lane": "network", "priority": "background", "label": "Automatic update check" })
  }

  function switchOn(key) {
    return Model.setting(root.settings, key) === "On"
  }

  // ---- automatic updates (1n2p.36): the one place the widget changes addons with no
  // key pressed, and only as `U` would, through the same preview and plan.

  // After each compatible status: the update a background check asked for, once
  // Model.autoUpdateDue allows it (no client running, nothing else underway). `list`
  // first, so the preview is compared with the rows the Updates view would show.
  function autoUpdate() {
    var due = Model.autoUpdateDue({ "pending": autoUpdatePending, "settings": root.settings, "held": autoUpdateHeld,
      "stage": autoUpdateStage, "canAct": canAct, "playing": Model.playing(targets), "interrupted": !!interrupted,
      "busy": mutationLane.busy || queues.mutation.length > 0 || confirm !== null,
      "updates": summary && summary.updates ? summary.updates.available : 0 })
    if (due.drop) autoUpdatePending = false
    if (!due.run) return
    autoUpdateStage = "list"
    autoUpdateListAfter = lastRequestId + 1
    request({ "resource": "list", "template": "list", "label": "Installed", "priority": "background" })
  }

  // The list answered: `U`'s preview over every client, as a `match` of its update rows.
  // A game started or a sheet opened meanwhile puts it back for a later status.
  function autoUpdatePreview() {
    if (!canAct || Model.playing(targets) || !!interrupted || confirm !== null) {
      autoUpdateStage = ""
      autoUpdatePending = true
      return
    }
    var wanted = Model.updateRequest("", Model.updateRows(lists, "", targets), targets)
    autoUpdateStage = "preview"
    previewOf({ "template": wanted.template, "slots": wanted.slots, "label": Model.copy("auto_update.label"),
      "priority": "background", "mode": "match", "shown": wanted.shown, "auto": true }, "", false)
  }

  // The preview's answer (Model.autoUpdatePreviewAct): its plan applied only when the
  // panel itself would apply it with no sheet; anything that needs the player is left
  // for them, with the Updates badge as before.
  function autoUpdatePreviewed(r, outcome, env) {
    var path = Model.templateShape(r.template, templates).path
    var decision = Model.previewDecision({ "mode": "match", "shown": r.shown, "again": r.again === true, "path": path,
      "context": Build.CONTEXT, "targets": targets }, env)
    var act = Model.autoUpdatePreviewAct(outcome, env, decision)
    if (act === "run") {
      autoUpdateStage = "plan"
      runPlan(decision.plan, r.spec, r.contextGeneration)
      return
    }
    autoUpdateStage = ""
    if (act === "stop") stopAutoUpdate(env ? errorOf(env, r.label + " failed") : null)
  }

  // The apply's answer (Model.autoUpdateApplyAct): a notification says what changed,
  // whose click opens History with its Undo; a failure is said on the panel's card and
  // once in a notification, and turns automatic updates off.
  function autoUpdateApplied(r, outcome, env, tail) {
    autoUpdateStage = ""
    var act = Model.autoUpdateApplyAct(outcome, env)
    if (act === "done" && canAct) Quickshell.execDetached(Model.autoUpdateToastArgv(Model.autoUpdateFacts(env, targets), Build.PLUGIN_ID))
    if (act === "stop") stopAutoUpdate(env ? errorOf(env, r.label + " failed") : failureOf(r, outcome, tail))
    changed()
  }

  // A failure: the setting turned Off (saved by the bar) and held off in this service
  // until the player turns it on again; the panel's card says what failed.
  function stopAutoUpdate(error) {
    autoUpdatePending = false
    autoUpdateHeld = true
    autoUpdateStopped = true
    if (error) {
      lastError = error
      lastResult = { "ok": false, "line": error.message, "partial": false, "commands": [] }
    }
    settingRequested("autoUpdate", "Off")
    if (canAct) Quickshell.execDetached(Model.autoUpdateStoppedToastArgv(Model.updateFacts(targets, summary), Build.PLUGIN_ID))
  }

  // ------------------------------------------------------------- reinstall and show

  // `omaddons omarchy install` detached, under the stamped context: the reload it causes
  // destroys this service and its children.
  function reinstallWidget() {
    reinstallProc.command = [root.binary, "--actor", "widget", "--reason", "reinstall from the bar widget", "omarchy", "install"]
    reinstallProc.environment = Build.CONTEXT
    reinstallProc.startDetached()
    // A reinstall that works reloads the widget, and this service with it; one that
    // does not leaves the card saying when it was tried (Model.phaseCard).
    reinstalledAtMs = Date.now()
    refreshBar()
    return "ok"
  }

  // A first-run card put off (Not now, 1n2p.32): for the shell's lifetime, per client.
  function dismissOnboarding(kind, target) {
    var next = {}
    for (var k in onboardingDismissed) next[k] = onboardingDismissed[k].slice()
    var list = next[kind] || []
    if (list.indexOf(String(target || "")) === -1) list.push(String(target || ""))
    next[kind] = list
    onboardingDismissed = next
  }

  // The cards the panel shows now, by kind, for IPC's state() (1n2p.34): the phase
  // card's (Model.phaseCard), which stands alone; else "interrupted" for an interrupted
  // change, and the failure card's ("stuck" for a lane let go, else "failure").
  function cardKinds() {
    if (phaseCard) return [phaseCard.kind]
    var kinds = []
    if (interrupted && phase === "ok") kinds.push("interrupted")
    if (lastError) kinds.push(lastError.code === "stuck" ? "stuck" : "failure")
    return kinds
  }

  // Opens the panel on the focused monitor at `view`; `summon` opens, never toggles.
  // The payload is dropped for bar widgets (F0), so the view waits here.
  function show(view) {
    requestedView = String(view || "")
    var host = root.shell
    if (host && typeof host.summon === "function" && host.summon(Build.PLUGIN_ID, "") === true) return "ok"
    Quickshell.execDetached(["omarchy-shell", "shell", "summon", Build.PLUGIN_ID, "{}"])
    return "ok"
  }

  function laneState(lane, name) {
    return { "running": lane.request ? lane.request.resource : null, "queued": queues[name].length }
  }

  function stateJson() {
    return JSON.stringify({
      "phase": phase,
      "bar": bar,
      "summary": summary,
      "action": action,
      // What the activity bar says now (1n2p.18): `{kind, text, fraction, elapsed,
      // stalled, cancellable}`, null when nothing the player waits on runs.
      "activity": Model.activityView(activity, targets, Date.now()),
      "lastResult": lastResult ? { "ok": lastResult.ok, "line": lastResult.line } : null,
      "panelOpen": panelOpen,
      // The open panel's scope, view, page depth, open sheet and zoom (design doc §2.1).
      "panel": panelOpen ? panelState : null,
      "binary": root.binary,
      "toolVersion": Build.TOOL_VERSION,
      "widgetVersion": manifest && manifest.version ? String(manifest.version) : "",
      "contextGeneration": contextGeneration,
      "lanes": { "mutation": laneState(mutationLane, "mutation"), "local": laneState(localLane, "local"), "network": laneState(networkLane, "network") },
      "lastRefreshMs": lastRefreshMs,
      // The cards shown now, by kind (cardKinds), for the checkpoint scripts.
      "cards": cardKinds(),
      // The open confirmation and the last plan preview's decision (M4-D28): the plan
      // sheet's title, and the whole sheet as text, a line each.
      "confirm": confirm ? { "heading": confirm.heading || "", "planId": pendingConfirm && pendingConfirm.plan ? pendingConfirm.plan.planId : null,
        "title": confirm.sheet ? String(confirm.sheet.title || "") : "", "text": Model.dialogText(confirm.sheet) } : null,
      "lastPreview": lastPreview
    })
  }

  // ------------------------------------------------------------- freshness (M3-D4)

  function startBell() {
    bellStarted = false
    bellProc.running = true
  }

  // A plain desktop helper: no stamped context.
  Process {
    id: bellProc
    command: ["inotifywait", "-m", "-q", "-e", "moved_to", "--format", "%f", Build.CACHE_DIR]
    stdout: SplitParser {
      onRead: function(name) { if (name === "doorbell.json") bellDebounce.restart() }
    }
    onStarted: root.bellStarted = true
    onExited: function(exitCode, exitStatus) {
      // inotifywait exits 1 at once on a missing directory.
      bellRetry.interval = exitCode === 1 ? root.bellMissingRetryMs : root.bellRetryMs
      bellRetry.restart()
    }
    onRunningChanged: if (!running && !root.bellStarted) {
      bellRetry.interval = root.bellMissingRetryMs
      bellRetry.restart()
    }
  }

  Timer {
    id: bellDebounce
    interval: root.bellDebounceMs
    onTriggered: root.changed()
  }

  Timer {
    id: bellRetry
    onTriggered: root.startBell()
  }

  // The safety net behind the doorbell, and the only freshness when it cannot run.
  // While a client runs, at most runningPollMs, so its exit is never missed for long.
  Timer {
    id: pollTimer
    repeat: true
    running: true
    interval: {
      var base = root.panelOpen ? root.panelPollMs : Model.intSetting(root.settings, "refreshIntervalSec", 300, 30, 3600) * 1000
      return root.anyRunning ? Math.min(base, root.runningPollMs) : base
    }
    onTriggered: root.refresh()
  }

  // The retry after a missing or silent binary (scheduleRetry).
  Timer {
    id: retryTimer
    onTriggered: root.refresh()
  }

  // The bar's clock: an answer goes stale, and the tooltip's ages move, with nothing
  // else changing (Model.barState).
  Timer {
    id: barTimer
    repeat: true
    running: true
    interval: Model.BAR_TICK_MS
    onTriggered: root.refreshBar()
  }

  // Window events, filtered to the windows that may be WoW's (windowEvent): a Proton
  // game window's class names its launcher, not the game (M3-D13), so the filter is wide.
  Connections {
    target: Hyprland
    function onRawEvent(event) {
      root.windowEvent(String(event && event.name ? event.name : ""), String(event && event.data ? event.data : ""))
    }
  }

  Timer {
    id: windowDebounce
    interval: root.windowDebounceMs
    onTriggered: root.refresh()
  }

  Timer {
    id: followUpTimer
    onTriggered: root.followUp()
  }

  Timer {
    id: autoCheckTimer
    repeat: true
    running: true
    interval: root.autoCheckIntervalMs
    onTriggered: root.autoCheck()
  }

  // The last result leaves the pinned area after resultHoldMs (design doc §2.1).
  Timer {
    id: resultTimer
    interval: root.resultHoldMs
    onTriggered: root.resultShown = false
  }

  // The rows a change changed read as usual again after rowDoneMs.
  Timer {
    id: doneTimer
    interval: root.rowDoneMs
    onTriggered: root.doneRows = ({})
  }

  Process {
    id: reinstallProc
  }

  // One copy (copyCommand): its own wl-copy, decided on `exited` alone. wl-copy forks a
  // child that serves the clipboard and may keep the output open, so the end of a
  // stream proves nothing; a missing binary never starts and never exits, it only stops
  // running (F0). The copier answers once, then goes.
  Component {
    id: copyComponent

    Item {
      id: copier

      property string text: ""
      property var done: null
      property bool started: false
      property bool settled: false

      function start() {
        deadline.restart()
        child.running = true
      }

      function settle(ok) {
        if (settled) return
        settled = true
        deadline.stop()
        var callback = done
        done = null
        // The view that asked may have gone (a page closed) before wl-copy answered.
        if (callback) {
          try {
            callback(ok)
          } catch (e) {
            console.warn("omaddons: a copy's answer reached no view:", e)
          }
        }
        copier.destroy()
      }

      // No exit in time: the child is stopped (SIGTERM, F0) and the copy did not happen.
      // Only here: after `exited` the child is gone, and nothing is signalled.
      Timer {
        id: deadline
        interval: root.copyDeadlineMs
        onTriggered: {
          if (child.running) child.running = false
          copier.settle(false)
        }
      }

      Process {
        id: child
        command: Model.copyArgv(copier.text)
        onStarted: copier.started = true
        onExited: function(exitCode, exitStatus) { copier.settle(exitCode === 0 && exitStatus === 0) }
        onRunningChanged: if (!running && !copier.started) copier.settle(false)
      }
    }
  }

  // One read of the clipboard (pasteAddonList): wl-paste through its builder, in a
  // process of its own on no lane, decided once it exited 0 and its output ended (in
  // either order, F0). A missing wl-paste never starts, any other exit means no text was
  // there, and a read past pasteDeadlineMs is stopped: each is "could not read". The
  // reader answers once, then goes. Its text is checked by the caller, never run.
  Component {
    id: pasteComponent

    Item {
      id: reader

      property var done: null
      property bool started: false
      property bool exited: false
      property bool ended: false
      property int code: -1
      property bool settled: false

      function start() {
        deadline.restart()
        child.running = true
      }

      function settle(ok) {
        if (settled) return
        settled = true
        deadline.stop()
        var callback = done
        done = null
        var text = ok && typeof collector.text === "string" ? collector.text : ""
        if (callback) {
          try {
            callback(ok, text)
          } catch (e) {
            console.warn("omaddons: a paste's answer reached no view:", e)
          }
        }
        reader.destroy()
      }

      function check() {
        if (exited && code !== 0) settle(false)
        else if (exited && ended) settle(true)
      }

      // No answer in time: the child is stopped (SIGTERM, F0) and nothing is pasted.
      Timer {
        id: deadline
        interval: root.pasteDeadlineMs
        onTriggered: {
          if (child.running) child.running = false
          reader.settle(false)
        }
      }

      Process {
        id: child
        command: Model.pasteArgv()
        stdout: StdioCollector {
          id: collector
          onStreamFinished: {
            reader.ended = true
            reader.check()
          }
        }
        onStarted: reader.started = true
        onExited: function(exitCode, exitStatus) {
          reader.exited = true
          reader.code = exitStatus === 0 ? exitCode : -1
          reader.check()
        }
        onRunningChanged: if (!running && !reader.started) reader.settle(false)
      }
    }
  }

  IpcHandler {
    id: ipc
    target: Build.PLUGIN_ID

    function state(): string { return root.stateJson() }
    function refresh(): string { root.refresh(); return "ok" }
    function check(): string { root.check(); return "ok" }
    function show(view: string): string { return root.show(view) }
  }

  // ------------------------------------------------------------- the runner

  // One lane runs one request at a time: a fresh Process per request, completion on
  // `exited` and the end of stdout (in either order, F0) with `running` false, a
  // deadline for reads (from the start, or of inactivity for network reads), and
  // SIGTERM then SIGKILL, the lane held until the child really exits. A request built
  // with standard input (a pasted list, 1n2p.37) gets it written once the child starts,
  // then closed, so `sync -` reads it to its end; every other child gets none.
  component Lane: Item {
    id: lane

    // The service: `laneFinished` and `laneProgress` are called on it directly.
    property var sink: null
    property int deadlineMs: 0
    property bool inactivity: false
    // The running request's deadline: its own `deadlineMs` when it sets one (a match
    // preview), else the lane's.
    property int runDeadlineMs: 0
    property bool runInactivity: false
    property int killGraceMs: 5000
    property int streamGraceMs: 2000
    property var request: null
    readonly property bool busy: request !== null
    property var proc: null
    property bool started: false
    property bool exitedSeen: false
    property bool streamSeen: false
    property bool streamGraceOver: false
    property int exitCode: -1
    property bool timedOut: false
    property bool cancelled: false
    property var tail: []
    // The running request's standard input (buildArgv's `stdin`), or null for none.
    property var input: null

    function run(r, argv, env, stdin) {
      started = false
      exitedSeen = false
      streamSeen = false
      streamGraceOver = false
      exitCode = -1
      timedOut = false
      cancelled = false
      tail = []
      request = r
      var own = typeof r.deadlineMs === "number" && r.deadlineMs >= 0
      runDeadlineMs = own ? r.deadlineMs : deadlineMs
      runInactivity = own ? r.inactivity === true : inactivity
      input = typeof stdin === "string" ? stdin : null
      proc = processComponent.createObject(lane, { "stdinEnabled": input !== null, "command": argv, "environment": env })
      if (runDeadlineMs > 0) deadline.restart()
      proc.running = true
    }

    // The child started: its standard input written whole, then closed (its end of file).
    function feed(child) {
      if (input === null) return
      child.write(input)
      child.stdinEnabled = false
    }

    function cancel() {
      if (!proc || cancelled) return
      cancelled = true
      terminate()
    }

    // SIGTERM to the pid (what `running = false` sends, F0), then SIGKILL after the
    // grace period; never the group, which is the shell's. The grace starts before the
    // signal: a child that ends at once finishes the request, which stops it, and a
    // timer started after that would kill the lane's next child.
    function terminate() {
      if (!proc) return
      killTimer.restart()
      proc.signal(15)
    }

    function line(text) {
      var shown = String(text).slice(0, 512)
      var next = tail.concat([shown])
      if (next.length > 20) next = next.slice(next.length - 20)
      tail = next
      var event = null
      try {
        event = JSON.parse(text)
      } catch (e) {
        event = null
      }
      if (!event || event.event !== "progress") return
      if (runInactivity && runDeadlineMs > 0 && !timedOut && !cancelled) deadline.restart()
      sink.laneProgress(request, event)
    }

    function settle() {
      if (!request || !proc || !exitedSeen || proc.running) return
      if (!streamSeen && !streamGraceOver) {
        if (!streamGrace.running) streamGrace.restart()
        return
      }
      finish("")
    }

    function finish(forced) {
      if (!request) return
      deadline.stop()
      killTimer.stop()
      watchdog.stop()
      streamGrace.stop()
      var text = proc.stdout && typeof proc.stdout.text === "string" ? proc.stdout.text : ""
      var outcome = forced || Model.outcomeOf(exitCode, started, text, timedOut, cancelled, Build)
      var done = request
      var shown = tail
      var old = proc
      var meta = { "argv": Model.listOf(old.command).slice(), "exitCode": exitedSeen ? exitCode : -1 }
      request = null
      proc = null
      input = null
      old.destroy()
      sink.laneFinished(done, outcome, text, shown, meta)
    }

    Timer {
      id: deadline
      interval: lane.runDeadlineMs
      onTriggered: {
        lane.timedOut = true
        lane.terminate()
      }
    }

    Timer {
      id: killTimer
      interval: lane.killGraceMs
      onTriggered: if (lane.proc) {
        lane.proc.signal(9)
        watchdog.restart()
      }
    }

    // A child that never reports its end even after SIGKILL would hold the lane for
    // good, and every request behind it (1n2p.34): one more grace, then the lane lets
    // it go and frees itself (outcome "stuck", which the service says in words).
    Timer {
      id: watchdog
      interval: lane.killGraceMs
      onTriggered: if (lane.proc && lane.request) lane.finish("stuck")
    }

    Timer {
      id: streamGrace
      interval: lane.streamGraceMs
      onTriggered: {
        lane.streamGraceOver = true
        lane.settle()
      }
    }

    // Each handler acts only while its process is the lane's current one: a finished
    // process can still report (its `running` turning false after `exited`) when the
    // lane has already started the next request.
    Component {
      id: processComponent

      Process {
        id: child

        stdout: StdioCollector {
          onStreamFinished: {
            if (lane.proc !== child) return
            lane.streamSeen = true
            lane.settle()
          }
        }
        stderr: SplitParser {
          onRead: function(data) { if (lane.proc === child) lane.line(data) }
        }
        onStarted: if (lane.proc === child) {
          lane.started = true
          lane.feed(child)
        }
        onExited: function(exitCode, exitStatus) {
          if (lane.proc !== child) return
          lane.exitedSeen = true
          lane.exitCode = exitCode
          lane.settle()
        }
        // A missing binary never starts and never exits: it only stops running (F0).
        onRunningChanged: if (!running && lane.proc === child) {
          if (!lane.started) lane.finish("missing")
          else lane.settle()
        }
      }
    }
  }

  Lane {
    id: mutationLane
    sink: root
    killGraceMs: root.killGraceMs
    streamGraceMs: root.streamGraceMs
  }

  Lane {
    id: localLane
    sink: root
    deadlineMs: root.localDeadlineMs
    killGraceMs: root.killGraceMs
    streamGraceMs: root.streamGraceMs
  }

  Lane {
    id: networkLane
    sink: root
    deadlineMs: root.networkDeadlineMs
    inactivity: true
    killGraceMs: root.killGraceMs
    streamGraceMs: root.streamGraceMs
  }
}
