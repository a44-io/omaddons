import QtQuick
import QtQuick.Controls
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "views"
import "ui"
import "Model.js" as Model
import "Build.js" as Build

// The panel frame (design doc §2.1, F4), top to bottom: the hero (the scope as title,
// its glyph in the state colour, the meta line urgent first, and help, zoom, Check now,
// Undo last and Open AddOns), the scope Dropdown, the tab strip with each view's
// count, the content (the view, or the page on top of the navigation stack, alone in a
// Flickable), and the pinned bottom area outside it (progress, the last result with
// its Undo and Fix, the failure card, the freshness footer), which a long list never
// scrolls away (UX-02). Over it: the plan sheet every previewed action goes through
// (ui/PlanSheet.qml), the restore chooser and the key reference (`?`). `z` zooms the panel to the
// screen and back; Ctrl+±/0 and Ctrl+wheel scale the text (§5.6); both are kept in
// memory only.
//
// Keyboard first: one dispatcher looks every key up in Model.KEYMAP (design doc §3)
// and maps its action to a service call, the frame, or the current view or page. It
// decides nothing: labels come from Model.js over the CLI's own output, every action is
// a service call, and every CLI string renders as plain text (PlainLabel, or qs.Ui's
// own plain-text labels).
Item {
  id: root

  // The shared service, the bar's colors and font, and whether the panel is open.
  property var svc: null
  property color foreground: Commons.Color.foreground
  property color urgent: Commons.Color.urgent
  property color accent: Commons.Color.accent
  property string fontFamily: Style.font.family
  property bool opened: false

  signal closeRequested()
  // Tab and Shift-Tab: the neighbouring bar panel (Omarchy's, never intercepted).
  signal tabRequested(int direction)

  // Secondary text that reads on every theme, light ones included (§5.1, UX-44).
  readonly property color dim: Model.dimColor(foreground, Commons.Color.popups.background)

  // ---- view state, in memory only (M3-D3)
  property string view: "updates"
  // "" is All; otherwise a target id. View-bound requests carry it as their selection.
  property string filter: ""
  property int cursor: 0
  property bool cursorActive: false
  property double nowMs: Date.now()
  // A refusal from the service ("omaddons is not ready", a template it would not
  // build), until the next action.
  property string notice: ""
  // Why the last key did nothing here ("Nothing to pin here"), until the next key.
  property string keyNote: ""
  // The key reference (`?`) is open over everything but a dialog.
  property bool helpOpen: false
  // The views opened once: each is created on its first visit and kept.
  property var visitedViews: []
  // The navigation stack (§2.4): pages pushed on the view, each `{title, component,
  // props}`. The top one is the content; Esc, `h` and the back button pop it.
  property var pages: []
  // `z`: the panel fills the screen (BarWidget reads it); `uiScale`: the text zoom.
  property bool zoomed: false
  property real uiScale: 1.0

  // The views by name: F5 adds updates and installed, F6 issues and search, F9 backups.
  // A view without one leaves its slot empty; the switcher and the key map are final.
  // A view (and a page) may offer `frame` (the frame's own views have it from creation;
  // another's and a page get it on load), `rowCount`, `rowItem(i)`,
  // `typing`, `focusSearch()`, `escapeFilter()` (true when it took Esc: a filter it cleared)
  // and `handleKey(action, cursor)`, which gets KEYMAP's action names.
  property var viewComponents: ({ "updates": updatesView, "installed": installedView, "errors": errorsView, "issues": issuesView,
    "backups": backupsView, "search": searchView, "history": historyView })
  // Frame-wide view actions a later task registers (backup, dismiss, …): action →
  // function(frame).
  property var keyHandlers: ({
    "dismiss": function(frame) { return frame.dismissErrors() },
    "backup": function(frame) { return frame.backUp() },
    // Take over the folders already on a client (1n2p.32): `adopt --apply`, previewed
    // first, confirmed through the plan sheet.
    "adopt": function(frame, target) { return frame.takeOver(target) }
  })

  // Bumped when a view's loader loads, so `currentView` follows it.
  property int loadTick: 0
  readonly property var currentView: {
    var tick = loadTick
    var loader = viewLoaders.count > 0 ? viewLoaders.itemAt(Model.VIEWS.indexOf(view)) : null
    return loader ? loader.item : null
  }
  readonly property var topPage: pages.length > 0 ? pages[pages.length - 1] : null
  // What the keys act on: the page on top of the stack, else the view.
  readonly property var current: topPage ? pageLoader.item : currentView
  readonly property int rowCount: current && typeof current.rowCount === "number" ? current.rowCount : 0
  // A text field of the view has the keyboard: the key catcher lets it type.
  readonly property bool typing: !!current && current.typing === true

  readonly property string phase: svc ? String(svc.phase) : "starting"
  // Another omaddons build (M3-D42): read-only, only Retry and Reinstall act.
  readonly property bool readOnly: phase === "incompatible"
  readonly property bool canAct: !!svc && svc.canAct === true
  readonly property bool busy: !!svc && !!svc.action
  readonly property var summary: svc ? svc.summary : null
  readonly property var targets: svc && Array.isArray(svc.targets) ? svc.targets : []
  readonly property var machineIssues: svc && Array.isArray(svc.machineIssues) ? svc.machineIssues : []
  readonly property var selectedTarget: Model.targetById(targets, filter)
  readonly property var lastChange: svc ? svc.lastChange : null
  readonly property var undo: Model.undoAction(lastChange)
  // `o`: the scoped client's AddOns, or under All the cursored row's (design doc §2.2).
  readonly property var openArgv: Model.addonsDirArgv(Model.openTarget(targets, filter, cursorRowTarget()))
  readonly property var lastResult: svc ? svc.lastResult : null
  readonly property var resultUndo: Model.resultUndo(lastResult, lastChange)
  // What runs that the player waits on, in words (the activity bar, 1n2p.18): the
  // service's activity at activityNowMs, which ticks every second while one shows.
  property double activityNowMs: Date.now()
  readonly property var serviceActivity: svc ? svc.activity : null
  readonly property var activityView: serviceActivity ? Model.activityView(serviceActivity, targets, activityNowMs) : null
  readonly property bool activityShown: activityView !== null
  // The last result shows until the next activity, for the service's hold (§2.1).
  readonly property bool resultFresh: !!lastResult && !activityShown && !!svc && svc.resultShown !== false
  readonly property var lastError: svc ? svc.lastError : null
  readonly property var confirm: svc ? svc.confirm : null
  readonly property bool confirming: !!confirm
  // Without a compatible answer there are no views to act in: the card's buttons take
  // the cursor (j/k) and Enter.
  readonly property bool cardMode: phase === "missing" || phase === "unreachable" || phase === "incompatible"
  // The card in words (Model.phaseCard, design doc §6.4, 1n2p.34): the service's, else
  // the phase's own with no facts.
  readonly property var card: !cardMode ? null : (svc && svc.phaseCard ? svc.phaseCard : Model.phaseCard(phase, {}))
  readonly property var cardActions: card ? card.actions : []
  // What a card's Copy said a moment ago ("Copied"), "" otherwise.
  property string cardNote: ""
  // An interrupted change `status` reports (Model.interruptedChange), on a live answer.
  readonly property var interrupted: svc && phase === "ok" && svc.interrupted ? svc.interrupted : null
  // The first run's card above the views (Model.onboardingCard, 1n2p.32): one at a time,
  // never beside an interrupted change or over a page.
  readonly property var onboarding: !svc || phase !== "ok" || !!interrupted || !!topPage ? null : Model.onboardingCard({
    "summary": svc.summary, "targets": targets, "errors": svc.errors || null,
    "dismissed": svc.onboardingDismissed || {}, "keyHint": Model.boolSetting(svc.settings, "keyHint", true),
    "openedByMouse": svc.openedByMouse === true, "pluginId": Build.PLUGIN_ID })
  // What the onboarding card's Copy said a moment ago.
  property string onboardingNote: ""

  // A widget setting the panel changes (the keybinding tip seen): the bar copy saves it
  // through Omarchy's shell, so the widget writes no file.
  signal settingRequested(string key, var value)
  // Where a key is looked up in KEYMAP: the card's rows, a page's, or the view's.
  readonly property string keyContext: cardMode ? "card" : (topPage ? "page" : view)
  // Where the keyboard goes: the dialog's own keys while it is open, then the chooser's,
  // then the key reference's.
  readonly property Item focusItem: confirming ? planSheet.keyItem
    : (chooser.opened ? chooser.keyItem : (helpOpen ? help.keyItem : keyCatcher))
  // The frame's own shortcuts (Ctrl+Z, PgUp, PgDn) fire only where the catcher would
  // take keys: never under a dialog, the reference, or a field that is typing. The text
  // zoom works everywhere while the panel is open (design doc §3.2).
  readonly property bool shortcutsOn: opened && !typing && !confirming && !chooser.opened && !helpOpen

  // Below 480 px the tabs are glyphs, the meta line one part, and the detail goes
  // (design doc §2.1).
  readonly property bool narrow: canvas.width < Style.space(480)
  readonly property string heroState: Model.heroState(summary, targets, filter, machineIssues, busy)
  readonly property var tabCounts: Model.viewCounts(targets, filter, svc && Array.isArray(svc.commands) ? svc.commands : [],
    machineIssues)
  readonly property var tabViews: Model.VIEWS.map(function(v, i) {
    var counted = tabCounts[v] || { "count": 0, "urgent": false }
    return { "id": v, "title": Model.VIEW_TITLES[v], "key": String(i + 1), "glyph": Model.VIEW_GLYPHS[v],
      "count": counted.count, "urgent": counted.urgent }
  })

  // What the panel needs to show everything, in screen pixels (the text zoom scales
  // it): the top, the content and the pinned bottom area, or the dialog, chooser or key
  // reference over them.
  readonly property real frameImplicitHeight: top.implicitHeight + contentArea.implicitHeight + bottom.implicitHeight
    + gap * 2
  // The plan sheet asks for its whole plan (the bar caps the panel; past that it scrolls).
  readonly property real contentImplicitHeight: (confirming || chooser.opened || helpOpen
    ? Math.max(frameImplicitHeight, confirming ? Math.max(Style.space(220), planSheet.wantedHeight)
      : Style.space(helpOpen ? 560 : 360))
    : frameImplicitHeight) * uiScale
  readonly property real gap: Style.space(12)
  // The most height the panel may take (the bar's cap, which BarWidget hands down), and
  // so the most room the content has, in the canvas's units: a long list sizes itself
  // from these, never from the panel's own height, which follows the content
  // (1n2p.38: no binding loop through the panel).
  property real heightCap: Style.space(720)
  readonly property real contentRoom: Math.max(Style.space(160), heightCap / uiScale - top.implicitHeight - bottom.implicitHeight - gap * 2)
  // Where the views start in the content (below the cards and notices above them).
  readonly property real viewTop: content.y

  // For IPC's state() (design doc §2.1): the scope, view, page depth, open sheet and
  // zoom, set on the service while this panel is open.
  readonly property var panelState: ({
    "scope": filter, "view": view, "stackDepth": pages.length,
    "sheet": confirming ? "confirm" : (chooser.opened ? "restore" : (helpOpen ? "help" : null)),
    "zoomed": zoomed, "textZoom": uiScale
  })

  function alpha(c, a) { return Qt.rgba(c.r, c.g, c.b, a) }

  // ------------------------------------------------------------- state changes

  onOpenedChanged: {
    helpOpen = false
    if (!opened) return
    nowMs = Date.now()
    // The scope it opens on (§2.2): the one running client, else the scope the panel
    // showed last in this shell session (the service keeps it for every bar's panel;
    // a service without it, this frame's own), else All.
    setFilter(Model.openingScope(targets, svc && "scope" in svc ? svc.scope : filter))
    cursorActive = false
    notice = ""
    keyNote = ""
    flick.contentY = 0
    publishState()
    if (!takeRequestedView() && svc) svc.readView(view)
  }

  // A client forgotten or renamed since: the scope falls back to All (§2.2). Only on an
  // answer: a status that failed says nothing about which clients exist.
  onTargetsChanged: {
    var kept = Model.keptScope(targets, filter)
    if (summary !== null && kept !== filter) setFilter(kept)
  }

  onConfirmingChanged: {
    // A new preview starts on Cancel: Enter alone never confirms a destructive change.
    planSheet.reset()
    // A sheet is the top layer: the reference gives way to it.
    if (confirming) helpOpen = false
    Qt.callLater(root.focusNow)
  }
  onConfirmChanged: planSheet.reset()
  // A new activity, or news of one, reads the clock at once, not at the next tick.
  onServiceActivityChanged: activityNowMs = Date.now()
  onTypingChanged: if (!typing) Qt.callLater(root.focusNow)
  onHelpOpenChanged: Qt.callLater(root.focusNow)
  onPanelStateChanged: publishState()

  function focusNow() {
    if (root.opened && !root.typing) root.focusItem.forceActiveFocus()
  }

  // The open panel tells the service what it shows (IPC's state()); a service without
  // the property (a test's stub) is left alone.
  function publishState() {
    if (opened && svc && "panelState" in svc) svc.panelState = panelState
  }

  // The view the service was asked to show (middle click, `show <view>` over IPC).
  function takeRequestedView() {
    if (!svc || Model.VIEWS.indexOf(String(svc.requestedView)) < 0) return false
    var requested = String(svc.requestedView)
    svc.requestedView = ""
    showView(requested)
    return true
  }

  Connections {
    target: root.svc
    ignoreUnknownSignals: true
    function onRequestedViewChanged() { if (root.opened) root.takeRequestedView() }
  }

  // "checked N min ago" stays true while the panel sits open.
  Timer {
    interval: 30000
    repeat: true
    running: root.opened
    onTriggered: root.nowMs = Date.now()
  }

  // The activity bar's elapsed time and its "still working" line follow the clock.
  Timer {
    objectName: "activityClock"
    interval: 1000
    repeat: true
    triggeredOnStart: true
    running: root.opened && !!root.svc && !!root.svc.activity
    onTriggered: root.activityNowMs = Date.now()
  }

  // A view (a tab): the stack is cleared, and the view is created on its first visit
  // and kept (the one it leaves too, which may be the one the panel opened on).
  function showView(name) {
    var visited = visitedViews.slice()
    for (var v of [view, name]) if (visited.indexOf(v) < 0) visited.push(v)
    visitedViews = visited
    view = name
    pages = []
    keyNote = ""
    cursor = 0
    flick.contentY = 0
    if (svc) svc.readView(name)
  }

  // The scope, which the service remembers for the next panel that opens (in memory
  // only, M3-D3: it survives closing the panel, not a shell restart).
  function setFilter(id) {
    filter = String(id || "")
    cursor = 0
    if (svc && "scope" in svc) svc.scope = filter
  }

  // The client of the row under the cursor, "" when no row is: what a single-client
  // action (`o`) acts on under All, so it never guesses (§2.2).
  function cursorRowTarget() {
    if (!cursorActive || !current || !current.rows || typeof current.rows.length !== "number") return ""
    var row = cursor >= 0 && cursor < current.rows.length ? current.rows[cursor] : null
    return row && typeof row.target === "string" ? row.target : ""
  }

  // ------------------------------------------------------------- the navigation stack

  // A page on top of the view (or of the page before it): `{title, component, props}`.
  // The page remembers where the cursor and the scroll stood beneath it, so back returns
  // to the same row (design doc §2.4).
  function push(page) {
    if (!page || !page.component) return false
    var entry = Object.assign({}, page, { "under": { "cursor": cursor, "active": cursorActive, "y": flick.contentY } })
    pages = pages.concat([entry])
    cursor = 0
    cursorActive = false
    flick.contentY = 0
    return true
  }

  // A page over a page of the same kind (an addon page over another, through a
  // dependency, and back again) keeps the loader's item, which gets the new top page's
  // props; a page of another kind is made again by the loader, which hands it its own.
  onTopPageChanged: applyPageProps()
  function applyPageProps() {
    var item = pageLoader.item
    if (!item || !topPage || topPage.component !== pageLoader.loadedFor) return
    var props = topPage.props || {}
    for (var key in props) if (key in item) item[key] = props[key]
  }

  // An addon's page (1n2p.23, design doc §2.4), from any list or another page: `name`
  // as the row names it, and its `ref` when the row knows one.
  function openAddon(name, ref) {
    if (!name) return false
    return push({ "title": String(name), "component": addonPageComponent,
      "props": { "name": String(name), "ref": String(ref || ""), "link": "" } })
  }

  // A change's page (1n2p.30, design doc §2.4): Enter on a History row, by the run's key
  // (the page reads it from History's data, so a doorbell's re-read keeps it current).
  function openChange(key, title) {
    if (!key) return false
    return push({ "title": String(title || Model.VIEW_TITLES.history), "component": changePageComponent,
      "props": { "changeKey": String(key) } })
  }

  // The page of a ref or link pasted into Search (1n2p.25): read by that text, which
  // names nothing else; the read's answer names the addon (Model.addonPage).
  function openLink(text) {
    if (!text) return false
    return push({ "title": Model.copy("search.link.page"), "component": addonPageComponent,
      "props": { "name": "", "ref": "", "link": String(text) } })
  }

  // Back one page, to where the cursor stood beneath it; false when no page is open.
  function pop() {
    if (pages.length === 0) return false
    var under = pages[pages.length - 1].under || { "cursor": 0, "active": false, "y": 0 }
    pages = pages.slice(0, -1)
    cursor = under.cursor
    cursorActive = under.active
    flick.contentY = under.y
    return true
  }

  // Where back goes (§2.4): the view's title under one page, else the trail of the
  // pages beneath ("Updates › DBM").
  function backText() {
    if (pages.length === 0) return ""
    var trail = [String(Model.VIEW_TITLES[view] || "")]
    for (var i = 0; i < pages.length - 1; i++) trail.push(String(pages[i].title || ""))
    return pages.length === 1 ? trail[0] : trail.join(" › ")
  }

  // Esc where the catcher has the keys (a dialog, a chooser and the reference take
  // their own): back one layer (§2.4), a page, then a filter the view clears, then the
  // panel closes.
  function backOut() {
    keyNote = ""
    if (pop()) return true
    if (current && typeof current.escapeFilter === "function" && current.escapeFilter() === true) return true
    closeRequested()
    return true
  }

  // ------------------------------------------------------------- the key map

  // One function per PanelKeyCatcher signal. The catcher moves the cursor (j/k) and
  // steps views (h/l) itself; Enter, Space, x and every typed key go through act, which
  // looks them up in Model.KEYMAP.
  function move(dx, dy) {
    keyNote = ""
    if (dx !== 0) {
      // On a page, h goes back and l does nothing (§3.2).
      if (topPage) {
        if (dx < 0) pop()
        return
      }
      showView(Model.stepView(view, dx))
      return
    }
    moveCursor(cursor + dy)
  }

  // The cursor to row `to` (the card's button in a problem card), kept in range.
  function moveCursor(to) {
    var count = cardMode ? cardActions.length : rowCount
    cursorActive = true
    cursor = count > 0 ? Math.max(0, Math.min(count - 1, to)) : 0
    ensureCursorVisible()
  }

  // How many rows a page key moves: as many of the cursor's row as the content shows.
  function pageRows() {
    var item = current && typeof current.rowItem === "function" ? current.rowItem(cursor) : null
    var rowHeight = item && item.height > 0 ? item.height : Style.space(32)
    return Math.max(1, Math.floor(flick.height / rowHeight) - 1)
  }

  // Enter and Space: the card's button, or the row's primary action (the view's).
  function activate() {
    return act("Enter")
  }

  function deleteKey() {
    return act("x")
  }

  function textKey(text) {
    return act(text)
  }

  // The text zoom: one step bigger (1) or smaller (-1), or back to normal (0), between
  // 0.6 and 2 (§5.6).
  function bumpUiScale(step) {
    uiScale = step === 0 ? 1.0 : Math.max(0.6, Math.min(2.0, Math.round((uiScale + step * 0.1) * 10) / 10))
  }

  // The key `name` (Model.keyName's) where the panel stands: its KEYMAP action run, its
  // note shown when it acts only in other views, or nothing.
  function act(name) {
    var key = Model.panelKey(name, keyContext)
    keyNote = ""
    if (!key) return false
    if (key.action === null) {
      keyNote = String(key.note || "")
      return true
    }
    var action = key.action
    if (action === "view") {
      showView(key.view)
      return true
    }
    if (action === "help") {
      helpOpen = true
      return true
    }
    if (action === "zoom") {
      zoomed = !zoomed
      return true
    }
    if (action === "text_zoom") {
      bumpUiScale(key.step)
      return true
    }
    if (action === "press") return runCard(cardActions[Math.min(cursor, cardActions.length - 1)])
    if (action === "client") {
      setFilter(Model.stepFilter(targets, filter, key.step))
      return true
    }
    if (action === "search") {
      showView("search")
      if (current && typeof current.focusSearch === "function") current.focusSearch()
      return true
    }
    if (action === "edge") {
      moveCursor(key.step < 0 ? 0 : rowCount - 1)
      return true
    }
    if (action === "page") {
      moveCursor(cursor + key.step * pageRows())
      return true
    }
    // Every other action waits for a compatible answer (M3-D42).
    if (cardMode || !canAct) return false
    if (action === "update_all") return updateAll()
    if (action === "check") return checkNow()
    // History and a change's page undo their own change (design doc §3.2: "History: the
    // cursored change"); every other view, the last change.
    if (action === "undo") {
      if (current && typeof current.handleKey === "function" && current.handleKey("undo", cursor) === true) return true
      return undoLast()
    }
    if (action === "open") return openAddOns()
    return routeKey(action)
  }

  // An action the current view (or page) or a later task implements; an unregistered
  // one does nothing (no key exists before its action does).
  function routeKey(name) {
    if (current && typeof current.handleKey === "function" && current.handleKey(name, cursor) === true) return true
    var handler = keyHandlers[name]
    if (typeof handler !== "function") return false
    handler(root)
    return true
  }

  // The plan sheet's keys (it has the keyboard while open), as KEYMAP's sheet rows say:
  // the sheet's own. A held key that started before the preview appeared reaches it only
  // as auto-repeat, which never confirms: the verb needs a fresh press (M3-D46). Every
  // other key is the sheet's too: nothing behind it acts.
  function confirmKey(key, text, autoRepeat, modifiers) {
    return confirming ? planSheet.handleKey(key, text, autoRepeat, modifiers) : false
  }

  // ------------------------------------------------------------- actions

  function report(refusal) {
    notice = refusal ? String(refusal) : ""
    return refusal === ""
  }

  function updateAll() {
    if (busy) return false
    var request = Model.updateRequest(filter, Model.updateRows(svc.lists, filter, targets), targets)
    return report(svc.run(request.template, request.slots, request.label, request.shown))
  }

  // A check the full network lane could not take says so, never only on the console
  // (UX-42).
  function checkNow() {
    if (svc.check() === false) notice = Model.copy("activity.lane_full")
    return true
  }

  // The last change's undo; when the cached status cannot tell which change is last
  // (never a guessed run, M3-D37), History, where each change has its own undo.
  function undoLast() {
    if (undo.history === true) {
      showView("history")
      return true
    }
    if (busy || !undo.argv) return false
    return report(svc.runCliArgv(undo.argv, undo.label))
  }

  function openAddOns() {
    if (!openArgv) {
      // Under All with no row under the cursor: say how to choose one, never guess.
      if (!filter) keyNote = Model.copy("client.choose")
      return false
    }
    Quickshell.execDetached(Model.addonsDirArgv(Model.openTarget(targets, filter, cursorRowTarget())))
    return true
  }

  function runCli(command, label) {
    if (busy || !command || !command.argv) return false
    return report(svc.runCliArgv(command.argv, label))
  }

  // `d` (and the Issues view's Dismiss): the filtered client's new errors, or each
  // client's, bound to the errors read the panel shows.
  function dismissErrors() {
    if (busy || !svc) return false
    return report(svc.dismissErrors(Model.dismissPlan(svc.errors, filter)))
  }

  // `b` (and the Backups view's button): the filtered client, or every client.
  function backUp() {
    if (busy || !svc) return false
    var request = Model.backupRequest(filter, targets)
    return report(svc.run(request.template, request.slots, request.label))
  }

  // `r` on an addon row, or Enter on a backup: the restore chooser
  // (`{target, addon}` or `{target, backup}`).
  function openChooser(spec) {
    if (!canAct || busy || !spec || !spec.target) return false
    chooser.open(spec)
    return true
  }

  function chooserClosed() {
    Qt.callLater(root.focusNow)
  }

  // A card's action: Retry asks again, Update the panel reinstalls the widget, and the
  // two copies put a command or the details on the clipboard (never run).
  function runCard(action) {
    if (!svc) return false
    if (action === "retry") svc.refresh()
    else if (action === "reinstall") svc.reinstallWidget()
    else if (action === "copy_command" || action === "copy_details") {
      var text = action === "copy_command" ? Model.missingBinaryCommand() : (card ? card.details : "")
      if (!svc.copyCommand(text)) return false
      cardNote = Model.copy("card.copied")
      cardNoteTimer.restart()
    } else return false
    return true
  }

  // A client's take-over (1n2p.32): the folders already there brought under management
  // as they are. `adopt --apply -t <client>` runs as its preview, and the plan sheet
  // lists what it would adopt before anything is recorded.
  function takeOver(target) {
    if (!svc || !canAct || busy || typeof target !== "string" || target === "") return false
    var client = Model.sheetClient(targets, target, "")
    return report(svc.run("adopt_target", { "target": target }, Model.copy("installed.take_over.label", { "client": client })))
  }

  // The first run's card (1n2p.32): its one action, or Not now (for the shell's
  // lifetime; the keybinding tip, once seen, turns its setting Off for good).
  function runOnboarding(name) {
    var card = onboarding
    if (!svc || !card) return false
    if (name === "take_over") return takeOver(card.target)
    if (name === "install") return !!card.action && runCli(card.action, Model.copy("errors.capture.install"))
    if (name === "copy") {
      if (!svc.copyCommand(card.line)) return false
      onboardingNote = Model.copy("card.copied")
      cardNoteTimer.restart()
      settingRequested("keyHint", "Off")
      return true
    }
    if (name !== "not_now") return false
    if (card.kind === "keyhint") settingRequested("keyHint", "Off")
    svc.dismissOnboarding(card.kind, card.target)
    return true
  }

  // The failure card's Copy details.
  function copyErrorDetails() {
    if (!svc || !lastError || !svc.copyCommand(String(lastError.details || ""))) return false
    cardNote = Model.copy("card.copied")
    cardNoteTimer.restart()
    return true
  }

  // The interrupted change's notice: its problem's page in Issues, whose repair previews
  // first (the widget never finishes a change by itself, 1n2p.34).
  function showInterrupted() {
    if (!interrupted) return false
    if (interrupted.target !== "") setFilter(interrupted.target)
    showView("issues")
    return true
  }

  // The welcome card's page (1n2p.32).
  Component {
    id: addClientComponent
    AddClientPage {}
  }

  Timer {
    id: cardNoteTimer
    interval: 3000
    onTriggered: {
      root.cardNote = ""
      root.onboardingNote = ""
    }
  }

  // A view state's action (Model.viewState, design doc §7): Check now checks, Search opens
  // Search, Back up now backs up; Try again, Read again and Refresh read the view's data
  // again (a search asks again for its query).
  function stateAction(name) {
    if (!svc) return false
    if (name === "check") return checkNow()
    if (name === "search") {
      showView("search")
      return true
    }
    if (name === "backup") return backUp()
    // A missing client's card (§2.2): scan for the clients again, or forget this one
    // through the plain `targets rm` its own problem offers (the forget plan sheet).
    if (name === "rescan") return typeof svc.rescan === "function" && svc.rescan() !== false
    // The welcome card's Add a client folder (1n2p.32): a page with a path box.
    if (name === "add") return push({ "title": Model.copy("addclient.title"), "component": addClientComponent, "props": {} })
    if (name === "forget") {
      var forget = Model.forgetAction(selectedTarget)
      return !!forget && runCli(forget, "Forget " + Model.heroTitle(targets, filter))
    }
    if (name !== "retry" && name !== "read_errors" && name !== "refresh_backups") return false
    if (view === "search") return !!currentView && typeof currentView.searchAgain === "function" && currentView.searchAgain()
    return typeof svc.readAgain === "function" && svc.readAgain(view) !== false
  }

  function confirmYes() {
    if (svc) report(svc.confirmYes())
  }

  function confirmNo() {
    if (svc) svc.confirmNo()
  }

  // Keeps the cursor's row (or card button) inside the scrolled area: the key catcher
  // consumes j/k, so the Flickable never scrolls by itself (F0).
  function ensureCursorVisible() {
    // A long list scrolls itself (VirtualList, 1n2p.38): a row it has not built has no
    // item to map, so it positions the cursor's row itself.
    if (!cardMode && current && current.scrollsItself === true && typeof current.positionCursor === "function") {
      current.positionCursor()
      return
    }
    var item = null
    if (cardMode) item = cardButtons.itemAt(cursor)
    else if (current && typeof current.rowItem === "function") item = current.rowItem(cursor)
    if (!item) return
    var y = item.mapToItem(flick.contentItem, 0, 0).y
    var margin = Style.space(8)
    if (y < flick.contentY + margin)
      flick.contentY = Math.max(0, y - margin)
    else if (y + item.height > flick.contentY + flick.height - margin)
      flick.contentY = Math.min(Math.max(0, flick.contentHeight - flick.height), y + item.height - flick.height + margin)
  }

  // The service's (or a view's) text field gives the keyboard back.
  function leaveField() {
    keyCatcher.forceActiveFocus()
  }

  // ------------------------------------------------------------- the panel

  // The text zoom (§5.6): the frame draws on a logical canvas shrunk by `uiScale`, and
  // the transform grows it back, so every child lays out as at 1.0 and pointer input is
  // mapped through it (omabeads' zoom).
  Item {
    id: canvas
    objectName: "canvas"
    width: root.width / root.uiScale
    height: root.height / root.uiScale
    transform: Scale { xScale: root.uiScale; yScale: root.uiScale }

    PanelKeyCatcher {
      id: keyCatcher
      objectName: "keyCatcher"
      anchors.fill: parent
      blocked: root.typing || root.confirming || chooser.opened || root.helpOpen

      onMoveRequested: function(dx, dy) { root.move(dx, dy) }
      // Enter emits returnRequested then activateRequested (F0): only the latter acts.
      onActivateRequested: root.activate()
      onDeleteRequested: root.deleteKey()
      onCloseRequested: root.backOut()
      onTabRequested: function(direction) { root.tabRequested(direction) }
      onTextKey: function(text) { root.textKey(text) }

      // ---------- Top: the hero, the scope, the tabs ----------
      Column {
        id: top
        objectName: "top"
        width: parent.width
        spacing: root.gap

        PanelHero {
          id: hero
          objectName: "hero"
          width: parent.width
          title: Model.heroTitle(root.targets, root.filter)
          // Narrow: the meta line keeps its first part (§2.1).
          meta: {
            var full = Model.heroMeta(root.summary, root.targets, root.filter, root.machineIssues)
            return root.narrow ? full.split(" · ")[0] : full
          }
          // Read-only keeps the last numbers, marked stale.
          detail: root.readOnly ? "stale" : (root.narrow ? "" : Model.heroDetail(root.summary, root.targets, root.filter))
          metaOpacity: root.readOnly ? 0.5 : 1.0
          foreground: root.foreground
          fontFamily: root.fontFamily

          iconComponent: Component {
            Text {
              objectName: "heroGlyph"
              textFormat: Text.PlainText
              // U+F0297 nf-md-gamepad_variant, as on the bar, in the state's colour.
              text: String.fromCodePoint(0xF0297)
              color: root.heroState === "urgent" ? root.urgent : (root.heroState === "busy" ? root.accent : root.dim)
              font.family: root.fontFamily
              font.pixelSize: Style.font.display
            }
          }

          trailingControl: Component {
            Row {
              spacing: Style.spacing.sm

              PanelActionButton {
                objectName: "helpButton"
                // U+F0625 nf-md-help_circle_outline.
                iconText: String.fromCodePoint(0xF0625)
                tooltipText: Model.withKey("Keys", "help")
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.helpOpen = true
              }

              PanelActionButton {
                objectName: "zoomButton"
                // U+F0293 / U+F0294 nf-md-fullscreen / fullscreen_exit.
                iconText: String.fromCodePoint(root.zoomed ? 0xF0294 : 0xF0293)
                tooltipText: Model.withKey(root.zoomed ? "Back to panel size" : "Fill the screen", "zoom")
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.zoomed = !root.zoomed
              }

              PanelActionButton {
                id: checkButton
                objectName: "checkButton"
                // U+F04E6 nf-md-sync: it spins while a check runs (UX-01).
                iconText: String.fromCodePoint(0xF04E6)
                tooltipText: Model.withKey(spinning ? "Checking…" : "Check now", "check")
                readonly property bool spinning: !!root.svc && root.svc.checking === true
                enabled: root.canAct
                opacity: enabled ? 1.0 : 0.4
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.checkNow()

                RotationAnimation on rotation {
                  running: checkButton.spinning
                  from: 0
                  to: 360
                  duration: 1000
                  loops: Animation.Infinite
                  onRunningChanged: if (!running) checkButton.rotation = 0
                }
              }

              PanelActionButton {
                objectName: "undoButton"
                // U+F054C nf-md-undo.
                iconText: String.fromCodePoint(0xF054C)
                tooltipText: Model.withKey(root.undo.label, "undo")
                enabled: root.canAct && (root.undo.history === true || (!root.busy && root.undo.argv !== null))
                opacity: enabled ? 1.0 : 0.4
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.undoLast()
              }

              // Kept here until the rows carry their own actions (.15); `o` opens it.
              PanelActionButton {
                objectName: "openButton"
                // U+F0770 nf-md-folder_open.
                iconText: String.fromCodePoint(0xF0770)
                tooltipText: Model.withKey("Open AddOns", "open")
                enabled: root.canAct && root.openArgv !== null
                opacity: enabled ? 1.0 : 0.4
                foreground: root.foreground
                fontFamily: root.fontFamily
                onClicked: root.openAddOns()
              }
            }
          }
        }

        // ---------- The scope: All clients, or one (t/T) ----------
        Dropdown {
          id: scope
          objectName: "scope"
          // With no compatible answer, neither the scope nor the tabs show (§2.1).
          visible: !root.cardMode && root.targets.length > 0
          width: parent.width
          showLabel: false
          label: "Client"
          value: root.filter
          options: Model.scopeOptions(root.targets)
          foreground: root.foreground
          fontFamily: root.fontFamily
          onChanged: function(value) { root.setFilter(value) }
        }

        // ---------- The tabs (h/l, the digits) ----------
        TabStrip {
          id: switcher
          objectName: "switcher"
          visible: !root.cardMode
          width: parent.width
          narrow: root.narrow
          value: root.view
          views: root.tabViews
          foreground: root.foreground
          fontFamily: root.fontFamily
          onChanged: function(value) { root.showView(value) }
        }
      }

      // ---------- The content: the view or the page on top, alone in a Flickable ----------
      Flickable {
        id: flick
        objectName: "contentFlick"
        anchors.top: top.bottom
        anchors.topMargin: root.gap
        anchors.bottom: bottom.top
        anchors.bottomMargin: root.gap
        width: parent.width
        contentWidth: width
        contentHeight: contentArea.implicitHeight
        clip: true
        boundsBehavior: Flickable.StopAtBounds
        flickableDirection: Flickable.VerticalFlick
        interactive: contentHeight > height
        ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

        Column {
          id: contentArea
          objectName: "contentArea"
          width: flick.width
          spacing: root.gap

          // Narrow: the tabs are glyphs, so the content names its view (§2.1).
          PlainLabel {
            objectName: "viewTitle"
            visible: root.narrow && !root.cardMode && !root.topPage
            width: parent.width
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
            text: String(Model.VIEW_TITLES[root.view] || "")
          }

          // ---------- Cards: no answer to work with ----------
          Text {
            visible: root.phase === "starting"
            width: parent.width
            textFormat: Text.PlainText
            text: "Asking omaddons…"
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
          }

          BorderSurface {
            id: phaseCard
            objectName: "phaseCard"
            visible: root.cardMode
            width: parent.width
            implicitHeight: cardColumn.implicitHeight + Style.spacing.xl * 2
            color: root.alpha(root.urgent, 0.10)
            borderSpec: Border.flat(root.alpha(root.urgent, 0.35), 1)
            radius: Style.cornerRadius

            Column {
              id: cardColumn
              anchors.left: parent.left
              anchors.right: parent.right
              anchors.verticalCenter: parent.verticalCenter
              anchors.leftMargin: Style.space(12)
              anchors.rightMargin: Style.space(12)
              spacing: Style.spacing.md

              PlainLabel {
                objectName: "cardTitle"
                width: parent.width
                wrapMode: Text.WordWrap
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                font.bold: true
                text: root.card ? root.card.title : ""
              }

              // What happened and what to do, in words; the child's own output only
              // goes to Copy details (design doc §6.4).
              PlainLabel {
                objectName: "cardDetail"
                visible: text !== ""
                width: parent.width
                wrapMode: Text.WordWrap
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.bodySmall
                text: root.card ? root.card.sentence : ""
              }

              // That it tries again by itself, or when Update the panel last ran.
              Repeater {
                objectName: "cardNotes"
                model: root.card ? root.card.notes : []

                PlainLabel {
                  required property string modelData
                  width: cardColumn.width
                  wrapMode: Text.WordWrap
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  text: modelData
                }
              }

              Row {
                spacing: Style.spacing.md

                Repeater {
                  id: cardButtons
                  model: root.cardActions

                  Button {
                    required property string modelData
                    required property int index
                    objectName: "card_" + modelData
                    text: Model.copy("card.action." + modelData)
                    hasCursor: root.cursorActive && index === root.cursor
                    bordered: true
                    foreground: root.foreground
                    fontFamily: root.fontFamily
                    fontSize: Style.font.bodySmall
                    onClicked: root.runCard(modelData)
                  }
                }

                PlainLabel {
                  objectName: "cardCopied"
                  visible: text !== ""
                  anchors.verticalCenter: parent.verticalCenter
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  text: root.cardNote
                }
              }
            }
          }

          // ---------- The widget's own notices ----------
          // (No client on this computer is every view's welcome card: Model.viewState.)

          // A change a crash or a reload cut off (1n2p.34), from `status` alone: what
          // happened in words, and its problem's page, whose repair previews first. The
          // widget never finishes it by itself. (An outdated widget is the phase card's:
          // `status` reports it only with `widget.outdated`, which is another build.)
          NoticeCard {
            objectName: "interruptedCard"
            visible: !!root.interrupted && !root.topPage
            width: parent.width
            kind: "failure"
            title: root.interrupted ? root.interrupted.title : ""
            body: root.interrupted ? root.interrupted.sentence : ""
            buttons: [{ "name": "show", "text": Model.copy("interrupted.action") }]
            foreground: root.foreground
            urgent: root.urgent
            fontFamily: root.fontFamily
            onClicked: function(name) { if (name === "show") root.showInterrupted() }
          }

          // The first run's card (1n2p.32): take over, record Lua errors, or the key tip.
          NoticeCard {
            objectName: "onboardingCard"
            visible: !!root.onboarding
            width: parent.width
            kind: "info"
            title: root.onboarding ? root.onboarding.title : ""
            body: !root.onboarding ? "" : root.onboarding.line !== "" ? root.onboarding.sentence + "\n" + root.onboarding.line
              : root.onboarding.sentence
            buttons: root.onboarding ? root.onboarding.buttons : []
            foreground: root.foreground
            urgent: root.urgent
            fontFamily: root.fontFamily
            onClicked: function(name) { root.runOnboarding(name) }
          }

          PlainLabel {
            objectName: "onboardingCopied"
            visible: !!root.onboarding && text !== ""
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: root.onboardingNote
          }

          // ---------- A page: its back button, then the page itself ----------
          Button {
            objectName: "backButton"
            visible: !!root.topPage
            // U+F0141 nf-md-chevron_left: back names where it goes (§2.4).
            iconText: String.fromCodePoint(0xF0141)
            text: root.backText()
            tooltipText: Model.withKey("Back", "close")
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.bodySmall
            onClicked: root.pop()
          }

          Loader {
            id: pageLoader
            objectName: "page"
            visible: !!root.topPage
            width: parent.width
            active: !!root.topPage
            sourceComponent: root.topPage ? root.topPage.component : null
            // The component the item was made from: a page over a page of the same kind
            // keeps the item (applyPageProps).
            property var loadedFor: null
            onLoaded: {
              if (!item) return
              loadedFor = root.topPage ? root.topPage.component : null
              if ("frame" in item) item.frame = root
              var props = root.topPage && root.topPage.props ? root.topPage.props : {}
              for (var key in props) if (key in item) item[key] = props[key]
            }
          }

          // ---------- The views: each made on its first visit and kept ----------
          Column {
            id: content
            objectName: "content"
            // The view shown (tests and pages read it as `content.item`).
            readonly property var item: root.currentView
            visible: !root.topPage
            width: parent.width
            // Read-only: the rows stay visible, inert.
            enabled: !root.readOnly

            Repeater {
              id: viewLoaders
              model: Model.VIEWS

              Loader {
                required property string modelData
                objectName: "viewLoader_" + modelData
                width: content.width
                visible: root.view === modelData
                active: root.view === modelData || root.visitedViews.indexOf(modelData) >= 0
                sourceComponent: root.viewComponents[modelData] || null
                // The frame's own views have it already; another's (a test's) gets it here.
                onLoaded: if (item && "frame" in item && item.frame !== root) item.frame = root
                onItemChanged: root.loadTick++
              }
            }
          }
        }
      }

      // ---------- The pinned bottom area: progress, the result, the failure, the footer ----------
      // Outside the Flickable, so it never scrolls away (UX-02).
      Column {
        id: bottom
        objectName: "footer"
        anchors.bottom: parent.bottom
        width: parent.width
        spacing: Style.spacing.sm

        BorderSurface {
          objectName: "errorCard"
          // Like the result row: an earlier failure is not shown while another runs.
          visible: !!root.lastError && !root.cardMode && !root.busy
          width: parent.width
          implicitHeight: errorColumn.implicitHeight + Style.spacing.lg * 2
          color: root.alpha(root.urgent, 0.10)
          borderSpec: Border.flat(root.alpha(root.urgent, 0.35), 1)
          radius: Style.cornerRadius

          // A failure stays until it is dismissed, or until what failed succeeds (1n2p.18).
          PanelActionButton {
            id: errorDismiss
            objectName: "errorDismiss"
            visible: !!root.svc && typeof root.svc.dismissError === "function"
            anchors.right: parent.right
            anchors.top: parent.top
            anchors.margins: Style.spacing.sm
            // U+F0156 nf-md-close.
            iconText: String.fromCodePoint(0xF0156)
            tooltipText: Model.copy("activity.dismiss")
            foreground: root.foreground
            fontFamily: root.fontFamily
            Accessible.role: Accessible.Button
            Accessible.name: Model.copy("activity.dismiss")
            onClicked: root.svc.dismissError()
          }

          Column {
            id: errorColumn
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            anchors.leftMargin: Style.space(12)
            anchors.rightMargin: Style.space(12) + (errorDismiss.visible ? errorDismiss.width + Style.spacing.sm : 0)
            spacing: Style.spacing.sm

            PlainLabel {
              objectName: "errorMessage"
              width: parent.width
              wrapMode: Text.WordWrap
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: root.lastError ? String(root.lastError.message || "") : ""
            }

            // A fix the classifier accepts is a button; the first is the Fix, the
            // rest its alternatives.
            Flow {
              width: parent.width
              spacing: Style.spacing.sm

              Repeater {
                model: root.lastError && Array.isArray(root.lastError.fixes) ? root.lastError.fixes : []

                Button {
                  required property var modelData
                  required property int index
                  objectName: "fix_" + index
                  enabled: root.canAct && !root.busy
                  opacity: enabled ? 1.0 : 0.4
                  text: index === 0 ? "Fix: " + modelData.command : modelData.command
                  bordered: index === 0
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  fontSize: Style.font.caption
                  onClicked: root.runCli(modelData, index === 0 ? "Fix" : "Fix (alternative)")
                }
              }

              // A request that ended with no envelope: what ran and how it ended, for a
              // bug report (Model.failureDetails, 1n2p.34).
              Button {
                objectName: "copyDetails"
                visible: !!root.lastError && typeof root.lastError.details === "string" && root.lastError.details !== ""
                text: Model.copy("card.action.copy_details")
                bordered: true
                foreground: root.foreground
                fontFamily: root.fontFamily
                fontSize: Style.font.caption
                onClicked: root.copyErrorDetails()
              }

              PlainLabel {
                objectName: "errorCopied"
                visible: text !== ""
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                text: root.cardNote
              }
            }

            // A prose fix, or one the classifier refused: text.
            PlainLabel {
              objectName: "fixText"
              visible: text !== ""
              width: parent.width
              wrapMode: Text.WordWrap
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: root.lastError && root.lastError.fixText ? String(root.lastError.fixText) : ""
            }

            // A restore whose folders changed owner since the backup: one button per
            // folder, each the same restore's preview for that folder.
            Flow {
              width: parent.width
              spacing: Style.spacing.sm

              Repeater {
                model: root.lastError && Array.isArray(root.lastError.choices) ? root.lastError.choices : []

                Button {
                  required property var modelData
                  required property int index
                  objectName: "choice_" + index
                  enabled: root.canAct && !root.busy
                  opacity: enabled ? 1.0 : 0.4
                  text: "Restore " + modelData.label
                  bordered: true
                  foreground: root.foreground
                  fontFamily: root.fontFamily
                  fontSize: Style.font.caption
                  onClicked: root.report(root.svc.restoreFolder(modelData.addon))
                }
              }
            }

            // A restore that cannot exchange here: the backup's own plain folder.
            PlainLabel {
              objectName: "backupFolder"
              visible: text !== ""
              width: parent.width
              wrapMode: Text.WrapAnywhere
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: root.lastError && root.lastError.folder ? "The backup's own folder: " + root.lastError.folder : ""
            }
          }
        }

        // The activity bar (§2.1, 1n2p.18): a running change with its progress and the
        // queue, else a check or a read the player waits on; else the last result with
        // its Undo, for the service's hold.
        ActivityBar {
          objectName: "activityBar"
          width: parent.width
          view: root.activityView
          queued: root.svc && typeof root.svc.queuedText === "string" ? root.svc.queuedText : ""
          foreground: root.foreground
          dim: root.dim
          accent: root.accent
          fontFamily: root.fontFamily
          onCancelRequested: if (root.svc && typeof root.svc.cancelActivity === "function") root.svc.cancelActivity()
        }

        Row {
          objectName: "result"
          visible: root.resultFresh
          width: parent.width
          spacing: Style.spacing.md

          PlainLabel {
            objectName: "resultLine"
            anchors.verticalCenter: parent.verticalCenter
            width: Math.min(implicitWidth, parent.width - (resultUndoButton.visible ? resultUndoButton.width + parent.spacing : 0))
            color: root.lastResult && !root.lastResult.ok ? root.urgent : root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: root.lastResult ? String(root.lastResult.line || "") : ""
          }

          Button {
            id: resultUndoButton
            objectName: "resultUndo"
            visible: root.resultUndo !== null
            enabled: root.canAct && !root.busy
            opacity: enabled ? 1.0 : 0.4
            text: "Undo"
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.caption
            onClicked: root.runCli(root.resultUndo, "Undo")
          }
        }

        // A partial result: each failed item's fix the classifier accepts.
        Flow {
          visible: root.resultFresh
          width: parent.width
          spacing: Style.spacing.sm

          Repeater {
            model: root.lastResult && Array.isArray(root.lastResult.fixes) ? root.lastResult.fixes : []

            Button {
              required property var modelData
              required property int index
              objectName: "itemFix_" + index
              enabled: root.canAct && !root.busy
              opacity: enabled ? 1.0 : 0.4
              text: "Fix: " + modelData.command
              bordered: true
              foreground: root.foreground
              fontFamily: root.fontFamily
              fontSize: Style.font.caption
              onClicked: root.runCli(modelData, "Fix")
            }
          }
        }

        PlainLabel {
          objectName: "notice"
          visible: text !== ""
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.urgent
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: root.notice
        }

        // A key that does nothing here says so (design doc §3.3), until the next key.
        PlainLabel {
          objectName: "keyNote"
          visible: text !== ""
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: root.keyNote
        }

        // The freshness footer (§2.1, UX-23): the clients, the scope's oldest check,
        // who is running; it ticks while the panel is open.
        PlainLabel {
          objectName: "freshness"
          visible: text !== ""
          width: parent.width
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: Model.footerText(root.summary, root.targets, root.filter, root.nowMs)
        }
      }
    }

    // ---------- The plan sheet: the CLI's own dry run, readable (M3-D9, M3-D46, 1n2p.17) ----------
    PlanSheet {
      id: planSheet
      objectName: "planSheet"
      anchors.fill: parent
      z: 10
      opened: root.confirming
      plan: root.confirm && root.confirm.sheet ? root.confirm.sheet : null
      foreground: root.foreground
      fontFamily: root.fontFamily
      onConfirmed: root.confirmYes()
      onCanceled: root.confirmNo()
    }

    RestoreChooser {
      id: chooser
      objectName: "chooser"
      anchors.fill: parent
      z: 8
      frame: root
    }

    // ---------- The key reference (`?`): KEYMAP's rows where the panel stands ----------
    // Over the views, pages and problem cards, under a dialog. While it is open every key
    // is its own: `?` and Esc close it, j/k scroll it, and nothing beneath it acts.
    Sheet {
      id: help
      objectName: "help"
      anchors.fill: parent
      z: 9
      reference: true
      opened: root.helpOpen
      title: "Keys · " + (root.cardMode ? "Problem card"
        : (root.topPage ? String(root.topPage.title || "") : String(Model.VIEW_TITLES[root.view] || "")))
      foreground: root.foreground
      fontFamily: root.fontFamily
      onCanceled: root.helpOpen = false

      Repeater {
        objectName: "helpSections"
        model: root.helpOpen ? Model.helpSections(root.keyContext) : []

        Column {
          required property var modelData
          width: help.bodyWidth
          spacing: Style.spacing.xs
          bottomPadding: Style.spacing.md

          SectionHeader {
            width: parent.width
            title: modelData.title
            foreground: root.foreground
            fontFamily: root.fontFamily
          }

          Repeater {
            model: modelData.rows

            Row {
              required property var modelData
              width: help.bodyWidth
              spacing: Style.spacing.md

              PlainLabel {
                id: hintLabel
                objectName: "helpKey"
                width: Style.space(84)
                elide: Text.ElideNone
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
                text: modelData.hint
              }

              PlainLabel {
                objectName: "helpLabel"
                width: parent.width - hintLabel.width - parent.spacing
                wrapMode: Text.WordWrap
                color: help.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                text: modelData.label
              }
            }
          }
        }
      }
    }

    // The text zoom by the wheel (§5.6): Ctrl+wheel, on an item above everything so the
    // Flickable never takes the tick first; without Ctrl the handler declines and the
    // wheel falls through to the list. The item takes no click.
    Item {
      anchors.fill: parent
      z: 20

      WheelHandler {
        acceptedModifiers: Qt.ControlModifier
        onWheel: function(event) { root.bumpUiScale(event.angleDelta.y > 0 ? 1 : -1) }
      }
    }
  }

  // Each view has its frame from creation: given on load instead, every binding that
  // reads the frame ran again for each of its properties as they arrived (the Updates
  // view built its 500 rows five times on its first visit, 1n2p.38).
  Component {
    id: updatesView
    UpdatesView { frame: root }
  }

  Component {
    id: installedView
    InstalledView { frame: root }
  }

  Component {
    id: issuesView
    IssuesView { frame: root }
  }

  Component {
    id: errorsView
    ErrorsView { frame: root }
  }

  Component {
    id: searchView
    SearchView { frame: root }
  }

  Component {
    id: backupsView
    BackupsView { frame: root }
  }

  Component {
    id: historyView
    HistoryView { frame: root }
  }

  // The addon page (1n2p.23): the frame owns its component, so a page can open another
  // addon's page (a dependency) on top of itself.
  Component {
    id: addonPageComponent
    AddonPage {}
  }

  // A change's page (1n2p.30): what a History row's change did, client by client, and
  // its undo; an addon on it opens that addon's page on top.
  Component {
    id: changePageComponent
    ChangePage {}
  }

  // The keys that are no single character (design doc §3: Ctrl+Z, PgUp, PgDn, the
  // text zoom) are window shortcuts: they fire on the ShortcutOverride pass, before the
  // key catcher sees the key, and go through the same KEYMAP lookup as every other key.
  Item {
    Shortcut {
      sequence: "Ctrl+Z"
      enabled: root.shortcutsOn
      onActivated: root.act("Ctrl+Z")
    }
    Shortcut {
      sequence: "PgUp"
      enabled: root.shortcutsOn
      onActivated: root.act("PgUp")
    }
    Shortcut {
      sequence: "PgDown"
      enabled: root.shortcutsOn
      onActivated: root.act("PgDn")
    }
    Shortcut {
      sequences: ["Ctrl+=", "Ctrl++"]
      enabled: root.opened
      onActivated: root.act("Ctrl+=")
    }
    Shortcut {
      sequences: ["Ctrl+-", "Ctrl+_"]
      enabled: root.opened
      onActivated: root.act("Ctrl+-")
    }
    Shortcut {
      sequence: "Ctrl+0"
      enabled: root.opened
      onActivated: root.act("Ctrl+0")
    }
  }
}
