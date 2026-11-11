import QtQuick
import QtTest
import Quickshell
import qs.Commons
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "../../Store.js" as Store

// PanelView.qml (the frame) against a stub service (pipeline step 4): view switching,
// the key map's dispatch to service calls, the key catcher blocked while a view types,
// the read-only state of an incompatible binary, the plan sheet's fresh-press rule after
// a stale preview and its scrolling, the cards and the footer. Then the keyboard panel inside
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
      // The activity bar's model (1n2p.18): what the player waits on, what waits, the
      // rows a change changed, and whether the last result still shows.
      property var activity: null
      property string queuedText: ""
      property var doneRows: ({})
      property bool resultShown: true
      // Whether check() is taken (false: the network lane is full).
      property bool checkAccepted: true
      property int cancels: 0
      property int dismissals: 0
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
      // The scope a panel showed last, which the next one to open starts from (§2.2).
      property string scope: ""

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
      function check() { checks++; return checkAccepted }
      function cancelActivity() { cancels++; return true }
      function dismissError() { dismissals++; lastError = null }
      function refresh() { refreshes++ }
      function reinstallWidget() { reinstalls++; return "ok" }
      // The card shown when omaddons cannot be used, and an interrupted change
      // (Model.phaseCard, Model.interruptedChange, 1n2p.34); what Copy put on the clipboard.
      property var phaseCard: null
      property var interrupted: null
      // The real copyCommand answers `done` with wl-copy's outcome; this answers `copyOk`
      // (false: wl-copy missing).
      property var copies: []
      property bool copyOk: true
      function copyCommand(text, done) {
        copies = copies.concat([text])
        if (typeof text !== "string" || text === "") return false
        if (typeof done === "function") done(copyOk)
        return true
      }
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

  // A view whose rows belong to clients (an era row, then a retail row), as every real
  // view's `rows` do: what `o` acts on under All.
  Component {
    id: rowsView
    Item {
      property var frame: null
      readonly property var rows: [{ "target": "era" }, { "target": "retail" }]
      property int rowCount: 2
      property bool typing: false
      width: 500
      implicitHeight: 60

      function handleKey(name, cursor) { return false }
      function rowItem(i) { return repeater.itemAt(i) }

      Column {
        Repeater {
          id: repeater
          model: 2
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
    // Retail runs here, so the panel opened on it (design doc §2.2); these tests start
    // under All, and the opening rule has tests of its own.
    view.setFilter("")
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

  // A plan sheet as Model builds one: `count` removals on Retail (one by default), the
  // banner `stale`.
  function planSheet(title, stale, count) {
    var items = []
    for (var i = 0; i < (count || 1); i++) {
      var name = "Foo" + (i ? i : "")
      items.push({ "op": "remove", "addon": name, "client": "Retail", "clientKey": "k", "from": "1.0", "to": "", "name": name,
        "detail": "1.0 (1 folder)", "text": name + " 1.0 (1 folder)", "header": "" })
    }
    return { "title": title, "stale": stale || "", "items": items,
      "effects": [{ "kind": "folders_removed", "severity": "urgent", "glyph": 0xF0258, "text": "Removes the folder Foo. Undo brings it back." }],
      "confirmLabel": count > 1 ? "Remove " + count : "Remove Foo", "cancelLabel": "Cancel", "destructive": true, "glyph": 0xF01B4 }
  }

  // The open plan sheet has the keyboard.
  function sheetHasKeys() {
    tryVerify(function() { return child("planSheet").keyItem.activeFocus })
  }

  // ------------------------------------------------------------- views

  function test_h_l_and_the_digits_switch_views_and_read_them() {
    compare(view.view, "updates")
    keyClick("l")
    compare(view.view, "installed")
    compare(svc.reads[svc.reads.length - 1], "installed")
    keyClick("h")
    keyClick("h")
    compare(view.view, "history", "h wraps around")
    keyClick("3")
    compare(view.view, "search")
    keyClick(Qt.Key_Right)
    compare(view.view, "errors")
    compare(svc.reads[svc.reads.length - 1], "errors")
    keyClick("5")
    compare(view.view, "issues")
    child("tab_installed").clicked()
    compare(view.view, "installed")
    // The tabs are in design doc §2.1's order, Installed titled Addons.
    compare(child("tab_installed").text, "Addons")
    // Its key is in the tooltip, not on the chip.
    compare(child("tab_installed").tooltipText, "Addons (2)")
    verify(child("tab_installed").selected)
    verify(child("content").item !== null)
    // Every view of the switcher has its component; one without would leave the slot empty.
    for (var n = 1; n <= Model.VIEWS.length; n++) {
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
    // The first j highlights row 0; three more would pass the last row (2).
    for (var i = 0; i < 4; i++) keyClick("j")
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
    // A plan sheet over it takes its place: the reference closes.
    svc.confirm = { "sheet": planSheet("Remove Foo from Retail?"), "stale": "" }
    verify(!view.helpOpen)
    sheetHasKeys()
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

  // With no row highlighted (a fresh view or page), the first j or k highlights row 0;
  // after that they move by one (omad-88pf).
  function test_the_first_j_or_k_highlights_the_first_row() {
    view.viewComponents = { "updates": fakeView }
    verify(!view.cursorActive)
    keyClick("j")
    compare([view.cursorActive, view.cursor], [true, 0])
    keyClick("j")
    compare(view.cursor, 1)
    // k too, in the panel opened again.
    view.opened = false
    open(view)
    verify(!view.cursorActive)
    keyClick("k")
    compare([view.cursorActive, view.cursor], [true, 0])
    // A page pushed on top starts with nothing highlighted; its first j is its line 0.
    verify(view.push({ "title": "DBM", "component": fakeView }))
    compare(child("page").item.rowCount, 3)
    verify(!view.cursorActive)
    keyClick("j")
    compare([view.cursorActive, view.cursor], [true, 0])
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

  function test_an_incompatible_binary_leaves_only_update_and_retry() {
    svc.phase = "incompatible"
    svc.phaseCard = Model.phaseCard("incompatible", { "mine": "0.0.0-fixture", "incompatible": { "kind": "version", "theirs": "9.0.0" } })
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
    compare(child("cardTitle").text, "The panel is from another omaddons version")
    compare(child("cardDetail").text, "Panel 0.0.0-fixture, omaddons 9.0.0. Update the panel to match; until then it only shows what it read last.")
    // The card's buttons take the cursor and Enter: Update the panel first, then Retry.
    compare(child("card_reinstall").text, "Update the panel")
    keyClick(Qt.Key_Return)
    compare(svc.reinstalls, 1)
    // The first j highlights Update the panel, the second Retry.
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick(Qt.Key_Return)
    compare(svc.refreshes, 1)
    child("card_retry").clicked()
    compare(svc.refreshes, 2)
    // Navigation still works.
    keyClick("l")
    compare(view.view, "installed")
  }

  // Each failure has its own card, in words, with one action besides Retry (1n2p.34).
  function test_each_failure_card_says_what_happened_and_offers_its_action() {
    var info = { "binary": "/opt/omaddons-fixture/omaddons", "home": "/opt", "mine": "0.0.0-fixture", "deadlineSec": 20,
      "failure": { "label": "Refresh", "outcome": "timeout", "argv": ["/opt/omaddons-fixture/omaddons", "status"], "exitCode": -1,
        "tail": ["thread 'main' panicked"] } }
    // A missing binary: Retry, and the command to run from the binary in use, copied.
    svc.phase = "missing"
    svc.phaseCard = Model.phaseCard("missing", info)
    compare(child("cardTitle").text, "Can't find omaddons")
    verify(child("cardDetail").text.indexOf("It was at ~/omaddons-fixture/omaddons.") === 0)
    compare(child("cardNotes").count, 1)
    compare(child("cardNotes").itemAt(0).text, "The panel tries again by itself.")
    verify(findChild(view, "card_reinstall") === null)
    keyClick(Qt.Key_Return)
    compare(svc.refreshes, 1)
    // The first j highlights Retry, the second the command's Copy.
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick(Qt.Key_Return)
    compare(svc.copies, ["omaddons omarchy install"])
    compare(child("cardCopied").text, "Copied")
    // wl-copy missing (omad-phcg): the card says nothing was copied, never "Copied".
    svc.copyOk = false
    keyClick(Qt.Key_Return)
    compare(svc.copies.length, 2)
    compare(child("cardCopied").text, "Couldn't copy (wl-copy is missing)")
    svc.copyOk = true
    // No answer in time, an unreadable one, a child let go: their words, Retry and Copy
    // details; the child's output only in the details, never on the card.
    var kinds = [["timeout", "omaddons didn't answer in time"], ["malformed", "omaddons didn't answer"],
      ["stuck", "omaddons stopped answering"]]
    for (var i = 0; i < kinds.length; i++) {
      svc.phase = "unreachable"
      info.outcome = kinds[i][0]
      info.failure.outcome = kinds[i][0]
      svc.phaseCard = Model.phaseCard("unreachable", info)
      compare(child("cardTitle").text, kinds[i][1])
      verify(child("cardDetail").text.indexOf("panicked") === -1, "the output stays out of the card")
      child("card_copy_details").clicked()
      compare(svc.copies[svc.copies.length - 1], svc.phaseCard.details)
      verify(svc.phaseCard.details.indexOf("thread 'main' panicked") !== -1)
    }
    // The same version's other files: Update the panel, and when it was tried.
    svc.phase = "incompatible"
    svc.phaseCard = Model.phaseCard("incompatible", { "mine": "0.0.0-fixture", "incompatible": { "kind": "outdated", "theirs": "0.0.0-fixture" },
      "reinstalledAtMs": Date.now() - 30000, "nowMs": Date.now() })
    compare(child("cardTitle").text, "The panel is out of date")
    compare(child("cardNotes").itemAt(0).text, "Update the panel ran just now; this is still the old panel.")
  }

  // An interrupted change, from `status` alone: a notice whose one action opens its
  // problem in Issues; the panel runs nothing by itself (1n2p.34).
  function test_an_interrupted_change_is_announced_and_its_action_runs_nothing() {
    verify(!child("interruptedCard").visible)
    svc.interrupted = { "clients": ["Retail"], "target": "retail", "title": "A change on Retail was interrupted",
      "sentence": "omaddons finishes or undoes it before the next change. See what it would do first." }
    var card = child("interruptedCard")
    verify(card.visible)
    compare(findChild(card, "noticeTitle").text, "A change on Retail was interrupted")
    findChild(card, "noticeButton_0").clicked()
    compare([view.view, view.filter], ["issues", "retail"])
    compare(svc.runs.length + svc.cliRuns.length + svc.checks, 0, "nothing ran")
    // Only on a live answer: no card while omaddons cannot be used.
    svc.phase = "missing"
    verify(!child("interruptedCard").visible)
  }

  // A request that ended with no envelope: its words, and Copy details (1n2p.34).
  function test_the_failure_card_copies_its_details() {
    svc.lastError = { "message": "Update all didn't finish: omaddons took too long", "code": "timeout", "fixes": [], "fixText": "",
      "details": "What: Update all\nEnded: timed out, no exit code" }
    verify(child("errorCard").visible)
    compare(child("errorMessage").text, "Update all didn't finish: omaddons took too long")
    child("copyDetails").clicked()
    compare(svc.copies, ["What: Update all\nEnded: timed out, no exit code"])
    compare(child("errorCopied").text, "Copied")
    // wl-copy missing (omad-phcg): the card says nothing was copied.
    svc.copyOk = false
    child("copyDetails").clicked()
    compare(svc.copies.length, 2)
    compare(child("errorCopied").text, "Couldn't copy (wl-copy is missing)")
    // A failure the CLI described has no details to copy.
    svc.lastError = { "message": "no target matches", "code": "target_not_found", "fixes": [], "fixText": "" }
    verify(!child("copyDetails").visible)
  }

  // No client: no command line to type anywhere (1n2p.32). Every view's state is the
  // welcome card with Scan again and Add a client folder (tst_onboarding.qml drives it).
  function test_no_clients_offers_the_welcome_card_never_a_command() {
    svc.targets = []
    verify(!child("scope").visible, "no client to choose")
    compare(findChild(view, "noTargets"), null, "the terminal-only text is gone")
    var state = Model.viewState("updates", { "service": svc, "nowMs": Date.now() })
    compare([state.state, state.notices[0].kind, state.notices[0].actions.map(function(a) { return a.name })],
      ["welcome", "welcome", ["rescan", "add"]])
    // A status that failed lists no client either: it is no welcome.
    var kept = svc.summary
    svc.summary = null
    verify(Model.viewState("updates", { "service": svc, "nowMs": Date.now() }).state !== "welcome")
    svc.summary = kept
    // Add a client folder opens its page, with the path box.
    verify(view.stateAction("add"))
    verify(view.topPage !== null)
    verify(findChild(view, "pathField") !== null)
  }

  // ------------------------------------------------------------- the plan sheet

  function test_yes_needs_a_fresh_press_after_a_stale_preview() {
    svc.confirm = { "sheet": planSheet("Remove Foo from Retail?"), "stale": "" }
    sheetHasKeys()
    var sheet = child("planSheet")
    compare(sheet.selectedIndex, 0, "a new preview starts on Cancel")
    // Its title names the change, its verb is the change's, destructive in urgent.
    compare([child("sheetTitle", sheet).text, child("sheetConfirm", sheet).text, child("sheetCancel", sheet).text],
      ["Remove Foo from Retail?", String.fromCodePoint(0xF01B4) + " Remove Foo", "Cancel"])
    verify(child("destructiveBorder", sheet).visible)
    verify(!child("planStale", sheet).visible, "no banner on a first preview")
    // Behind the sheet nothing acts.
    keyClick("u")
    compare(svc.runs.length, 0)
    keyClick("y")
    compare(svc.yes, 1)
    // The run found the plan stale: the service previews again, with the banner.
    var banner = "Changed since you looked: a newer version came out, or the change is different now. Nothing ran; here is what would happen now."
    svc.confirm = { "sheet": planSheet("Remove Foo from Retail?", banner), "stale": banner }
    sheetHasKeys()
    verify(child("planStale", sheet).visible)
    compare(child("planStale", sheet).title, banner)
    compare(sheet.selectedIndex, 0, "the new preview starts on Cancel too")
    // The key still held from the first Yes arrives only as auto-repeat: never a Yes.
    verify(view.confirmKey(Qt.Key_Y, "y", true))
    verify(view.confirmKey(Qt.Key_Return, "", true))
    compare(svc.yes, 1)
    // Enter on Cancel cancels; a fresh y confirms.
    keyClick(Qt.Key_Return)
    compare(svc.no, 1)
    svc.confirm = { "sheet": planSheet("Remove Foo from Retail?"), "stale": "" }
    sheetHasKeys()
    keyClick("l")
    compare(sheet.selectedIndex, 1)
    keyClick(Qt.Key_Return)
    compare(svc.yes, 2)
    // Back to the frame's keys.
    tryVerify(function() { return child("keyCatcher").activeFocus })
    keyClick("c")
    compare(svc.checks, 1)
  }

  function test_esc_and_n_cancel_the_plan_sheet() {
    svc.confirm = { "sheet": planSheet("Undo the update on Classic Era from 2026-10-22 22:46?"), "stale": "" }
    sheetHasKeys()
    keyClick(Qt.Key_Escape)
    compare(svc.no, 1)
    svc.confirm = { "sheet": planSheet("Undo the update on Classic Era from 2026-10-22 22:46?"), "stale": "" }
    sheetHasKeys()
    keyClick("n")
    compare(svc.no, 2)
    compare(svc.yes, 0)
  }

  // Forty items: none is cut, the panel asks for the room, and past its cap the body
  // scrolls inside the sheet, by j and k too.
  function test_forty_items_scroll_inside_the_plan_sheet() {
    var small = make({ "svc": svc, "height": 420 })
    view.opened = false
    open(small)
    var frameOnly = small.contentImplicitHeight
    svc.confirm = { "sheet": planSheet("Remove 40 addons from Retail?", "", 40), "stale": "" }
    tryVerify(function() { return child("planSheet", small).keyItem.activeFocus })
    var sheet = child("planSheet", small)
    compare(child("planItems", sheet).count, 40, "every item, no cap")
    verify(small.contentImplicitHeight > frameOnly, "the panel asks for the sheet's room")
    compare(child("sheetConfirm", sheet).text, String.fromCodePoint(0xF01B4) + " Remove 40")
    var body = child("sheetBody", sheet)
    tryVerify(function() { return body.contentHeight > body.height + 1 }, 2000, "the body overflows: " + body.contentHeight + " in " + body.height)
    verify(body.interactive)
    compare(body.contentY, 0)
    keyClick("j")
    verify(body.contentY > 0, "j scrolls the sheet")
    keyClick("k")
    compare(body.contentY, 0, "k scrolls back")
    compare([svc.yes, svc.no], [0, 0], "scrolling decides nothing")
  }

  // ------------------------------------------------------------- the last change

  // A last change the cached status cannot tell (never a guessed run, M3-D37): Undo
  // opens History, where every change has its own undo (1n2p.30), and runs nothing.
  function test_an_unknown_last_change_opens_history() {
    svc.lastChange = { "unknown": true }
    verify(findChild(view, "lastChangeUnknown") === null, "no 'last change: unknown' line")
    verify(child("undoButton").enabled)
    compare(child("undoButton").tooltipText, "Undo: choose the change in History (Ctrl+Z)")
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    compare(view.view, "history")
    compare(svc.cliRuns.length, 0)
    compare(svc.reads[svc.reads.length - 1], "history")
  }

  function test_an_undo_is_offered_as_redo() {
    svc.lastChange = { "unknown": false, "kind": "undo", "label": "Redo update on era",
      "undo": { "command": "omaddons undo --run 01K", "argv": ["omaddons", "undo", "--run", "01K"], "destructive": true, "runnable": true } }
    compare(child("undoButton").tooltipText, "Redo update on era (Ctrl+Z)")
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    compare(json(svc.cliRuns[0]), json([["omaddons", "undo", "--run", "01K"], "Redo update on era"]))
  }

  // ------------------------------------------------------------- the footer

  // A change running as the service starts it, after `event` (a progress line) when given.
  function changeActivity(label, event) {
    var a = Model.activityStart({ "id": 1, "kind": "mutation", "lane": "mutation", "priority": "user",
      "template": "update_target", "slots": { "target": "retail" }, "label": label }, Date.now(), null)
    return event ? Model.activityProgress(a, event, Date.now()) : a
  }

  function checkActivity() {
    return Model.activityStart({ "id": 2, "resource": "check", "kind": "data", "lane": "network", "label": "Check now" }, Date.now(), null)
  }

  function test_the_footer_shows_progress_the_result_and_the_fix() {
    svc.action = { "label": "Update all", "current": 1, "total": 4, "message": "download 1/4 · gh:o/dbm@2 on retail" }
    svc.activity = changeActivity("Update all", { "event": "progress", "phase": "download", "message": "gh:o/dbm@2 on retail",
      "current": 1, "total": 4, "target": "retail", "ref": "gh:o/dbm" })
    svc.queuedText = "Next: Update era"
    verify(child("progressTrack").visible)
    compare(child("progressFill").width, child("progressTrack").width * 0.25)
    // The phase in words, never the CLI's own message (UX-32).
    compare(child("progressLine").text, "Update all: downloading (1 of 4)")
    compare(child("queued").text, "Next: Update era")
    verify(child("busyNote").visible)
    verify(!child("activityCancel").visible, "a change in flight is never cancelled")
    verify(!child("undoButton").enabled, "no undo while a change runs")
    svc.action = null
    svc.activity = null
    svc.queuedText = ""
    verify(!child("activityBar").visible)
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
    // A failure stays until it is dismissed.
    child("errorDismiss").clicked()
    compare(svc.dismissals, 1)
    verify(!child("errorCard").visible)
  }

  function test_a_check_shows_in_the_activity_bar_and_can_be_cancelled() {
    svc.lastResult = { "ok": true, "line": "Updated 1 addon", "partial": false, "commands": [] }
    verify(child("result").visible)
    svc.activity = checkActivity()
    verify(child("activityBar").visible)
    compare(child("progressLine").text, "Checking for updates…")
    // No count yet: the fixed 15 % fill.
    compare(child("progressFill").width, child("progressTrack").width * 0.15)
    verify(!child("busyNote").visible, "a check holds no change back")
    verify(!child("result").visible, "the activity bar shows the check, not the last result")
    verify(child("activityCancel").visible)
    child("activityCancel").clicked()
    compare(svc.cancels, 1)
    svc.activity = Model.activityProgress(checkActivity(), { "event": "progress", "phase": "fetch", "message": "gh:o/foo",
      "current": 12, "total": 42, "target": null, "ref": "gh:o/foo" }, Date.now())
    compare(child("progressLine").text, "Checking for updates (12 of 42)")
    svc.activity = null
    verify(child("result").visible, "the last result comes back while it is held")
  }

  function test_the_last_result_leaves_when_its_hold_ends() {
    svc.lastResult = { "ok": true, "line": "Updated 1 addon", "partial": false, "commands": [] }
    verify(child("result").visible)
    svc.resultShown = false
    verify(!child("result").visible)
    verify(child("freshness").visible, "the footer stays")
  }

  function test_a_check_the_full_network_lane_drops_says_so() {
    svc.checkAccepted = false
    keyClick("c")
    compare(svc.checks, 1)
    compare(child("notice").text, Model.copy("activity.lane_full"))
  }

  function test_the_activity_bar_says_how_long_and_when_it_has_gone_quiet() {
    var a = checkActivity()
    a.startedAt = Date.now() - 45000
    a.lastEventAt = Date.now() - 35000
    svc.activity = a
    // Read at once (a new activity reads the clock), then each second.
    verify(child("activityElapsed").visible)
    verify(/^4[56] s$/.test(child("activityElapsed").text), child("activityElapsed").text)
    verify(/^Still working \(no news for 3[56] s\)$/.test(child("activityStalled").text), child("activityStalled").text)
    tryVerify(function() { return /^4[78] s$/.test(child("activityElapsed").text) }, 3000, "it ticks")
  }

  // A repair's Undo (G2) is the bound Undo last: shown only once a status read after the
  // repair names it as the last change, never the fix envelope's own `doctor undo` hint.
  function repairChange(run) {
    return Model.lastChangeOf({ "run": run, "kind": "repair", "at": "2026-11-08T07:23:33Z", "actor": "widget", "reason": "Repair",
      "targets": [], "changes": {}, "undo": { "command": "omaddons undo --run " + run, "argv": ["omaddons", "undo", "--run", run] } }, "ok")
  }

  function test_the_results_undo_of_a_repair_is_undo_last_once_status_names_it() {
    var run = "01M9RVNRVNRVNRVNRVNRVNRVNR"
    svc.lastChange = repairChange(run)
    var result = { "ok": true, "line": "Repaired: chmod 0600 <config>/credentials/github (was 0644)", "partial": false,
      "repair": true, "statusSeen": false, "fixes": [],
      "commands": [{ "action": "undo", "command": "omaddons doctor undo r", "argv": null, "runnable": false }] }
    svc.lastResult = result
    compare(child("resultLine").text, "Repaired: chmod 0600 <config>/credentials/github (was 0644)")
    verify(!child("resultUndo").visible, "no Undo before a status names the repair")
    var seen = JSON.parse(JSON.stringify(result))
    seen.statusSeen = true
    svc.lastResult = seen
    verify(child("resultUndo").visible)
    child("resultUndo").clicked()
    compare(json(svc.cliRuns[0]), json([["omaddons", "undo", "--run", run], "Undo"]))
    // Another change since: the result's Undo would not revert the repair.
    svc.lastChange = { "unknown": false, "kind": "update", "label": "Undo update on era",
      "undo": { "command": "omaddons undo --run 01J", "argv": ["omaddons", "undo", "--run", "01J"], "destructive": true, "runnable": true } }
    verify(!child("resultUndo").visible)
  }

  function test_undo_last_of_a_repair_reads_undo_repair() {
    var run = "01M9RVNRVNRVNRVNRVNRVNRVNR"
    svc.lastChange = repairChange(run)
    compare(child("undoButton").tooltipText, "Undo repair (Ctrl+Z)")
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    compare(json(svc.cliRuns[0]), json([["omaddons", "undo", "--run", run], "Undo repair"]))
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

  // Three clients here and one uninstalled (§2.2): the Dropdown lists the missing one
  // last, `t` and `T` step through the three only, and the trigger follows.
  function test_the_switcher_lists_every_client_and_t_steps_through_the_ones_here() {
    svc.targets = [
      { "id": "anniversary", "displayName": "Burning Crusade Anniversary", "missing": true, "running": false, "issues": [] },
      { "id": "era", "displayName": "Classic Era", "running": false, "updates": { "available": 1 }, "issues": [] },
      { "id": "forever", "displayName": "WoW: Forever", "running": false, "runningUnknown": true, "issues": [] },
      { "id": "retail", "displayName": "Retail", "running": true, "updates": { "available": 2 }, "issues": [] }
    ]
    var scope = child("scope")
    compare(json(scope.options.map(function(o) { return o.label })), json(["All clients · 3 updates", "Classic Era · 1 update",
      "WoW: Forever · can't tell if running", "Retail · 2 updates · running", "Burning Crusade Anniversary · not found"]))
    var seen = []
    for (var i = 0; i < 4; i++) {
      keyClick("t")
      seen.push([view.filter, scope.value, child("hero").title])
    }
    compare(json(seen), json([["era", "era", "Classic Era"], ["forever", "forever", "WoW: Forever"],
      ["retail", "retail", "Retail"], ["", "", "All clients"]]))
    keyClick("T")
    compare(scope.value, "retail")
    // The missing client is chosen in the Dropdown; from it, t goes back to All.
    scope.choose("anniversary")
    compare(child("hero").title, "Burning Crusade Anniversary")
    keyClick("t")
    compare(view.filter, "")
    // "N clients" counts the clients on this computer.
    compare(child("hero").detail, "3 clients")
  }

  // §2.2: a panel opens on the one running client; else on the scope a panel showed
  // last in this shell session, kept by the service for every bar's panel; else All.
  function test_the_panel_opens_on_the_running_client_else_the_last_scope_else_all() {
    var fresh = make({ "svc": svc })
    open(fresh)
    compare(fresh.filter, "retail", "the one client that runs")
    compare(svc.scope, "retail")
    fresh.opened = false
    svc.targets = svc.targets.map(function(t) { return Object.assign({}, t, { "running": false }) })
    fresh.setFilter("era")
    // Closed and opened again, and another bar's panel too: both on era.
    open(fresh)
    compare(fresh.filter, "era")
    var other = make({ "svc": svc })
    open(other)
    compare(other.filter, "era", "the service keeps the scope for every bar's panel")
    // A missing client is never a default scope.
    other.opened = false
    svc.targets = svc.targets.concat([{ "id": "classic", "displayName": "Classic", "missing": true, "running": false, "issues": [] }])
    other.setFilter("classic")
    open(other)
    compare(other.filter, "")
    // Two clients running: neither is chosen for you; the last scope stands.
    other.opened = false
    other.setFilter("era")
    svc.targets = svc.targets.map(function(t) { return Object.assign({}, t, { "running": !t.missing }) })
    open(other)
    compare(other.filter, "era")
  }

  // §2.2: a client forgotten or renamed while it is the scope: back to All. A status
  // that failed (no summary) says nothing about which clients exist.
  function test_a_client_that_is_gone_from_status_falls_back_to_all() {
    view.setFilter("era")
    var summary = svc.summary
    svc.summary = null
    svc.targets = [svc.targets[1]]
    compare(view.filter, "era", "no answer: the scope stands")
    svc.summary = summary
    svc.targets = svc.targets.slice()
    compare(view.filter, "")
    compare(child("hero").title, "All clients")
    // A client that went missing stays the scope: it says what happened to it.
    view.setFilter("retail")
    svc.targets = [Object.assign({}, svc.targets[0], { "missing": true })]
    compare(view.filter, "retail")
  }

  // `o` (§2.2): under All, the client of the row under the cursor, never a guess; the
  // scoped client's otherwise.
  function test_o_under_all_opens_the_cursored_rows_client_and_never_guesses() {
    view.viewComponents = { "updates": rowsView }
    compare(view.filter, "")
    keyClick("o")
    compare(Quickshell.detachedCommands.length, 0)
    compare(child("keyNote").text, "Choose a client first: t, or a row of one")
    verify(!child("openButton").enabled)
    // The first j highlights the era row, the second the retail row.
    keyClick("j")
    keyClick("j")
    compare([view.cursorActive, view.cursor], [true, 1])
    verify(child("openButton").enabled)
    keyClick("o")
    compare(json(Quickshell.detachedCommands[0]), json(["xdg-open", "/games/retail/Interface/AddOns"]))
    keyClick("k")
    keyClick("o")
    compare(json(Quickshell.detachedCommands[1]), json(["xdg-open", "/games/era/Interface/AddOns"]))
    // Under one client, that client's, whichever row the cursor is on.
    view.setFilter("retail")
    keyClick("o")
    compare(json(Quickshell.detachedCommands[2]), json(["xdg-open", "/games/retail/Interface/AddOns"]))
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
    svc.activity = changeActivity("Update all", null)
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
    // A plan sheet is the top layer.
    svc.confirm = { "sheet": planSheet("Remove Foo from Retail?"), "stale": "" }
    sheetHasKeys()
    keyClick(Qt.Key_Escape)
    compare([svc.no, view.pages.length, closed], [1, 1, 0], "Esc cancels the sheet only")
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
    keyClick("5")
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
    svc.confirm = { "sheet": planSheet("Remove Foo from Retail?"), "stale": "" }
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
    // Hyprland's animations off (Style.reduceMotion): it stands still, its tooltip
    // saying it checks (design doc §5.7, 1n2p.40).
    Style.reduceMotion = true
    try {
      svc.checking = true
      verify(button.spinning)
      compare(button.tooltipText, "Checking… (c)")
      wait(150)
      compare(button.rotation, 0)
    } finally {
      svc.checking = false
      Style.reduceMotion = false
    }
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
    // It opened on Retail, the one client that runs (§2.2): its 2 updates.
    compare(narrow.filter, "retail")
    compare(child("hero", narrow).meta, "2 UPDATES", "one part of the meta line")
    verify(!view.narrow, "540 is not narrow")
    verify(!child("viewTitle").visible)
  }

  // ------------------------------------------------------------- inside BarWidget

  function test_the_bar_widget_holds_the_frame_in_its_keyboard_panel() {
    Store.set(svc)
    var bar = barComponent.createObject(testCase, { "settings": { "panelWidth": 640 } })
    made = made.concat([bar])
    tryVerify(function() { return bar.svc === svc })
    var panel = child("panel", bar)
    var inner = child("view", bar)
    compare(panel.contentWidth, 640)
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
    compare(panel.contentWidth, 600)
  }
}
