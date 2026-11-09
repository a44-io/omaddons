import QtQuick
import QtTest
import Quickshell
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "../../Store.js" as Store

// PanelView.qml (the frame) against a stub service (pipeline step 4): view switching,
// the key map's dispatch to service calls, the key catcher blocked while a view types,
// the read-only state of an incompatible binary, the confirm dialog's fresh-press rule
// after a stale preview, the cards and the footer. Then the keyboard panel inside
// BarWidget.qml.
TestCase {
  id: testCase
  name: "Panel"
  when: windowShown
  visible: true
  width: 600
  height: 720

  property var svc: null
  property var view: null
  property var made: []

  // The service as the frame sees it: its model, and every call recorded.
  Component {
    id: serviceComponent
    QtObject {
      property string phase: "ok"
      readonly property bool canAct: phase === "ok"
      property var summary: ({
        "state": "updates", "targets": 2, "running": ["retail"],
        "updates": { "available": 3, "targets": ["era", "retail"] },
        "issues": { "p0": 0, "p1": 0, "p2": 1, "p3": 0 }, "issuesTotal": 1, "issuesUrgent": 0,
        "errors": null, "checkDue": false, "widgetOutdated": false
      })
      property var targets: [
        { "id": "era", "running": false, "runningUnknown": false, "clientDir": "/games/era",
          "updates": { "available": 1, "checkedAt": "2100-01-01T00:00:00Z" }, "issues": [] },
        { "id": "retail", "running": true, "runningUnknown": false, "clientDir": "/games/retail",
          "updates": { "available": 2, "checkedAt": "2100-01-01T00:00:00Z" }, "issues": [] }
      ]
      property var lastChange: ({
        "unknown": false, "kind": "update", "label": "Undo update on era",
        "undo": { "command": "omaddons undo --run 01J", "argv": ["omaddons", "undo", "--run", "01J"], "destructive": true, "runnable": true }
      })
      property var action: null
      property string queuedAction: ""
      property var lastResult: null
      property var lastError: null
      property var confirm: null
      property string incompatibleText: ""
      property string unreachableText: ""
      property string binary: "/opt/omaddons-fixture/omaddons"
      property string requestedView: ""
      property var commands: []
      property var machineIssues: []
      property bool checking: false
      // What the open panel publishes for IPC's state().
      property var panelState: null

      property var runs: []
      property var cliRuns: []
      property var reads: []
      property int checks: 0
      property int refreshes: 0
      property int reinstalls: 0
      property int yes: 0
      property int no: 0

      // `shown` (what the view showed, for a previewed update) is recorded when given.
      function run(name, slots, label, shown) { runs = runs.concat([shown === undefined ? [name, slots, label] : [name, slots, label, shown]]); return "" }
      function runCliArgv(argv, label) { cliRuns = cliRuns.concat([[argv, label]]); return "" }
      function readView(view) { reads = reads.concat([view]) }
      function check() { checks++ }
      function refresh() { refreshes++ }
      function reinstallWidget() { reinstalls++; return "ok" }
      function confirmYes() { yes++; confirm = null; return "" }
      function confirmNo() { no++; confirm = null }
      function applySettings(settings) {}
      function setPanelOpen(key, open) {}
    }
  }

  Component {
    id: viewComponent
    PanelView {
      width: 540
      height: 700
    }
  }

  // A view as F5, F6 and F9 will load one: rows, the keys it handles, a search field.
  Component {
    id: fakeView
    Item {
      property var frame: null
      property int rowCount: 3
      property bool typing: false
      property var keys: []
      property int searches: 0
      width: 500
      implicitHeight: 90

      function handleKey(name, cursor) {
        keys = keys.concat([[name, cursor]])
        return true
      }
      function focusSearch() { searches++ }
      function rowItem(i) { return rows.itemAt(i) }

      Column {
        Repeater {
          id: rows
          model: 3
          Rectangle { width: 10; height: 30 }
        }
      }
    }
  }

  // A 150-row view, to scroll the content past the panel's height.
  Component {
    id: longView
    Item {
      property var frame: null
      property int rowCount: 150
      property bool typing: false
      width: 500
      implicitHeight: rows.implicitHeight

      function handleKey(name, cursor) { return true }
      function rowItem(i) { return repeater.itemAt(i) }

      Column {
        id: rows
        Repeater {
          id: repeater
          model: 150
          Rectangle { width: 10; height: 30 }
        }
      }
    }
  }

  // A view with a filter that Esc clears before the panel closes (the .22 seam).
  Component {
    id: filterView
    Item {
      property var frame: null
      property int rowCount: 1
      property bool typing: false
      property string filterText: ""
      width: 500
      implicitHeight: 30

      function handleKey(name, cursor) { return true }
      function escapeFilter() {
        if (filterText === "") return false
        filterText = ""
        return true
      }
    }
  }

  // Counts how many times a view is made.
  property int viewsMade: 0
  Component {
    id: countedView
    Item {
      property var frame: null
      property int rowCount: 0
      property bool typing: false
      property string mark: ""
      width: 500
      implicitHeight: 30
      Component.onCompleted: testCase.viewsMade++
      function handleKey(name, cursor) { return false }
    }
  }

  Component {
    id: barComponent
    BarWidget {}
  }

  function make(props) {
    var v = viewComponent.createObject(testCase, props || {})
    verify(v !== null)
    made = made.concat([v])
    return v
  }

  function open(v) {
    v.opened = true
    v.focusItem.forceActiveFocus()
    tryVerify(function() { return v.focusItem.activeFocus })
  }

  function init() {
    Quickshell.resetDetachedCommands()
    svc = serviceComponent.createObject(testCase)
    view = make({ "svc": svc })
    open(view)
  }

  function cleanup() {
    for (var i = 0; i < made.length; i++) made[i].destroy()
    made = []
    Store.clear(svc)
    svc.destroy()
    svc = null
    view = null
  }

  function child(name, from) {
    var found = findChild(from || view, name)
    verify(found !== null, "no " + name)
    return found
  }

  function json(value) {
    return JSON.stringify(value)
  }

  // ------------------------------------------------------------- views

  function test_h_l_and_the_digits_switch_views_and_read_them() {
    compare(view.view, "updates")
    keyClick("l")
    compare(view.view, "installed")
    compare(svc.reads[svc.reads.length - 1], "installed")
    keyClick("h")
    keyClick("h")
    compare(view.view, "backups", "h wraps around")
    keyClick("3")
    compare(view.view, "search")
    keyClick(Qt.Key_Right)
    compare(view.view, "issues")
    child("tab_installed").clicked()
    compare(view.view, "installed")
    // The tabs are in design doc §2.1's order, Installed titled Addons.
    compare(child("tab_installed").text, "2 Addons")
    verify(child("tab_installed").selected)
    verify(child("content").item !== null)
    // Every view of the switcher has its component; one without would leave the slot empty.
    for (var n = 1; n <= 5; n++) {
      keyClick(String(n))
      verify(child("content").item !== null, "view " + n)
    }
    view.viewComponents = ({})
    compare(child("content").item, null)
  }

  function test_a_requested_view_opens_with_the_panel() {
    view.opened = false
    svc.requestedView = "issues"
    view.opened = true
    compare(view.view, "issues")
    compare(svc.requestedView, "")
    // While open, too (`show <view>` over IPC).
    svc.requestedView = "backups"
    compare(view.view, "backups")
  }

  // ------------------------------------------------------------- the key map

  function test_U_updates_all_or_the_selected_client() {
    // Each previews with what the Updates view shows (nothing read here: no rows).
    keyClick("U")
    compare(json(svc.runs[0]), json(["update_all", {}, "Update all", []]))
    keyClick("t")
    compare(view.filter, "era")
    keyClick("U")
    compare(json(svc.runs[1]), json(["update_target", { "target": "era" }, "Update era", []]))
    keyClick("t")
    compare(view.filter, "retail")
    keyClick("t")
    compare(view.filter, "", "t comes back to All")
    // T steps back: the last client, then the one before it.
    keyClick("T")
    compare(view.filter, "retail")
    keyClick("T")
    compare(view.filter, "era")
    keyClick("T")
    compare(view.filter, "")
    // While a change runs, no other starts.
    svc.action = { "label": "Update all", "current": 1, "total": 4, "message": "retail: download 1/4" }
    keyClick("U")
    compare(svc.runs.length, 2)
  }

  function test_c_ctrl_z_o_and_their_buttons() {
    keyClick("c")
    compare(svc.checks, 1)
    // `z` alone is no undo any more (design doc §3.3): it zooms.
    keyClick("z")
    compare(svc.cliRuns.length, 0)
    verify(view.zoomed)
    keyClick("z")
    verify(!view.zoomed)
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    compare(json(svc.cliRuns[0]), json([["omaddons", "undo", "--run", "01J"], "Undo update on era"]))
    compare(svc.cliRuns.length, 1, "Ctrl+Z runs once, never as a typed z too")
    // o needs a client: nothing with All.
    keyClick("o")
    compare(Quickshell.detachedCommands.length, 0)
    verify(!child("openButton").enabled)
    keyClick("t")
    keyClick("o")
    compare(json(Quickshell.detachedCommands[0]), json(["xdg-open", "/games/era/Interface/AddOns"]))
    verify(child("openButton").enabled)
    child("checkButton").clicked()
    compare(svc.checks, 2)
  }

  function test_s_goes_to_search_and_focuses_its_field() {
    var search = fakeView.createObject(testCase)
    made = made.concat([search])
    view.viewComponents = { "search": fakeView }
    // `/` is the filter's (.22): nothing yet.
    keyClick("/")
    compare(view.view, "updates")
    keyClick("s")
    compare(view.view, "search")
    var loaded = child("content").item
    verify(loaded !== null)
    compare(loaded.frame, view)
    compare(loaded.searches, 1)
  }

  function test_view_keys_go_to_the_view_or_their_registration_and_else_nowhere() {
    view.viewComponents = { "updates": fakeView }
    var loaded = child("content").item
    keyClick("j")
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 2, "the cursor stops at the last row")
    keyClick("k")
    keyClick("p")
    keyClick("x")
    // Updates binds no `x`: it says why and reaches no view, until the next key.
    compare(child("keyNote").text, "Nothing to remove here")
    verify(child("keyNote").visible)
    keyClick(Qt.Key_Return)
    compare(child("keyNote").text, "")
    keyClick(Qt.Key_Space)
    compare(json(loaded.keys), json([["pin", 1], ["enter", 1], ["enter", 1]]),
      "Enter acts once (activateRequested only)")
    // A view without a component, and a key no one registered: nothing.
    view.keyHandlers = ({})
    keyClick("4")
    keyClick("b")
    compare(svc.runs.length + svc.cliRuns.length, 0)
    // Registered by the action's name, as the views receive it.
    var asked = []
    view.keyHandlers = { "backup": function(frame) { asked.push(frame.filter) } }
    keyClick("t")
    keyClick("b")
    compare(json(asked), json(["era"]))
  }

  function test_the_catcher_is_blocked_while_a_view_types() {
    view.viewComponents = { "updates": fakeView }
    var loaded = child("content").item
    loaded.typing = true
    verify(child("keyCatcher").blocked)
    keyClick("U")
    keyClick("c")
    // A field that types keeps its own Ctrl+Z (the frame's shortcut is off).
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    compare(svc.runs.length + svc.cliRuns.length, 0)
    compare(svc.checks, 0)
    loaded.typing = false
    tryVerify(function() { return view.focusItem.activeFocus })
    keyClick("c")
    compare(svc.checks, 1)
  }

  function test_esc_closes_and_tab_goes_to_the_next_panel() {
    var closed = 0
    var tabs = []
    view.closeRequested.connect(function() { closed++ })
    view.tabRequested.connect(function(direction) { tabs.push(direction) })
    keyClick(Qt.Key_Escape)
    compare(closed, 1)
    keyClick(Qt.Key_Tab)
    keyClick(Qt.Key_Backtab, Qt.ShiftModifier)
    compare(json(tabs), json([1, -1]))
  }

  // ------------------------------------------------------------- the key reference

  // The reference's section titles and its rows' keys, as drawn.
  function helpShown() {
    var sections = child("helpSections")
    var out = []
    for (var i = 0; i < sections.count; i++) {
      var section = sections.itemAt(i)
      out.push(section.modelData.title)
    }
    return out
  }

  function test_question_mark_opens_the_reference_in_every_view_and_on_a_card() {
    for (var n = 0; n < Model.VIEWS.length; n++) {
      keyClick(String(n + 1))
      keyClick("?")
      verify(view.helpOpen, Model.VIEWS[n])
      verify(child("help").visible)
      tryVerify(function() { return child("sheetKeys", child("help")).activeFocus })
      compare(child("sheetTitle", child("help")).text, "Keys · " + Model.VIEW_TITLES[Model.VIEWS[n]])
      // This view's own keys first, then the frame's, then a dialog's.
      compare(json(helpShown()), json(Model.helpSections(Model.VIEWS[n]).map(function(s) { return s.title })))
      compare(helpShown()[0], Model.VIEW_TITLES[Model.VIEWS[n]])
      keyClick("?")
      verify(!view.helpOpen, "? closes it again")
      tryVerify(function() { return view.focusItem.activeFocus })
    }
    // A problem card: its own keys.
    svc.phase = "missing"
    keyClick("?")
    verify(view.helpOpen)
    compare(child("sheetTitle", child("help")).text, "Keys · Problem card")
    compare(helpShown()[0], "Problem card")
    // Esc closes the reference, one layer: the panel stays open.
    var closed = 0
    view.closeRequested.connect(function() { closed++ })
    keyClick(Qt.Key_Escape)
    verify(!view.helpOpen)
    compare(closed, 0)
  }

  function test_every_key_beneath_the_reference_is_inert() {
    view.viewComponents = { "updates": fakeView }
    var loaded = child("content").item
    keyClick("?")
    verify(view.helpOpen)
    for (var key of ["U", "u", "c", "o", "p", "x", "b", "d", "t", "T", "s", "y", "n", "e", "g", "G", "1", "3", "h", "l", "j", "k"]) keyClick(key)
    keyClick(Qt.Key_Return)
    keyClick(Qt.Key_Space)
    keyClick(Qt.Key_PageDown)
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    verify(view.helpOpen, "still open")
    compare(svc.runs.length + svc.cliRuns.length + svc.checks, 0)
    compare(json(loaded.keys), json([]))
    compare([view.view, view.filter, view.cursor], ["updates", "", 0])
    // A dialog over it takes its place: the reference closes.
    svc.confirm = { "text": "Remove Foo?", "stale": "" }
    verify(!view.helpOpen)
    tryVerify(function() { return child("confirmKeys").activeFocus })
  }

  function test_j_k_scroll_the_reference_and_it_opens_at_the_top() {
    // A short panel, so the reference scrolls.
    var small = make({ "svc": svc, "height": 260 })
    view.opened = false
    open(small)
    small.textKey("?")
    var help = child("help", small)
    var body = child("sheetBody", help)
    tryVerify(function() { return child("sheetKeys", help).activeFocus })
    verify(body.contentHeight > body.height, "the reference is longer than the panel")
    keyClick("j")
    keyClick(Qt.Key_Down)
    var down = body.contentY
    verify(down > 0)
    keyClick("k")
    verify(body.contentY < down)
    for (var i = 0; i < 200; i++) keyClick("j")
    compare(body.contentY, body.contentHeight - body.height, "it stops at the bottom")
    keyClick("?")
    small.textKey("?")
    compare(body.contentY, 0, "it opens at the top every time")
  }

  function test_g_G_and_the_page_keys_move_the_cursor() {
    view.viewComponents = { "updates": fakeView }
    keyClick("G")
    compare(view.cursor, 2)
    keyClick("g")
    compare(view.cursor, 0)
    keyClick(Qt.Key_PageDown)
    compare(view.cursor, 2, "a page down stops at the last row")
    keyClick(Qt.Key_PageUp)
    compare(view.cursor, 0)
    verify(view.cursorActive)
  }

  function test_every_button_with_a_key_says_it_from_the_key_map() {
    compare(child("checkButton").tooltipText, "Check now (" + Model.keyHint("check") + ")")
    compare(child("undoButton").tooltipText, "Undo update on era (" + Model.keyHint("undo") + ")")
    compare(child("openButton").tooltipText, "Open AddOns (" + Model.keyHint("open") + ")")
    verify(child("undoButton").tooltipText.endsWith("(Ctrl+Z)"))
    // The reference says how it closes, from its own rows.
    keyClick("?")
    compare(child("referenceNote", child("help")).text.indexOf(Model.keyHint("close", "help")), 0)
  }

  function test_every_view_handles_every_action_its_rows_name() {
    for (var n = 0; n < Model.VIEWS.length; n++) {
      var name = Model.VIEWS[n]
      view.showView(name)
      var loaded = child("content").item
      verify(loaded !== null, name)
      var rows = Model.KEYMAP.filter(function(r) { return r.context === name })
      verify(rows.length > 0, name)
      for (var i = 0; i < rows.length; i++)
        verify(loaded.handleKey(rows[i].action, 0) === true, name + " handles " + rows[i].action)
    }
  }

  // ------------------------------------------------------------- read-only (M3-D42)

  function test_an_incompatible_binary_leaves_only_retry_and_reinstall() {
    svc.phase = "incompatible"
    svc.incompatibleText = "The widget is from omaddons 0.0.0-fixture; the binary is omaddons 9.0.0."
    view.viewComponents = { "updates": fakeView }
    for (var key of ["U", "u", "c", "o", "p", "x", "b", "d"]) keyClick(key)
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    compare(svc.runs.length + svc.cliRuns.length + svc.checks, 0)
    compare(child("keyNote").text, "", "a problem card explains no key")
    compare(child("content").item.keys.length, 0)
    verify(!child("content").enabled)
    verify(!child("checkButton").enabled)
    verify(!child("undoButton").enabled)
    verify(!child("openButton").enabled)
    compare(child("hero").detail, "stale")
    compare(child("cardTitle").text, svc.incompatibleText)
    // The card's buttons take the cursor and Enter.
    keyClick(Qt.Key_Return)
    compare(svc.refreshes, 1)
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(svc.reinstalls, 1)
    child("card_retry").clicked()
    compare(svc.refreshes, 2)
    // Navigation still works.
    keyClick("l")
    compare(view.view, "installed")
  }

  function test_the_missing_and_unreachable_cards_say_what_to_do_as_text() {
    svc.phase = "missing"
    compare(child("cardTitle").text, "omaddons not found at /opt/omaddons-fixture/omaddons")
    var commands = child("missingCommands")
    compare(commands.count, 2)
    compare(commands.itemAt(0).text, "omarchy bar set " + Build.PLUGIN_ID + " binary <path>")
    compare(commands.itemAt(1).text, "omaddons omarchy install")
    verify(findChild(view, "card_reinstall") === null)
    keyClick(Qt.Key_Return)
    compare(svc.refreshes, 1)
    svc.phase = "unreachable"
    svc.unreachableText = "thread 'main' panicked"
    compare(child("cardDetail").text, "thread 'main' panicked")
    verify(child("cardDetail").visible)
  }

  function test_no_clients_shows_how_to_add_one_as_text() {
    svc.targets = []
    verify(child("noTargets").visible)
    verify(!child("scope").visible, "no client to choose")
    // A status that failed lists no client either: it is no "no clients found".
    var kept = svc.summary
    svc.summary = null
    verify(!child("noTargets").visible)
    svc.summary = kept
  }

  // ------------------------------------------------------------- the confirm dialog

  function test_yes_needs_a_fresh_press_after_a_stale_preview() {
    svc.confirm = { "text": "Remove Foo 1.0 from retail (1 folder)", "stale": "" }
    tryVerify(function() { return child("confirmKeys").activeFocus })
    compare(child("confirmDialog").selectedIndex, 0, "a new preview starts on No")
    // Behind the dialog nothing acts.
    keyClick("u")
    compare(svc.runs.length, 0)
    keyClick("y")
    compare(svc.yes, 1)
    // The run found the plan stale: the service previews again.
    svc.confirm = { "text": "Remove Foo 2.0 from retail (1 folder)", "stale": "Changed since you looked: Foo. Nothing ran; check the new preview." }
    tryVerify(function() { return child("confirmKeys").activeFocus })
    verify(child("confirmDialog").message.indexOf("Changed since you looked: Foo.") === 0)
    verify(child("confirmDialog").message.indexOf("Remove Foo 2.0") > 0)
    // The key still held from the first Yes arrives only as auto-repeat: never a Yes.
    verify(view.confirmKey(Qt.Key_Y, "y", true))
    verify(view.confirmKey(Qt.Key_Return, "", true))
    compare(svc.yes, 1)
    // Enter on the default No cancels; a fresh y confirms.
    keyClick(Qt.Key_Return)
    compare(svc.no, 1)
    svc.confirm = { "text": "Remove Foo 2.0 from retail (1 folder)", "stale": "" }
    tryVerify(function() { return child("confirmKeys").activeFocus })
    keyClick("l")
    compare(child("confirmDialog").selectedIndex, 1)
    keyClick(Qt.Key_Return)
    compare(svc.yes, 2)
    // Back to the frame's keys.
    tryVerify(function() { return child("keyCatcher").activeFocus })
    keyClick("c")
    compare(svc.checks, 1)
  }

  function test_esc_and_n_cancel_the_dialog() {
    svc.confirm = { "text": "Undo update on era", "stale": "" }
    tryVerify(function() { return child("confirmKeys").activeFocus })
    keyClick(Qt.Key_Escape)
    compare(svc.no, 1)
    svc.confirm = { "text": "Undo update on era", "stale": "" }
    tryVerify(function() { return child("confirmKeys").activeFocus })
    keyClick("n")
    compare(svc.no, 2)
    compare(svc.yes, 0)
  }

  // ------------------------------------------------------------- the last change

  function test_an_unknown_last_change_disables_undo_and_says_so() {
    svc.lastChange = { "unknown": true }
    verify(child("lastChangeUnknown").visible)
    verify(!child("undoButton").enabled)
    keyClick("z")
    compare(svc.cliRuns.length, 0)
  }

  function test_an_undo_is_offered_as_redo() {
    svc.lastChange = { "unknown": false, "kind": "undo", "label": "Redo update on era",
      "undo": { "command": "omaddons undo --run 01K", "argv": ["omaddons", "undo", "--run", "01K"], "destructive": true, "runnable": true } }
    compare(child("undoButton").tooltipText, "Redo update on era (Ctrl+Z)")
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    compare(json(svc.cliRuns[0]), json([["omaddons", "undo", "--run", "01K"], "Redo update on era"]))
  }

  // ------------------------------------------------------------- the footer

  function test_the_footer_shows_progress_the_result_and_the_fix() {
    svc.action = { "label": "Update all", "current": 1, "total": 4, "message": "retail: download 1/4 · DBM" }
    svc.queuedAction = "Update era"
    verify(child("progressTrack").visible)
    compare(child("progressLine").text, "Update all: retail: download 1/4 · DBM")
    compare(child("queued").text, "next: Update era")
    verify(child("busyNote").visible)
    verify(!child("undoButton").enabled, "no undo while a change runs")
    svc.action = null
    svc.queuedAction = ""
    svc.lastResult = { "ok": true, "line": "Updated 3 addons on 2 clients · /reload in game", "partial": false,
      "commands": [{ "action": "undo", "command": "omaddons undo", "argv": ["omaddons", "undo"], "runnable": true }] }
    compare(child("resultLine").text, "Updated 3 addons on 2 clients · /reload in game")
    child("resultUndo").clicked()
    compare(json(svc.cliRuns[0]), json([["omaddons", "undo"], "Undo"]))
    svc.lastError = { "message": "no release of Foo loads on era", "code": "no_compatible_release",
      "fixes": [{ "command": "omaddons outdated --refresh", "argv": ["omaddons", "outdated", "--refresh"], "runnable": true }],
      "fixText": "" }
    compare(child("errorMessage").text, "no release of Foo loads on era")
    child("fix_0").clicked()
    compare(json(svc.cliRuns[1]), json([["omaddons", "outdated", "--refresh"], "Fix"]))
    svc.lastError = { "message": "Omarchy is not installed here", "code": "not_found", "fixes": [],
      "fixText": "this needs the Omarchy desktop (omarchy.org); everything else works without it" }
    compare(child("fixText").text, "this needs the Omarchy desktop (omarchy.org); everything else works without it")
    // The next change runs: the earlier failure is not shown under its progress.
    verify(child("errorCard").visible)
    svc.action = { "label": "Remove Foo", "current": 1, "total": 1, "message": "stage 1/1 · era" }
    verify(!child("errorCard").visible, "no earlier error while a change runs")
    svc.action = null
    verify(child("errorCard").visible)
  }

  function test_a_partial_result_offers_each_failed_items_fix() {
    svc.lastResult = { "ok": true, "line": "Updated 2 of 3; Plater failed: the download stopped", "partial": true, "commands": [],
      "fixes": [{ "command": "omaddons update -t era -- Plater", "argv": ["omaddons", "update", "-t", "era", "--", "Plater"], "runnable": true }] }
    compare(child("resultLine").text, "Updated 2 of 3; Plater failed: the download stopped")
    compare(child("itemFix_0").text, "Fix: omaddons update -t era -- Plater")
    child("itemFix_0").clicked()
    compare(json(svc.cliRuns[0]), json([["omaddons", "update", "-t", "era", "--", "Plater"], "Fix"]))
  }

  // ------------------------------------------------------------- the frame (design doc §2.1)

  function test_the_hero_names_the_scope_and_says_what_needs_you_urgent_first() {
    var hero = child("hero")
    compare(hero.title, "All clients")
    compare(hero.meta, "3 UPDATES · RETAIL RUNNING")
    compare(hero.detail, "2 clients")
    keyClick("t")
    compare(hero.title, "era")
    compare(hero.meta, "1 UPDATE")
    // A new Lua error goes first, and the glyph turns urgent.
    var targets = JSON.parse(JSON.stringify(svc.targets))
    targets[0].errors = { "new": 2 }
    svc.targets = targets
    compare(hero.meta, "2 NEW ERRORS · 1 UPDATE")
    compare(view.heroState, "urgent")
    var glyph = findChild(hero, "heroGlyph")
    verify(glyph !== null)
    compare(String(glyph.color), String(view.urgent))
    keyClick("t")
    compare(view.heroState, "quiet", "retail has none")
    // While a change runs, the glyph is the accent's.
    svc.action = { "label": "Update all", "current": 0, "total": 1, "message": "" }
    compare(view.heroState, "busy")
    compare(String(glyph.color), String(view.accent))
  }

  function test_the_scope_dropdown_follows_t_and_sets_the_scope() {
    var scope = child("scope")
    verify(scope.visible)
    compare(json(scope.options.map(function(o) { return o.value })), json(["", "era", "retail"]))
    compare(scope.options[2].label, "retail · 2 updates · running")
    compare(scope.value, "")
    keyClick("t")
    compare(scope.value, "era", "the trigger follows t")
    keyClick("T")
    compare(scope.value, "")
    scope.choose("retail")
    compare(view.filter, "retail")
    compare(child("hero").title, "retail")
    // No compatible answer: no scope, no tabs (§2.1).
    svc.phase = "missing"
    verify(!scope.visible)
    verify(!child("switcher").visible)
  }

  function test_the_tabs_count_what_each_view_has_and_urgent_counts_say_so() {
    compare(child("countText", child("tab_updates")).text, "3")
    verify(child("countText", child("tab_updates")).visible)
    verify(!findChild(child("tab_issues"), "countText").visible, "a zero count is not shown")
    var targets = JSON.parse(JSON.stringify(svc.targets))
    targets[1].issues = [{ "id": "fm-1", "urgent": true, "title": "a journal waits" }]
    svc.targets = targets
    svc.commands = [{ "action": "backup_create", "argv": ["omaddons", "backup", "create", "-t", "era"] }]
    compare(child("countText", child("tab_issues")).text, "1")
    verify(child("countText", child("tab_issues")).font.bold, "urgent: bold, on an urgent border")
    compare(child("countText", child("tab_backups")).text, "1")
    verify(!child("countText", child("tab_backups")).font.bold)
    // Each tab says its key.
    compare(child("tab_search").tooltipText, "Search (3)")
    // Under one client, its counts.
    keyClick("t")
    compare(child("countText", child("tab_updates")).text, "1")
    verify(!findChild(child("tab_issues"), "countText").visible)
  }

  function test_progress_results_cards_and_the_footer_stay_visible_below_a_long_list() {
    view.viewComponents = { "updates": longView }
    svc.action = { "label": "Update all", "current": 1, "total": 4, "message": "retail: download 1/4" }
    svc.lastError = { "message": "DBM failed to update", "code": "network", "fixes": [], "fixText": "" }
    view.height = 400
    waitForRendering(view)
    keyClick("G")
    compare(view.cursor, 149)
    var flick = child("contentFlick")
    verify(flick.contentY > 0, "the list scrolled")
    compare(flick.contentY, flick.contentHeight - flick.height, "to its end")
    for (var name of ["progressLine", "freshness"]) {
      var item = child(name)
      verify(item.visible, name)
      var y = item.mapToItem(view, 0, 0).y
      verify(y >= 0 && y + item.height <= view.height, name + " is inside the panel at " + y)
    }
    // The failure card is pinned too (shown once the change is over).
    svc.action = null
    var card = child("errorCard")
    verify(card.visible)
    var cardY = card.mapToItem(view, 0, 0).y
    verify(cardY >= 0 && cardY + card.height <= view.height, "the failure card is inside the panel at " + cardY)
    compare(child("freshness").text, "2 clients · checked just now · retail running")
  }

  function test_esc_backs_out_one_layer_at_a_time_dialog_page_filter_panel() {
    view.viewComponents = { "updates": filterView }
    var closed = 0
    view.closeRequested.connect(function() { closed++ })
    verify(view.push({ "title": "DBM", "component": fakeView, "props": { "rowCount": 2 } }))
    compare(child("page").item.rowCount, 2, "the page gets its props")
    compare(child("backButton").text, "Updates")
    verify(!child("content").visible, "the page replaces the view")
    // A dialog is the top layer.
    svc.confirm = { "text": "Remove Foo?", "stale": "" }
    tryVerify(function() { return child("confirmKeys").activeFocus })
    keyClick(Qt.Key_Escape)
    compare([svc.no, view.pages.length, closed], [1, 1, 0], "Esc cancels the dialog only")
    tryVerify(function() { return view.focusItem.activeFocus })
    // Then the page.
    keyClick(Qt.Key_Escape)
    compare([view.pages.length, closed], [0, 0], "then the page")
    verify(child("content").visible)
    // Then the view's filter, then the panel.
    var loaded = child("content").item
    loaded.filterText = "plat"
    keyClick(Qt.Key_Escape)
    compare([loaded.filterText, closed], ["", 0], "then the filter")
    keyClick(Qt.Key_Escape)
    compare(closed, 1, "then the panel closes")
  }

  function test_a_deeper_stack_shows_its_trail_and_h_or_the_back_button_pops_it() {
    view.push({ "title": "DBM", "component": fakeView })
    view.push({ "title": "LibDBIcon", "component": fakeView })
    compare(child("backButton").text, "Updates › DBM")
    keyClick("?")
    compare(child("sheetTitle", child("help")).text, "Keys · LibDBIcon")
    keyClick("?")
    tryVerify(function() { return view.focusItem.activeFocus })
    keyClick("l")
    compare(view.pages.length, 2, "l does nothing on a page")
    keyClick("h")
    compare(view.pages.length, 1)
    compare(child("backButton").text, "Updates")
    child("backButton").clicked()
    compare(view.pages.length, 0)
    // A tab clears the stack.
    view.push({ "title": "DBM", "component": fakeView })
    keyClick("4")
    compare([view.view, view.pages.length], ["issues", 0])
  }

  function test_z_fills_the_screen_and_the_text_zoom_scales_within_bounds() {
    keyClick("z")
    verify(view.zoomed)
    compare(child("zoomButton").tooltipText, "Back to panel size (z)")
    child("zoomButton").clicked()
    verify(!view.zoomed)
    var base = view.frameImplicitHeight
    keyClick(Qt.Key_Equal, Qt.ControlModifier)
    compare(view.uiScale, 1.1)
    compare(view.contentImplicitHeight, view.frameImplicitHeight * 1.1)
    compare(child("canvas").width, view.width / 1.1)
    keyClick(Qt.Key_Minus, Qt.ControlModifier)
    keyClick(Qt.Key_Minus, Qt.ControlModifier)
    compare(view.uiScale, 0.9)
    keyClick(Qt.Key_0, Qt.ControlModifier)
    compare(view.uiScale, 1.0)
    for (var i = 0; i < 20; i++) keyClick(Qt.Key_Equal, Qt.ControlModifier)
    compare(view.uiScale, 2.0, "at most 2×")
    for (var j = 0; j < 30; j++) view.bumpUiScale(-1)
    compare(view.uiScale, 0.6, "at least 0.6×")
    view.bumpUiScale(0)
    // Ctrl+wheel too; a plain wheel scrolls instead.
    mouseWheel(view, 50, 50, 0, 120, Qt.NoButton, Qt.ControlModifier)
    compare(view.uiScale, 1.1)
    mouseWheel(view, 50, 50, 0, 120)
    compare(view.uiScale, 1.1)
    // Kept in memory for the session: closing the panel keeps them, nothing writes them.
    keyClick("z")
    view.opened = false
    view.opened = true
    verify(view.zoomed)
    compare(view.uiScale, 1.1)
    compare(base > 0, true)
  }

  function test_the_open_panel_publishes_its_scope_view_stack_sheet_and_zoom() {
    compare(json(svc.panelState), json({ "scope": "", "view": "updates", "stackDepth": 0, "sheet": null, "zoomed": false, "textZoom": 1 }))
    keyClick("t")
    keyClick("2")
    view.push({ "title": "DBM", "component": fakeView })
    keyClick("z")
    compare(json(svc.panelState), json({ "scope": "era", "view": "installed", "stackDepth": 1, "sheet": null, "zoomed": true, "textZoom": 1 }))
    keyClick("?")
    compare(svc.panelState.sheet, "help")
    keyClick("?")
    svc.confirm = { "text": "Remove Foo?", "stale": "" }
    compare(svc.panelState.sheet, "confirm")
    svc.confirm = null
    compare(svc.panelState.sheet, null)
    // A closed panel stops publishing.
    view.opened = false
    keyClick("t")
    view.filter = "retail"
    compare(svc.panelState.scope, "era")
  }

  function test_a_view_is_made_on_its_first_visit_and_kept() {
    viewsMade = 0
    view.viewComponents = { "updates": countedView, "installed": countedView }
    compare(viewsMade, 1, "only the view shown")
    keyClick("2")
    compare(viewsMade, 2)
    var installed = child("content").item
    installed.mark = "kept"
    keyClick("1")
    keyClick("2")
    compare(viewsMade, 2, "each made once")
    compare(child("content").item.mark, "kept", "and kept as it was")
  }

  function test_check_now_spins_while_a_check_runs() {
    var button = child("checkButton")
    verify(!button.spinning)
    compare(button.tooltipText, "Check now (c)")
    svc.checking = true
    verify(button.spinning)
    compare(button.tooltipText, "Checking… (c)")
    tryVerify(function() { return button.rotation !== 0 })
    svc.checking = false
    tryCompare(button, "rotation", 0)
  }

  function test_a_narrow_panel_shows_glyph_tabs_and_names_its_view() {
    var narrow = make({ "svc": svc, "width": 420 })
    view.opened = false
    open(narrow)
    verify(narrow.narrow)
    var tab = child("tab_updates", narrow)
    compare(tab.text, "")
    verify(tab.iconText !== "")
    verify(child("viewTitle", narrow).visible)
    compare(child("viewTitle", narrow).text, "Updates")
    compare(child("hero", narrow).detail, "")
    compare(child("hero", narrow).meta, "3 UPDATES", "one part of the meta line")
    verify(!view.narrow, "540 is not narrow")
    verify(!child("viewTitle").visible)
  }

  // ------------------------------------------------------------- inside BarWidget

  function test_the_bar_widget_holds_the_frame_in_its_keyboard_panel() {
    Store.set(svc)
    var bar = barComponent.createObject(testCase, { "settings": { "panelWidth": 600 } })
    made = made.concat([bar])
    tryVerify(function() { return bar.svc === svc })
    var panel = child("panel", bar)
    var inner = child("view", bar)
    compare(panel.contentWidth, 600)
    compare(inner.svc, svc)
    verify(!panel.open)
    bar.open()
    verify(panel.open)
    verify(inner.opened)
    tryVerify(function() { return inner.focusItem.activeFocus })
    keyClick(Qt.Key_Tab)
    compare(json(bar.switched), json([1]))
    keyClick(Qt.Key_Escape)
    verify(!bar.opened)
    // An out-of-range width falls back to the default.
    bar.settings = { "panelWidth": 2000 }
    compare(panel.contentWidth, 540)
  }
}
