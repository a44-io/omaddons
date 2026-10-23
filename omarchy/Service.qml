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
  property var bar: Model.reduceStatus(null).bar
  property var targets: []
  property var summary: null
  property var lastChange: null
  property var widgetInfo: null
  // `status`'s recommendations, as buttons where the classifier accepts them.
  property var commands: []
  property var lists: null
  property var errors: null
  property var backups: null
  property var search: null
  property var changelog: null
  // The running mutation: {name, label, current, total, message}.
  property var action: null
  // The label of the queued user mutation ("next: …").
  property string queuedAction: ""
  // The last finished action: {ok, line, partial, commands}.
  property var lastResult: null
  // The last failure: {message, code, fixes}.
  property var lastError: null
  // The read whose failure `lastError` is ("" for an action's): a later success of that
  // same read takes the card away.
  property string lastErrorResource: ""
  // The open confirmation: {text, stale}.
  property var confirm: null
  property string incompatibleText: ""
  property string unreachableText: ""
  // The last outcome per resource, for the views and state().
  property var outcomes: ({})
  property string requestedView: ""
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
  // The raw details `s` asked for, view-bound: `{selection, data}`.
  property var rawErrors: null
  property var rawSelection: null
  // The dismissals `d` still has to send, one client at a time.
  property var pendingDismissals: []
  property double backupsAt: 0
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

  // ---- timing (the tests shrink these)
  property int bellDebounceMs: 150
  property int bellRetryMs: 5000
  property int bellMissingRetryMs: 30000
  property int panelPollMs: 15000
  property int runningPollMs: 30000
  property int windowDebounceMs: 500
  // After a window closed while a client ran: refreshes this long after the close.
  property var followUpDelaysMs: [2000, 5000, 15000]
  property int autoCheckIntervalMs: Model.AUTO_CHECK_INTERVAL_MS
  property int localDeadlineMs: Model.WIDGET_LOCAL_DEADLINE_SEC * 1000
  property int networkDeadlineMs: Model.WIDGET_NETWORK_DEADLINE_SEC * 1000
  property int killGraceMs: Model.WIDGET_KILL_GRACE_SEC * 1000
  property int streamGraceMs: Model.WIDGET_STREAM_GRACE_SEC * 1000

  // The template table: Model.COMMANDS; the tests add fixture templates.
  property var templates: Model.COMMANDS

  // ---- scheduler state
  property var queues: Model.emptyQueues()
  property var lastApplied: ({})
  property int lastRequestId: 0
  property var openPanels: ({})
  property var searchSelection: null
  property var changelogSelection: null
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

  // ------------------------------------------------------------- settings

  // The bar copies push their settings. A new binary is a new context: answers from
  // the old one no longer apply.
  function applySettings(next) {
    var before = root.binary
    settings = next && typeof next === "object" ? next : ({})
    if (root.binary !== before) {
      contextGeneration++
      refresh()
    }
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
    if (opened) {
      request({ "resource": "status", "template": "status_full", "label": "Open panel" })
      readList()
      readErrors()
      readBackups()
    }
  }

  // ------------------------------------------------------------- requests

  function request(fields) {
    lastRequestId++
    var r = {
      "id": lastRequestId, "contextGeneration": contextGeneration, "target": "", "resource": "",
      "kind": "data", "priority": "user", "selection": null, "lane": "local",
      "template": "", "slots": {}, "cli": null, "purpose": "read", "label": "",
      "preview": false, "token": "", "confirmed": false, "spec": null, "stale": ""
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

  function check() {
    return request({ "resource": "check", "template": "check", "lane": "network", "label": "Check now" })
  }

  function readList() {
    return request({ "resource": "list", "template": "list", "label": "Installed" })
  }

  function readErrors() {
    return request({ "resource": "errors", "template": "errors", "label": "Issues" })
  }

  // `s`: one addon's raw details (`--raw-details`), view-bound to that row.
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

  function readBackups() {
    return request({ "resource": "backups", "template": "backups", "label": "Backups" })
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
    var resource = currentView === "updates" || currentView === "installed" ? "list"
      : (currentView === "issues" ? "errors" : (currentView === "backups" ? "backups" : ""))
    if (resource === "") return false
    var have = resource === "list" ? lists : (resource === "errors" ? errors : backups)
    var at = resource === "list" ? listsAt : (resource === "errors" ? errorsAt : backupsAt)
    if (have && staleReads[resource] !== true && Date.now() - at <= viewMaxAgeMs) return false
    var next = {}
    for (var key in staleReads) if (key !== resource) next[key] = staleReads[key]
    staleReads = next
    return resource === "list" ? readList() : (resource === "errors" ? readErrors() : readBackups())
  }

  // Something changed (the doorbell, or the widget's own action): the bar first, then
  // the open panel's view.
  function changed() {
    refresh()
    staleReads = { "list": true, "errors": true, "backups": true }
    if (panelOpen && currentView !== "") readView(currentView)
  }

  // A search, view-bound to its query and target filter; All ("") searches every client
  // in one request.
  function searchFor(query, target) {
    var selection = { "query": String(query || ""), "target": String(target || "") }
    searchSelection = selection
    var shape = Model.searchRequest(selection.query, selection.target)
    return request({
      "resource": "search", "template": shape.template, "lane": "network", "kind": "view", "target": selection.target,
      "selection": selection, "slots": shape.slots, "label": "Search"
    })
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

  function previewOf(spec, stale) {
    return mutation({
      "resource": spec.template || "cli", "template": spec.template || "", "slots": spec.slots || {},
      "cli": spec.cli || null, "label": spec.label, "priority": spec.priority || "user",
      "preview": true, "purpose": "preview", "spec": spec, "stale": stale || ""
    })
  }

  // A mutating template by name: destructive ones preview first (M3-D9), silent ones
  // run in the background. Returns "" or why it was refused.
  function run(name, slots, label) {
    if (!canAct) return incompatibleText || "omaddons is not ready"
    var template = templates.hasOwnProperty(name) ? templates[name] : null
    if (!template || template.mutates !== true) return "no such action: " + name
    if (template.bind === true) return name + " runs with its view's token (runBound)"
    var spec = { "template": name, "slots": slots || {}, "label": String(label || name), "priority": template.silent === true ? "background" : "user" }
    var refusal = trialBuild(spec)
    if (refusal) return refusal
    var path = Model.templateShape(name, templates).path
    if (template.confirm === true || Model.CLI_PATHS[path].destructive) {
      previewOf(spec, "")
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
    if (verdict.destructive) {
      previewOf(spec, "")
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

  // Yes on the open confirmation: the real run, bound to its preview's token when the
  // path binds, in the generation the preview was made in.
  function confirmYes() {
    if (!confirm || !pendingConfirm) return "nothing to confirm"
    if (!canAct) return incompatibleText || "omaddons is not ready"
    var pending = pendingConfirm
    confirm = null
    pendingConfirm = null
    var spec = pending.spec
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
        "preview": true, "token": "", "confirmed": false })
    } catch (e) {
      return String(e.message || e)
    }
    return ""
  }

  function argvOf(r) {
    var opts = {
      "binary": root.binary, "label": r.label || r.template || "omaddons", "context": Build.CONTEXT,
      "templates": root.templates, "preview": r.preview === true, "token": r.token || "", "confirmed": r.confirmed === true
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
      laneNamed(name).run(r, built.argv, built.env)
    }
    queuedAction = queuedLabel()
    if (again) Qt.callLater(pump)
  }

  function queuedLabel() {
    var list = queues.mutation
    for (var i = 0; i < list.length; i++) if (list[i].priority === "user") return list[i].label
    return ""
  }

  function withSpinning(model, spinning) {
    var out = {}
    for (var key in model) if (model.hasOwnProperty(key)) out[key] = model[key]
    out.spinning = spinning
    return out
  }

  function startAction(r) {
    action = { "name": r.template || r.resource, "label": r.label, "current": 0, "total": 0,
      "message": r.preview ? "Checking what it would do…" : "" }
    bar = withSpinning(bar, true)
  }

  function laneProgress(r, event) {
    if (r.kind !== "mutation" || !action) return
    action = { "name": action.name, "label": action.label,
      "current": typeof event.current === "number" ? event.current : 0,
      "total": typeof event.total === "number" ? event.total : 0, "message": Model.progressLine(event) }
  }

  function selectionFor(r) {
    if (r.resource === "search") return searchSelection
    if (r.resource === "changelog") return changelogSelection
    if (r.resource === "errors_raw") return rawSelection
    if (r.resource === "backups_detail") return detailSelection
    return null
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
        "fixes": Model.fixButtons(env), "fixText": Model.fixText(env),
        // A restore's ownership choices, and the backup's folder for a copy by hand.
        "choices": Model.ambiguityChoices(env), "folder": Model.backupFolderOf(env) }
    }
    return { "message": fallback, "code": "", "fixes": [], "fixText": "", "choices": [], "folder": "" }
  }

  // Called by a lane (directly, so the values stay JavaScript) when a request ends.
  function laneFinished(r, outcome, stdout, tail) {
    var env = outcome === "ok" || outcome === "failed" || outcome === "incompatible" ? Model.parseEnvelope(stdout) : null
    if (r.kind === "mutation") {
      action = null
      bar = withSpinning(bar, false)
    }
    noteOutcome(r, outcome)
    requestFinished(r.resource, outcome)
    if (outcome === "missing") {
      pendingDismissals = []
      phase = "missing"
      bar = Model.reduceStatus(null).bar
      if (r.kind === "mutation") lastError = { "message": "omaddons not found at " + root.binary, "code": "missing", "fixes": [] }
    } else if (outcome === "incompatible") {
      enterIncompatible(env)
    } else if (Model.acceptResult(r, lastApplied, selectionFor(r), contextGeneration)) {
      if (r.kind !== "mutation") lastApplied = Model.markApplied(lastApplied, r)
      apply(r, outcome, env, tail)
    } else {
      console.log("omaddons: dropped a late", r.resource, "answer")
    }
    pump()
  }

  function apply(r, outcome, env, tail) {
    var read = r.resource !== "status" && r.kind !== "mutation" && r.purpose !== "action"
    // A read that failed before and answers now: its failure card goes.
    if (read && outcome === "ok" && lastError !== null && lastErrorResource === r.resource) {
      lastError = null
      lastErrorResource = ""
    }
    if (r.resource === "status") {
      applyStatus(outcome, env, tail)
    } else if (r.kind === "mutation" || r.purpose === "action") {
      var before = lastError
      applyAction(r, outcome, env, tail)
      if (lastError !== before) lastErrorResource = ""
    } else if (outcome !== "ok") {
      // A background read's failure stays out of the panel; the next status says how
      // the read went.
      if (r.priority === "background") console.warn("omaddons:", r.label + ":", outcome)
      else {
        lastError = errorOf(env, r.label + ": " + outcome)
        lastErrorResource = r.resource
        // A row waiting for its notes says they could not be read, never "Reading…".
        if (r.resource === "changelog") changelog = { "selection": r.selection, "data": null, "failed": lastError.message }
      }
    } else if (r.resource === "list") {
      lists = Model.reduceList(env)
      listsAt = Date.now()
    } else if (r.resource === "errors") {
      errors = Model.reduceErrors(env)
      errorsAt = Date.now()
    } else if (r.resource === "errors_raw") {
      rawErrors = { "selection": r.selection, "data": Model.reduceErrors(env) }
    } else if (r.resource === "backups") {
      backups = Model.reduceBackups(env)
      backupsAt = Date.now()
    } else if (r.resource === "backups_detail") {
      backupsDetail = { "selection": r.selection, "data": Model.reduceBackups(env) }
    } else if (r.resource === "search") {
      var found = Model.reduceSearch(env)
      found.selection = r.selection
      search = found
    } else if (r.resource === "changelog") {
      changelog = { "selection": r.selection, "data": env.data }
    } else if (r.resource === "check") {
      refresh()
    }
  }

  function applyStatus(outcome, env, tail) {
    if (outcome === "ok" || outcome === "failed") {
      var model = Model.reduceStatus(env, Date.now())
      bar = withSpinning(model.bar, action !== null)
      targets = model.targets
      summary = model.summary
      lastChange = model.lastChange
      widgetInfo = model.widget
      commands = model.commands
      statusWarnings = model.warnings
      machineIssues = model.issues
      phase = "ok"
      incompatibleText = ""
      unreachableText = ""
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
      if (outcome === "ok") statusApplied()
      return
    }
    // malformed, oversized, timeout or cancelled: no answer to show.
    phase = "unreachable"
    unreachableText = outcome === "malformed" ? (tail.length ? tail.join("\n") : "omaddons gave no answer") : "omaddons status: " + outcome
  }

  function applyAction(r, outcome, env, tail) {
    if (r.purpose === "preview") {
      if (outcome === "ok") {
        var path = r.cli ? Model.classifyCliArgv(r.cli).path : Model.templateShape(r.template, templates).path
        var binds = Model.CLI_PATHS[path].binds
        var token = env.data && typeof env.data.precondition === "string" ? env.data.precondition : ""
        confirm = { "text": Model.confirmText(r.template || path, env), "stale": r.stale }
        pendingConfirm = { "spec": r.spec, "token": token, "binds": binds, "contextGeneration": r.contextGeneration }
      } else {
        confirm = null
        pendingConfirm = null
        failedPreview = r.spec
        lastError = errorOf(env, r.label + ": " + outcome)
      }
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
      lastResult = { "ok": outcome === "ok", "line": Model.resultLine(env), "partial": !!env && env.partial === true, "commands": commandsOf,
        "fixes": Model.itemFixes(env) }
      lastError = outcome === "ok" ? null : errorOf(env, r.label + " failed")
      var stale = outcome === "failed" && env.error && env.error.code === "plan_stale"
      // A preview gone stale is previewed again, never rerun with the same token.
      if (stale && r.purpose === "confirmed" && r.spec) previewOf(r.spec, Model.staleText(env))
      if (stale && r.purpose === "bind") {
        lastError = { "message": Model.boundStaleText(r.template, env), "code": "plan_stale", "fixes": [], "fixText": "" }
      }
    } else {
      lastResult = { "ok": false, "line": r.label + ": " + outcome, "partial": false, "commands": [] }
      lastError = { "message": outcome === "malformed" && tail.length ? tail.join("\n") : r.label + ": " + outcome, "code": outcome, "fixes": [] }
    }
    // A change asked in the panel that finished after every panel closed: its result
    // line as a toast; a click opens the panel.
    if (r.kind === "mutation" && r.priority !== "background" && !panelOpen && canAct) {
      Quickshell.execDetached(Model.resultToastArgv(lastResult.line, Build.PLUGIN_ID))
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

  // Another omaddons build answered (M3-D42): keep the last numbers, stale; clear the
  // confirmation; drop every queued mutation; refuse new ones until a compatible
  // status. Retry and Reinstall widget stay available.
  function enterIncompatible(env) {
    phase = "incompatible"
    incompatibleText = Model.incompatibleText(env, Build)
    bar = Model.staleBar(bar)
    confirm = null
    pendingConfirm = null
    pendingDismissals = []
    queues = { "mutation": [], "local": queues.local.slice(), "network": queues.network.slice() }
    queuedAction = ""
  }

  // ------------------------------------------------------------- desktop triggers (F8)

  // Each of these only adds requests to the scheduler above, and every one waits for a
  // compatible answer (M3-D42).

  // Hyprland's window events ring like the doorbell (M3-D13): a client starting or
  // exiting shows within a second. A Wine game outlives its window, so a close while a
  // client ran is followed by refreshes at each of followUpDelaysMs.
  function windowEvent(name) {
    if (name !== "openwindow" && name !== "closewindow") return
    windowDebounce.restart()
    if (name === "closewindow" && anyRunning) {
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
    for (var i = 0; i < checkpoints.length; i++) {
      run("backup_auto", { "target": checkpoints[i] }, first ? "Settings checkpoint at widget start" : "Settings checkpoint after play")
    }
    var reads = stopped.slice()
    var stale = Model.errorsToRefresh(targets)
    for (var j = 0; j < stale.length; j++) if (reads.indexOf(stale[j]) === -1) reads.push(stale[j])
    for (var k = 0; k < reads.length; k++) readErrorsOf(reads[k])
    var updatesNow = summary ? summary.updates.available : 0
    var errorsNow = summary && summary.errors ? summary.errors["new"] : 0
    var before = first ? { "updates": 0, "errorsNew": 0 } : observed
    // Toasts are argv (M3-D14), and the error one sees only names, counts and clients
    // (M3-D21).
    if (Model.rose(before.errorsNew, errorsNow, first) && switchOn("notifyErrors") && canAct) {
      Quickshell.execDetached(Model.errorToastArgv(Model.errorFacts(targets), Build.PLUGIN_ID))
    }
    if (Model.rose(before.updates, updatesNow, first) && switchOn("notifyUpdates") && canAct) {
      Quickshell.execDetached(Model.updateToastArgv(updatesNow, summary.updates.targets, Build.PLUGIN_ID))
    }
    observed = { "generation": contextGeneration, "targets": targets, "updates": updatesNow, "errorsNew": errorsNow }
    autoCheck()
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

  // ------------------------------------------------------------- reinstall and show

  // `omaddons omarchy install` detached, under the stamped context: the reload it causes
  // destroys this service and its children.
  function reinstallWidget() {
    reinstallProc.command = [root.binary, "--actor", "widget", "--reason", "reinstall from the bar widget", "omarchy", "install"]
    reinstallProc.environment = Build.CONTEXT
    reinstallProc.startDetached()
    return "ok"
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
      "lastResult": lastResult ? { "ok": lastResult.ok, "line": lastResult.line } : null,
      "panelOpen": panelOpen,
      "binary": root.binary,
      "toolVersion": Build.TOOL_VERSION,
      "widgetVersion": manifest && manifest.version ? String(manifest.version) : "",
      "contextGeneration": contextGeneration,
      "lanes": { "mutation": laneState(mutationLane, "mutation"), "local": laneState(localLane, "local"), "network": laneState(networkLane, "network") },
      "lastRefreshMs": lastRefreshMs
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

  // Window events, with no class filter: a Proton game window's class names its
  // launcher, not the game (M3-D13).
  Connections {
    target: Hyprland
    function onRawEvent(event) { root.windowEvent(String(event && event.name ? event.name : "")) }
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

  Process {
    id: reinstallProc
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
  // SIGTERM then SIGKILL, the lane held until the child really exits.
  component Lane: Item {
    id: lane

    // The service: `laneFinished` and `laneProgress` are called on it directly.
    property var sink: null
    property int deadlineMs: 0
    property bool inactivity: false
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

    function run(r, argv, env) {
      started = false
      exitedSeen = false
      streamSeen = false
      streamGraceOver = false
      exitCode = -1
      timedOut = false
      cancelled = false
      tail = []
      request = r
      proc = processComponent.createObject(lane, { "command": argv, "environment": env })
      if (deadlineMs > 0) deadline.restart()
      proc.running = true
    }

    function cancel() {
      if (!proc || cancelled) return
      cancelled = true
      terminate()
    }

    // SIGTERM to the pid (what `running = false` sends, F0), then SIGKILL after the
    // grace period; never the group, which is the shell's.
    function terminate() {
      if (!proc) return
      proc.signal(15)
      killTimer.restart()
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
      if (inactivity && deadlineMs > 0 && !timedOut && !cancelled) deadline.restart()
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
      streamGrace.stop()
      var text = proc.stdout && typeof proc.stdout.text === "string" ? proc.stdout.text : ""
      var outcome = forced || Model.outcomeOf(exitCode, started, text, timedOut, cancelled, Build)
      var done = request
      var shown = tail
      var old = proc
      request = null
      proc = null
      old.destroy()
      sink.laneFinished(done, outcome, text, shown)
    }

    Timer {
      id: deadline
      interval: lane.deadlineMs
      onTriggered: {
        lane.timedOut = true
        lane.terminate()
      }
    }

    Timer {
      id: killTimer
      interval: lane.killGraceMs
      onTriggered: if (lane.proc) lane.proc.signal(9)
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
        onStarted: if (lane.proc === child) lane.started = true
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
