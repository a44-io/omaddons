import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "../../Store.js" as Store

// The Updates and Installed views inside the frame (pipeline step 4). Against a stub
// service: rows, groups and markers, what Enter, `u`, `U`, `p`, `e` and `x` run (template and
// slots, and the argv Model.buildArgv makes of them), view-bound release notes and their
// links. Against the real Service on the mock Process: `x` previews the removal, Yes
// runs it bound to that preview, and a stale preview comes back instead of running.
TestCase {
  id: testCase
  name: "Views"
  when: windowShown
  visible: true
  width: 600
  height: 720

  property var svc: null
  property var view: null
  property var made: []

  // `list -v` as the binary prints it: two clients; retail runs.
  function addon(name, version, latest, status, extra) {
    var a = { "name": name, "version": version, "version_confidence": "exact", "ref": "gh:o/" + name.toLowerCase(),
      "channel": "stable", "pinned": false, "state": "ok", "reason": "explicit", "required_by": [], "orphan": false,
      "latest": { "version": latest, "status": status }, "folders": [name], "missing_folders": [],
      "drift": { "state": "clean", "files": 0, "folders": [] } }
    if (extra) for (var key in extra) a[key] = extra[key]
    return a
  }

  function listEnvelope(targets) {
    return { "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": "list", "exit_code": 0,
      "partial": false, "data": { "targets": targets }, "warnings": [], "commands": [] }
  }

  function listing() {
    return listEnvelope([
      { "id": "era", "key": "k-era", "label": "era", "counts": { "managed": 2, "unmanaged": 0 }, "addons": [
        addon("Bar", "v1", "v2", "update_available"),
        addon("Plater", "1.0", "1.1", "failed")
      ] },
      { "id": "retail", "key": "k-retail", "label": "retail", "counts": { "managed": 4, "unmanaged": 2 }, "addons": [
        addon("BigWigs", "v300", "v300", "current", { "reason": "dependency", "required_by": ["LittleWigs"] }),
        addon("DBM", "12.1.14", "12.1.15", "update_available"),
        addon("Foo", "v1", "v1", "current", { "drift": { "state": "modified", "files": 1, "folders": [] } }),
        addon("WeakAuras", "5.19.2", "5.20.0", "pinned", { "pinned": true })
      ] }
    ])
  }

  Component {
    id: serviceComponent
    QtObject {
      property string phase: "ok"
      readonly property bool canAct: phase === "ok"
      property var summary: null
      property var targets: [
        { "id": "era", "displayName": "Classic Era", "running": false, "runningUnknown": false, "clientDir": "/games/era",
          "updates": { "checkedAt": "2100-01-01T00:00:00Z" }, "issues": [] },
        { "id": "retail", "displayName": "Retail", "running": true, "runningUnknown": false, "clientDir": "/games/retail",
          "updates": { "checkedAt": "2100-01-01T00:00:00Z" }, "issues": [] }
      ]
      // The scope a panel showed last (§2.2), and the missing client card's Rescan.
      property string scope: ""
      property int rescans: 0
      function rescan() { rescans++; return true }
      property var lastChange: null
      property var action: null
      property string queuedAction: ""
      property var lastResult: null
      property var lastError: null
      property var confirm: null
      property string incompatibleText: ""
      property string unreachableText: ""
      property string binary: "/opt/omaddons-fixture/omaddons"
      property string requestedView: ""
      property var lists: null
      property var changelog: null
      property var errors: null
      property var statusWarnings: []
      property var machineIssues: []
      // `status`'s recommendations, reduced (the Issues view's suggestions, 1n2p.28).
      property var commands: []
      // An addon page's own read (1n2p.23): every readAddonInfo call, and its answer.
      property var addonInfo: null
      property var infoReads: []
      property int infoClears: 0
      function readAddonInfo(ref) {
        infoReads = infoReads.concat([ref])
        addonInfo = { "selection": { "ref": ref }, "data": null, "failed": "" }
        return true
      }
      function clearAddonInfo() { infoClears++; addonInfo = null }
      property var rawErrors: null
      // The doctor's explanation a problem row asked for, and every readExplanation call.
      property var explanation: null
      property var explains: []
      property int explainClears: 0
      property var search: null
      property var backups: null
      property var backupsDetail: null
      property var detailReads: []
      property int detailClears: 0
      property var folders: []
      // The panel's settings as the shell applied them (the settings sheet reads them).
      property var settings: ({})
      // Search's "Every client it works on" (omad-3rre): every installEach call.
      property var eachInstalls: []
      function installEach(ref, addon, ids) { eachInstalls = eachInstalls.concat([[ref, addon, ids]]); return "" }

      function readBackupsDetail(target) { detailReads = detailReads.concat([target]) }
      function clearBackupsDetail() { detailClears++; backupsDetail = null }
      function restoreFolder(addon) { folders = folders.concat([addon]); return "" }

      property var runs: []
      property var cliRuns: []
      property var notes: []
      property var reads: []
      property var rawReads: []
      property int rawClears: 0
      property var searches: []
      property var dismissals: []

      // `shown` (what the view showed, for a previewed update) is recorded when given.
      function run(name, slots, label, shown) { runs = runs.concat([shown === undefined ? [name, slots, label] : [name, slots, label, shown]]); return "" }
      function runCliArgv(argv, label) { cliRuns = cliRuns.concat([[argv, label]]); return "" }
      function readView(view) { reads = reads.concat([view]) }
      function readChangelog(target, addon) { notes = notes.concat([[target, addon]]) }
      function readRawErrors(target, addon) { rawReads = rawReads.concat([[target, addon]]) }
      function clearRawErrors() { rawClears++; rawErrors = null }
      function readExplanation(key, argv) { explains = explains.concat([[key, argv]]); return true }
      function clearExplanation() { explainClears++; explanation = null }
      function searchFor(query, target) { searches = searches.concat([[query, target]]) }
      // The catalog an empty Search box shows (1n2p.26): every readCatalog call, and the
      // answers by scope.
      property var catalogs: ({})
      property var catalogReads: []
      function readCatalog(target) { catalogReads = catalogReads.concat([target]); return true }
      function dismissErrors(plan) { dismissals = dismissals.concat([JSON.parse(JSON.stringify(plan))]); return "" }
      // The real copyCommand runs Model.copyArgv(text) and answers `done` with wl-copy's
      // outcome; this records the argv and answers `copyOk` (false: wl-copy missing).
      // Sharing (1n2p.37): what the view asked, and the answer it waits for.
      property var shareCopies: []
      property var sharePastes: []
      property var shareDone: null
      function copyAddonList(filter, done) { shareCopies = shareCopies.concat([filter]); shareDone = done; return true }
      function pasteAddonList(filter, done) { sharePastes = sharePastes.concat([filter]); shareDone = done; return true }
      property var copies: []
      property bool copyOk: true
      function copyCommand(text, done) {
        if (!Model.copyArgv(text)) return false
        copies = copies.concat([Model.copyArgv(text)])
        if (typeof done === "function") done(copyOk)
        return true
      }
      // What a change runs on and just changed (1n2p.18): the rows' own phase.
      property var activity: null
      property var doneRows: ({})
      // What Model.viewState reads (1n2p.19), and the views' Try again and Check now.
      property var reading: []
      property var outcomes: ({})
      property var lastCheck: null
      // The settings sheet's GitHub section and config.toml line (omad-r0to).
      property var github: null
      property var config: null
      property bool checking: false
      property int checks: 0
      property var readAgains: []
      function readAgain(view) { readAgains = readAgains.concat([view]); return true }
      function check() { checks++ }
      function refresh() {}
      function reinstallWidget() { return "ok" }
      function confirmYes() { confirm = null; return "" }
      function confirmNo() { confirm = null }
      // What BarWidget tells its service (omad-zppa's tests host the frame in it).
      function applySettings(next) {}
      function setPanelOpen(key, open) {}
    }
  }

  // The bar button, its keyboard panel sized as Omarchy's kit sizes it (the stub
  // KeyboardPanel: the card's padding and border around the frame), the frame inside.
  Component {
    id: barComponent
    BarWidget {}
  }

  // A view of `fractionalHeight`, as a view of text rows often measures.
  property real fractionalHeight: 300
  Component {
    id: fractionalView
    Item {
      property var frame: null
      property int rowCount: 0
      property bool typing: false
      implicitHeight: testCase.fractionalHeight
      function handleKey(name, cursor) { return false }
    }
  }

  Component {
    id: frameComponent
    PanelView {
      width: 540
      height: 700
      // As BarWidget hands down the bar's cap: here the panel's own height.
      heightCap: 700
    }
  }

  Component {
    id: realService
    Service {}
  }

  function make(service) {
    var v = frameComponent.createObject(testCase, { "svc": service })
    verify(v !== null)
    made = made.concat([v])
    v.opened = true
    // Retail runs here, so the panel opened on it (design doc §2.2); these tests start
    // under All, as tst_panel.qml's do.
    v.setFilter("")
    v.focusItem.forceActiveFocus()
    tryVerify(function() { return v.focusItem.activeFocus })
    return v
  }

  function init() {
    // A binding loop keeps a stale value for its pass (omad-5k9k: the page stack's top
    // page behind its own stack): every one fails the test that meets it.
    failOnWarning(/Binding loop detected/)
    Quickshell.resetDetachedCommands()
    svc = serviceComponent.createObject(testCase)
    svc.lists = Model.reduceList(listing())
    view = make(svc)
  }

  function cleanup() {
    for (var i = 0; i < made.length; i++) made[i].destroy()
    made = []
    // A test that hosted the frame in BarWidget handed it the service through the store.
    Store.clear(svc)
    if (svc) svc.destroy()
    svc = null
    view = null
    ProcessRegistry.reset()
    wait(0)
  }

  function json(value) {
    return JSON.stringify(value)
  }

  function content() {
    var item = findChild(view, "content").item
    verify(item !== null, "a loaded view")
    return item
  }

  function row(name, index) {
    var found = findChild(content(), name + "_" + index)
    verify(found !== null, "no " + name + "_" + index)
    return found
  }

  // What a row of `listing()` showed, in Model.shownOf's fields and order (the listing
  // names no release: its source, release and file are unknown).
  function shownRow(key, target, addon, from, to) {
    return { "target_key": key, "target_id": target, "addon": addon, "source": null, "channel": "stable",
      "release_id": null, "asset": null, "from_version": from, "to_version": to }
  }

  // The command words of the argv Model.buildArgv makes of a recorded run.
  function words(run) {
    // A previewed template builds as its preview (the dry run before its plan's apply).
    var built = Model.buildArgv(run[0], run[1], { "binary": "/opt/omaddons", "label": run[2], "context": Build.CONTEXT,
      "preview": !!Model.COMMANDS[run[0]].preview })
    var argv = built.argv
    var at = argv.indexOf("--reason") + 2
    while (argv[at] === "--progress" || argv[at] === "--dry-run" || argv[at] === "--if-match") at += argv[at] === "--dry-run" ? 1 : 2
    return argv.slice(at)
  }

  // ------------------------------------------------------------- Updates

  // The open (1n2p.38): with what was read before, a panel shows its rows at once,
  // before any read answers, within a generous budget (the baseline's is 50 ms).
  function test_an_open_with_cached_data_shows_its_rows_at_once() {
    var started = Date.now()
    var opened = make(svc)
    tryVerify(function() { return !!opened.currentView && opened.currentView.rowCount > 0 && opened.currentView.rowItem(0) !== null },
      2000, "the open shows the rows read before")
    var elapsed = Date.now() - started
    verify(elapsed < 1000, "rows shown in " + elapsed + " ms")
  }

  // 1n2p.38: every view the frame makes has its frame from creation, so each binding
  // that reads it runs once with it (given on load, the Updates view built its rows
  // five times on a first visit: 155 ms at 500 rows).
  function test_every_view_has_its_frame_from_creation() {
    var names = Object.keys(view.viewComponents)
    compare(names.length, Model.VIEWS.length)
    for (var i = 0; i < names.length; i++) {
      var item = view.viewComponents[names[i]].createObject(null)
      verify(item !== null, names[i])
      compare(item.frame, view, names[i] + " has its frame at creation")
      item.destroy()
    }
  }

  // The rows a long list has built (its delegates now, kept or reused).
  function builtRows(list, prefix) {
    var children = list.contentItem.children
    var built = 0
    for (var i = 0; i < children.length; i++) if (String(children[i].objectName).indexOf(prefix) === 0 && children[i].visible) built++
    return built
  }

  // 500 addons (1n2p.38): Addons builds only the rows in view, within its budget, and
  // keeps the cursor's row built and in view as it moves; a short list stays plain.
  function test_five_hundred_addons_build_fewer_than_sixty_rows() {
    var addons = []
    for (var i = 0; i < 500; i++) addons.push(addon("Addon" + (1000 + i), "v1", "v1", "current"))
    svc.lists = Model.reduceList(listEnvelope([{ "id": "retail", "key": "k-retail", "label": "retail", "counts": { "unmanaged": 0 },
      "addons": addons }]))
    var started = Date.now()
    keyClick("2")
    var installed = view.currentView
    compare(installed.rowCount, 500)
    var list = findChild(installed, "rowList")
    // The list lays out its rows on its next polish.
    tryVerify(function() { return builtRows(list, "installedRow_") > 0 }, 3000, "the list built its first rows")
    var elapsed = Date.now() - started
    verify(installed.scrollsItself, "a long list scrolls itself")
    var built = builtRows(list, "installedRow_")
    verify(built > 0 && built < 60, built + " rows built for 500")
    // Generous: the budget (design doc, "Costs, baseline": 100 ms at 500 rows) on a
    // loaded test machine.
    verify(elapsed < 1500, "Addons shown in " + elapsed + " ms")
    // G: the last row, built and in view; the frame's own content does not scroll.
    keyClick("G")
    compare(view.cursor, 499)
    tryVerify(function() { return installed.rowItem(499) !== null }, 2000, "the cursor's row is built")
    // In view once the list has laid out the move.
    tryVerify(function() {
      var last = installed.rowItem(499)
      var y = last ? last.mapToItem(list, 0, 0).y : -1
      return !!last && y >= 0 && y + last.height <= list.height + 1
    }, 2000, "the cursor's row is in view")
    verify(builtRows(list, "installedRow_") < 60, "still fewer than 60 rows built")
    // Back to a short list: every row built, the frame scrolling as before.
    svc.lists = Model.reduceList(listing())
    verify(!installed.scrollsItself)
    compare(installed.rowItem(0) !== null, true)
  }

  // The frame in BarWidget's keyboard panel, opened under All, as make() opens one: the
  // card sized as Omarchy's kit sizes it, its padding and border around the frame.
  function openInBar() {
    Store.set(svc)
    var bar = barComponent.createObject(testCase)
    verify(bar !== null)
    made = made.concat([bar])
    tryVerify(function() { return bar.svc === svc }, 2000, "the bar binds the service")
    verify(findChild(bar, "panel").verticalContentInset > 0, "the card has the kit's padding and border")
    bar.open()
    var frame = findChild(bar, "view")
    tryVerify(function() { return frame.opened })
    frame.setFilter("")
    return bar
  }

  // The frame's content at `zoom`, settled: whether it fits the frame's view, the
  // overflow said in the message.
  function frameFits(frame, zoom, what) {
    frame.uiScale = zoom
    verify(waitForPolish(testCase.Window.window), "the window's layout settles")
    var flick = findChild(frame, "contentFlick")
    verify(flick.contentHeight <= flick.height, what + " at zoom " + zoom + ": the frame's content fits ("
      + flick.contentHeight + " in " + flick.height + ")")
    verify(!flick.interactive, what + " at zoom " + zoom + ": the frame's content does not scroll")
    return flick
  }

  // omad-zppa: in the bar's keyboard panel, a long list fills the frame's room and no
  // more. The kit's sizes are the card's (padding and border included); read as the
  // frame's, they made a long list 28 px too tall, so the frame scrolled too, its scroll
  // bar showed over the list's on hover, and the list's end sat under the footer.
  function test_a_long_list_in_the_bar_widgets_panel_leaves_the_frame_nothing_to_scroll() {
    var addons = []
    for (var i = 0; i < 500; i++) addons.push(addon("Addon" + (1000 + i), "v1", "v1", "current"))
    svc.lists = Model.reduceList(listEnvelope([{ "id": "retail", "key": "k-retail", "label": "retail", "counts": { "unmanaged": 0 },
      "addons": addons }]))
    var bar = openInBar()
    var frame = findChild(bar, "view")
    frame.showView("installed")
    var installed = frame.currentView
    verify(installed.scrollsItself, "a long list scrolls itself")
    var list = findChild(installed, "rowList")
    for (var zoom of [1.0, 1.1, 1.3]) {
      var flick = frameFits(frame, zoom, "500 addons")
      var end = list.mapToItem(flick, 0, list.height).y
      verify(end <= flick.height, "at zoom " + zoom + ": the list ends inside the frame's view (" + end + " of " + flick.height + ")")
      verify(findChild(bar, "panel").contentHeight <= 720, "at zoom " + zoom + ": the card keeps its cap")
    }
  }

  // omad-zppa: the kit rounds the card's height, and a frame a fraction of a pixel short
  // scrolls its content by that fraction (and shows a scroll bar on hover). The panel
  // asks for whole pixels up, so a view of any fractional height fits.
  function test_a_view_of_fractional_height_fits_the_bar_widgets_panel() {
    var bar = openInBar()
    var frame = findChild(bar, "view")
    frame.viewComponents = { "updates": fractionalView }
    frame.showView("updates")
    for (var eighth = 1; eighth < 8; eighth++) {
      fractionalHeight = 300 + eighth / 8
      for (var zoom of [1.0, 1.1]) frameFits(frame, zoom, "a view of " + fractionalHeight + " px")
    }
  }

  // 1n2p.38: Qt's engine spent about a millisecond a row on Updates rows without a
  // release date (Date.parse of a text it cannot read: 480 ms at 500 rows). Now only an
  // RFC 3339 instant reaches it, and 500 such rows build well inside the view's budget.
  function test_five_hundred_updates_without_release_dates_build_within_budget() {
    var addons = []
    for (var i = 0; i < 500; i++) addons.push(addon("Addon" + (1000 + i), "v1", "v2", "update_available"))
    var lists = Model.reduceList(listEnvelope([{ "id": "retail", "key": "k-retail", "label": "retail", "counts": { "unmanaged": 0 },
      "addons": addons }]))
    var started = Date.now()
    var rows = Model.updateRows(lists, "", svc.targets, { "nowMs": Date.now(), "done": null })
    var elapsed = Date.now() - started
    compare(rows.length, 500)
    compare(rows[0].meta.indexOf("old"), -1, "no release date, no age")
    verify(elapsed < 200, "500 update rows built in " + elapsed + " ms")
  }

  // 150 updates (1n2p.38; the cost baseline: Updates built 786 ms at 150 rows): only the
  // rows in view are built, the view is shown within a generous budget, and the cursor's
  // row is kept in view.
  function test_a_hundred_and_fifty_updates_build_only_the_rows_in_view() {
    var addons = []
    for (var i = 0; i < 150; i++) addons.push(addon("Addon" + (1000 + i), "v1", "v2", "update_available"))
    keyClick("2")
    svc.lists = Model.reduceList(listEnvelope([{ "id": "retail", "key": "k-retail", "label": "retail", "counts": { "unmanaged": 0 },
      "addons": addons }]))
    var started = Date.now()
    keyClick("1")
    var updates = view.currentView
    compare(updates.rowCount, 150)
    var list = findChild(updates, "rowList")
    tryVerify(function() { return builtRows(list, "updateRow_") > 0 }, 3000, "the list built its first rows")
    var elapsed = Date.now() - started
    verify(updates.scrollsItself)
    verify(builtRows(list, "updateRow_") < 60, builtRows(list, "updateRow_") + " rows built for 150")
    verify(elapsed < 1500, "Updates shown in " + elapsed + " ms")
    keyClick("G")
    compare(view.cursor, 149)
    tryVerify(function() {
      var last = updates.rowItem(149)
      var y = last ? last.mapToItem(list, 0, 0).y : -1
      return !!last && y >= 0 && y + last.height <= list.height + 1
    }, 2000, "the cursor's row is in view")
  }

  function test_updates_are_grouped_by_client_with_the_count_U_runs() {
    var updates = content()
    compare(json(updates.rows.map(function(r) { return [r.group, r.groupCount, r.name, r.kind] })),
      json([["Classic Era", "1 update", "Bar", "update"], ["", "", "Plater", "failed"],
        ["Retail", "running · 1 update", "DBM", "update"], ["", "", "WeakAuras", "pinned"]]))
    // Each client's header: its name, whether it runs, its count (§2.2); its rows name
    // no client.
    var header = findChild(row("updateRow", 2), "clientHeader")
    verify(header.visible)
    compare([header.title, header.count], ["Retail", "running · 1 update"])
    verify(!findChild(row("updateRow", 3), "clientHeader").visible)
    // Two clients have updates: Update all does what no client header does.
    compare(updates.clientsWithUpdates, 2)
    verify(findChild(updates, "updateAll").visible)
    compare(findChild(updates, "updateAll").text, "Update all (2)")
    // A running client's rows say so in words (the reload words come from the CLI's
    // own preview, bmcx.5); each row is an AddonRow with its versions.
    compare(findChild(findChild(row("updateRow", 2), "addonRow"), "meta").text, "Retail is running")
    verify(findChild(findChild(row("updateRow", 0), "addonRow"), "meta").text.indexOf("running") === -1)
    compare(findChild(row("updateRow", 2), "addonRow").toVersion, "12.1.15")
    // Each client's header updates that client.
    var clientUpdate = findChild(row("updateRow", 2), "updateClient")
    verify(clientUpdate.visible)
    compare(clientUpdate.text, "Update Retail")
    verify(!findChild(row("updateRow", 3), "updateClient").visible)
    // Update all sits at the right, above the clients' buttons; its tooltip names `U`.
    var updateAll = findChild(updates, "updateAll")
    compare(updateAll.mapToItem(updates, updateAll.width, 0).x, clientUpdate.mapToItem(updates, clientUpdate.width, 0).x)
    compare(updateAll.tooltipText, Model.keyTip("update_all", "updates"))
    verify(updateAll.tooltipText.endsWith("(U)"))
    var underAll = clientUpdate.mapToItem(updates, 0, 0).x
    keyClick("t")
    keyClick("t")
    compare(view.filter, "retail")
    // Under one client its header shows as under All (omad-224l): Update Retail by its
    // player name, never its id (UX-38), where it was; no Update all.
    verify(!findChild(content(), "updateAll").visible)
    verify(findChild(row("updateRow", 0), "clientHeader").visible)
    var lone = findChild(row("updateRow", 0), "updateClient")
    verify(lone.visible)
    compare(lone.text, "Update Retail")
    compare(lone.mapToItem(content(), 0, 0).x, underAll, "the button keeps its place")
    lone.clicked()
    // The update previews with what the rows showed: retail's one update.
    compare(json(svc.runs[0]), json(["update_target", { "target": "retail" }, "Update Retail",
      [shownRow("k-retail", "retail", "DBM", "12.1.14", "12.1.15")]]))
    // `U` under one client does the same.
    keyClick("U")
    compare(json(svc.runs[1]), json(svc.runs[0]))
  }

  function test_u_updates_the_cursored_addon_and_says_so_where_nothing_waits() {
    // Updates: era's Bar, under the cursor.
    keyClick("u")
    compare(json(svc.runs[0]), json(["update_addon", { "target": "era", "addon": "Bar" }, "Update Bar",
      [shownRow("k-era", "era", "Bar", "v1", "v2")]]))
    // Backups binds no `u`: the key says why, and runs nothing.
    keyClick("6")
    keyClick("u")
    compare(svc.runs.length, 1)
    compare(findChild(view, "toastLine").text, "Nothing to update here")
    // The next key clears it.
    keyClick("j")
    compare(view.keyNote, "")
    verify(!findChild(view, "toast").showing)
  }

  function test_u_updates_one_addon_and_p_pins_by_its_state() {
    keyClick("u")
    compare(json(svc.runs[0]), json(["update_addon", { "target": "era", "addon": "Bar" }, "Update Bar",
      [shownRow("k-era", "era", "Bar", "v1", "v2")]]))
    compare(json(words(svc.runs[0])), json(["update", "-t", "era", "--", "Bar"]))
    // A failed check has nothing to update: Plater. The first j highlights Bar, the next
    // moves to Plater.
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick("u")
    compare(svc.runs.length, 1)
    keyClick("j")
    keyClick("j")
    keyClick("p")
    compare(json(svc.runs[1]), json(["unpin", { "target": "retail", "addon": "WeakAuras" }, "Unpin WeakAuras"]))
    keyClick("k")
    keyClick("p")
    compare(json(svc.runs[2]), json(["pin", { "target": "retail", "addon": "DBM" }, "Pin DBM"]))
    // While a change runs, the rows run nothing.
    svc.action = { "label": "Update Bar", "current": 0, "total": 0, "message": "" }
    keyClick("u")
    compare(svc.runs.length, 3)
  }

  // Enter on an Updates row opens the addon's page (1n2p.23); back returns to the row.
  function test_enter_on_an_update_opens_its_page_and_back_returns_to_the_row() {
    keyClick("j")
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 2)
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 0, "Enter updates nothing")
    compare(view.pages.length, 1)
    compare(view.backText(), "Updates")
    var page = findChild(view, "page").item
    compare([page.name, page.ref], ["DBM", "gh:o/dbm"])
    compare(json(svc.infoReads), json(["gh:o/dbm"]))
    compare(findChild(page, "pageTitle").text, "DBM")
    keyClick(Qt.Key_Escape)
    compare(view.pages.length, 0)
    compare([view.cursor, view.cursorActive], [2, true])
    compare(svc.infoClears, 1)
  }

  function test_e_shows_every_pending_note_of_its_own_row_and_safe_links_only() {
    keyClick("j")
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 2)
    keyClick("e")
    compare(json(svc.notes), json([["retail", "DBM"]]))
    var entry = row("updateRow", 2)
    verify(findChild(entry, "notes").visible)
    compare(findChild(entry, "notesStatus").text, "Reading the release notes…")
    // A late answer for another row is never shown here.
    svc.changelog = { "selection": { "target": "era", "addon": "Bar" }, "data": { "changelog": { "entries": [{ "version": "v2", "text": "Bar notes" }], "more_url": "https://github.com/o/bar/releases" } } }
    compare(findChild(entry, "notesStatus").text, "Reading the release notes…")
    // Every release since the installed one, newest first, and how far behind it is.
    svc.changelog = { "selection": { "target": "retail", "addon": "DBM" }, "data": { "changelog": { "truncated": false,
      "entries": [{ "version": "12.1.16", "text": "Adds the new raid." }, { "version": "12.1.15", "text": "Fixes the pull timer." }],
      "more_url": "https://github.com/o/dbm/releases" } } }
    compare(findChild(entry, "notesStatus").text, "2 releases behind")
    compare(findChild(findChild(entry, "noteEntry_0"), "noteVersion").text, "12.1.16")
    compare(findChild(findChild(entry, "noteEntry_1"), "noteText").text, "Fixes the pull timer.")
    var link = findChild(entry, "notesLink")
    verify(link.visible)
    compare(link.text, "More on GitHub")
    link.clicked()
    compare(json(Quickshell.detachedCommands[0]), json(["omarchy-launch-browser", "https://github.com/o/dbm/releases"]))
    svc.changelog = { "selection": { "target": "retail", "addon": "DBM" }, "data": { "changelog": { "entries": [], "more_url": "http://evil.example/x" } } }
    verify(!findChild(entry, "notesLink").visible)
    keyClick("e")
    verify(!findChild(entry, "notes").visible)
  }

  // One client behind under All (omad-sj3y): its header's Update {client} is the one bulk
  // button; an Update all beside it would do exactly the same, so the view's header and
  // its room go. The other client's failed check still offers Check again.
  function test_one_client_behind_under_all_has_only_its_headers_update() {
    svc.lists = Model.reduceList(listEnvelope([
      { "id": "era", "key": "k-era", "label": "era", "counts": { "managed": 1, "unmanaged": 0 }, "addons": [
        addon("Plater", "1.0", "1.1", "failed")
      ] },
      { "id": "retail", "key": "k-retail", "label": "retail", "counts": { "managed": 2, "unmanaged": 0 }, "addons": [
        addon("DBM", "12.1.14", "12.1.15", "update_available"),
        addon("Details", "v1", "v2", "update_available")
      ] }
    ]))
    var updates = content()
    compare(updates.count, 2)
    compare(updates.clientsWithUpdates, 1)
    verify(!findChild(updates, "updateAll").visible)
    verify(!findChild(updates, "updatesHeader").visible)
    compare(findChild(updates, "updatesHeader").height, 0)
    var clientUpdate = findChild(row("updateRow", 1), "updateClient")
    verify(clientUpdate.visible)
    compare(clientUpdate.text, "Update Retail")
    verify(!findChild(row("updateRow", 0), "updateClient").visible, "era has nothing to update")
    // `U` still updates the whole scope, previewed with Retail's two rows.
    keyClick("U")
    compare(svc.runs.length, 1)
    compare(svc.runs[0][0], "update_all")
    compare(svc.runs[0][3].length, 2)
  }

  // Checking is the hero's (omad-sj3y): the view has no Check now and no "Checked …" of
  // its own (the footer says when), and `c` still checks from it.
  function test_the_view_leaves_checking_to_the_hero_and_c_still_checks() {
    var updates = content()
    compare(findChild(updates, "checkNow"), null)
    compare(findChild(updates, "checkedLabel"), null)
    keyClick("c")
    compare(svc.checks, 1)
    // A failed check's row offers Check again, never an update.
    var failed = findChild(row("updateRow", 1), "addonRow")
    compare(failed.meta, "Couldn't check GitHub for a new release")
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick("u")
    compare(svc.runs.length, 0)
  }

  // `/` filters the updates as the Addons view does, the count beside it.
  function test_slash_filters_the_updates() {
    var updates = content()
    var field = findChild(updates, "filterField")
    keyClick("/")
    tryVerify(function() { return field.activeFocus })
    verify(updates.typing)
    keyClick("d")
    keyClick("b")
    compare(json(updates.rows.map(function(r) { return r.name })), json(["DBM"]))
    compare(findChild(updates, "filterCount").text, "1 of 4")
    // Update all still updates the whole scope, not only the matches.
    compare(findChild(updates, "updateAll").text, "Update all (2)")
    keyClick(Qt.Key_Escape)
    tryVerify(function() { return !field.activeFocus })
    compare(updates.query, "db")
    keyClick(Qt.Key_Escape)
    compare(updates.query, "")
    compare(updates.rows.length, 4)
  }

  // While a change runs on a row, the row says its own phase; just after, "Updated".
  function test_a_row_shows_its_own_phase_while_its_update_runs() {
    var started = Model.activityStart({ "id": 1, "kind": "mutation", "lane": "mutation", "priority": "user", "template": "update_addon",
      "slots": { "target": "retail", "addon": "DBM" }, "label": "Update DBM" }, Date.now(), svc.lists)
    svc.activity = Model.activityProgress(started, { "event": "progress", "phase": "download", "message": "", "current": 1,
      "total": 1, "target": "retail", "ref": "gh:o/dbm" }, Date.now())
    var dbm = findChild(row("updateRow", 2), "addonRow")
    verify(dbm.pending)
    compare(dbm.pendingText, "Updating… downloading (1 of 1)")
    verify(!findChild(row("updateRow", 0), "addonRow").pending, "another row is not moved")
    svc.activity = null
    svc.doneRows = { "retail/DBM": "Updated" }
    verify(!dbm.pending)
    compare(dbm.meta, "Updated")
  }

  function test_nothing_to_update_says_when_it_was_checked() {
    svc.lists = Model.reduceList(listEnvelope([{ "id": "era", "key": "k", "label": "era", "counts": { "unmanaged": 0 },
      "addons": [addon("Bar", "v2", "v2", "current")] }]))
    view.nowMs = Date.parse("2100-01-01T00:25:00Z")
    var empty = findChild(content(), "stateEmpty")
    verify(empty.visible)
    compare(empty.sentence, "All up to date. Checked 25 min ago.")
    verify(!findChild(content(), "updateAll").visible)
  }

  // ------------------------------------------------------------- the views' states (1n2p.19)

  // The rows stay while the list is read again, with a quiet line saying so.
  function test_a_refresh_never_blanks_the_updates() {
    svc.reading = ["list"]
    var state = findChild(content(), "viewState")
    verify(findChild(state, "stateRefreshing").visible, "refreshing says so")
    verify(!findChild(state, "stateEmpty").visible)
    compare(content().rows.length, 4, "the rows stay")
    verify(findChild(content(), "updateRow_0").visible)
    svc.reading = []
    verify(!findChild(state, "stateRefreshing").visible)
  }

  // A check that could not reach a source keeps the rows, says so, and Check now checks.
  function test_a_check_whose_source_did_not_answer_keeps_the_rows_with_check_now() {
    svc.lastCheck = { "at": "2100-01-01T00:00:00Z", "trouble": true, "failed": 0, "outcome": "ok" }
    var notices = findChild(findChild(content(), "viewState"), "stateNotices")
    compare(notices.count, 1)
    var notice = notices.itemAt(0)
    compare(notice.title, "Some sources didn't answer the last check; what they said before is shown.")
    verify(findChild(content(), "updateRow_0").visible, "the rows stay")
    notice.clicked("check")
    compare(svc.checks, 1, "Check now checks")
  }

  // No list yet: loading; a list that could not be read: the sentence and Try again,
  // which reads it again.
  function test_a_first_load_then_a_failed_read_offers_try_again() {
    svc.lists = null
    var state = findChild(content(), "viewState")
    var empty = findChild(state, "stateEmpty")
    verify(empty.visible)
    compare(empty.sentence, "Reading your addons…")
    compare(empty.actionText, "")
    compare(content().rows.length, 0)
    svc.outcomes = { "list": "failed" }
    compare(empty.sentence, "Couldn't read your addons.")
    compare(empty.actionText, "Try again")
    empty.action()
    compare(json(svc.readAgains), json(["updates"]), "Try again reads the view again")
  }

  // Search: the empty box says what to type; a search that failed asks again; skipped
  // sources say why, over the results that came.
  function test_search_says_what_to_type_and_why_sources_were_skipped() {
    keyClick("3")
    var search = content()
    var state = findChild(search, "viewState")
    compare(findChild(state, "stateEmpty").sentence, "Type a name, or paste a link from GitHub, WoWInterface, Tukui or WowAce.")
    findChild(search, "searchField").text = "dbm"
    compare(findChild(state, "stateEmpty").sentence, "Searching…")
    tryVerify(function() { return svc.searches.length === 1 })
    svc.outcomes = { "search": "failed" }
    compare(findChild(state, "stateEmpty").sentence, "The search didn't finish.")
    findChild(state, "stateEmpty").action()
    compare(json(svc.searches), json([["dbm", ""], ["dbm", ""]]), "Try again searches again")
    svc.outcomes = { "search": "ok" }
    svc.search = { "query": "dbm", "rows": [{ "name": "DBM", "ref": "dbm", "source": "catalog", "compat": "", "hints": {} }],
      "skipped": [{ "source": "github", "reason": "offline" }, { "source": "wowi", "reason": "offline" }],
      "selection": { "query": "dbm", "target": "" } }
    verify(!findChild(state, "stateEmpty").visible, "results: no empty state")
    compare(findChild(state, "stateNotices").count, 1)
    compare(findChild(state, "stateNotices").itemAt(0).title, "GitHub and WoWInterface couldn't be searched: offline.")
    compare(search.rowCount, 1)
  }

  // ------------------------------------------------------------- Installed

  function test_installed_rows_carry_their_markers_repair_and_open_their_page() {
    keyClick("2")
    var installed = content()
    // Each client's problems first (Foo's files changed), then its waiting updates, then
    // the rest by name (1n2p.22).
    compare(json(installed.rows.map(function(r) { return [r.group, r.name, r.markers, r.primary] })), json([
      ["Classic Era", "Bar", [], "update"], ["", "Plater", [], ""],
      ["Retail", "Foo", ["modified"], "reinstall"], ["", "DBM", [], "update"], ["", "BigWigs", ["dep"], ""], ["", "WeakAuras", ["pinned"], ""]]))
    // The state in words on an AddonRow: chips, the source and what needs it.
    var bigwigs = findChild(row("installedRow", 4), "addonRow")
    compare(json([bigwigs.title, bigwigs.chips.map(function(c) { return c.text }), bigwigs.meta]),
      json(["BigWigs", ["dependency"], "GitHub · needed by LittleWigs"]))
    var foo = findChild(row("installedRow", 2), "addonRow")
    compare(json([foo.band, foo.chips[0].text, foo.chips[0].urgent, foo.meta]), json(["urgent", "changed on disk", true, "GitHub · 1 file changed on disk"]))
    var dbm = findChild(row("installedRow", 3), "addonRow")
    compare([dbm.fromVersion, dbm.toVersion, dbm.band], ["12.1.14", "12.1.15", "accent"])
    // Foo: its first button, Repair, puts back its modified files.
    keyClick("j")
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 2)
    verify(foo.cursored)
    compare(json(foo.actions.map(function(a) { return a.name })), json(["repair", "pin", "restore", "remove"]))
    findChild(foo, "action_repair").clicked()
    compare(json(svc.runs[0]), json(["reinstall", { "target": "retail", "addon": "Foo" }, "Reinstall Foo"]))
    compare(json(words(svc.runs[0])), json(["install", "-t", "retail", "--reinstall", "--", "Foo"]))
    // Enter opens its page (1n2p.23), where Enter on its line repairs it too.
    keyClick(Qt.Key_Return)
    compare(view.pages.length, 1)
    compare(view.backText(), "Addons")
    var page = findChild(view, "page").item
    compare(json(page.rows.map(function(l) { return [l.client, l.primary] })), json([["Retail", "reinstall"], ["Classic Era", ""]]))
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[1]), json(svc.runs[0]))
    keyClick(Qt.Key_Escape)
    compare(view.cursor, 2, "back to Foo's row")
    // BigWigs: nothing waits, so `u` does nothing.
    keyClick("j")
    keyClick("j")
    keyClick("u")
    compare(svc.runs.length, 2)
    keyClick("x")
    compare(json(svc.runs[2]), json(["remove", { "target": "retail", "addon": "BigWigs" }, "Remove BigWigs"]))
    keyClick("p")
    compare(json(svc.runs[3]), json(["pin", { "target": "retail", "addon": "BigWigs" }, "Pin BigWigs"]))
    // `u` updates only an addon a release waits for: nothing for BigWigs, DBM's for DBM.
    keyClick("u")
    compare(svc.runs.length, 4)
    keyClick("k")
    keyClick("u")
    compare(json(svc.runs[4].slice(0, 3)), json(["update_addon", { "target": "retail", "addon": "DBM" }, "Update DBM"]))
    // A row's button acts on its row: DBM's Update.
    findChild(dbm, "action_update").clicked()
    compare(json(svc.runs[5].slice(0, 3)), json(["update_addon", { "target": "retail", "addon": "DBM" }, "Update DBM"]))
  }

  // `/` (1n2p.22): the filter box takes the keys, "n of m" beside it, urgent at none;
  // Down moves the list's cursor, Enter goes to the first match, Esc leaves the box,
  // and Esc again clears the filter with the cursor kept on its addon.
  function test_slash_filters_the_addons_and_esc_drops_it_keeping_the_cursor() {
    keyClick("2")
    var installed = content()
    var field = findChild(installed, "filterField")
    verify(!installed.typing)
    keyClick("/")
    tryVerify(function() { return field.activeFocus })
    verify(installed.typing)
    keyClick("w")
    keyClick("a")
    compare(installed.query, "wa")
    // WeakAuras by name, and Bar ("WoWInterface"? no: GitHub) left out.
    compare(json(installed.rows.map(function(r) { return r.name })), json(["WeakAuras"]))
    compare(findChild(installed, "filterCount").text, "1 of 6")
    keyClick("z")
    compare(findChild(installed, "filterCount").text, "0 of 6")
    verify(findChild(installed, "filterCount").font.bold, "urgent at none")
    verify(!findChild(installed, "stateEmpty").visible, "no empty sentence: the count says it")
    keyClick(Qt.Key_Backspace)
    // Enter: the first match, and the box gives the keys back.
    keyClick(Qt.Key_Return)
    tryVerify(function() { return !installed.typing })
    compare([view.cursor, view.cursorActive], [0, true])
    compare(installed.rows[0].name, "WeakAuras")
    // Esc in the view: the filter goes, the cursor stays on WeakAuras.
    keyClick(Qt.Key_Escape)
    compare(installed.query, "")
    compare(installed.rows[view.cursor].name, "WeakAuras")
    compare(view.cursor, 5)
    verify(view.opened, "Esc cleared the filter; it did not close the panel")
    // In the box, Down moves the list's cursor without leaving the box.
    keyClick("/")
    tryVerify(function() { return field.activeFocus })
    keyClick("b")
    keyClick(Qt.Key_Down)
    verify(field.activeFocus)
    compare(view.cursorActive, true)
    // Esc leaves the box, keeping its text.
    keyClick(Qt.Key_Escape)
    tryVerify(function() { return !field.activeFocus })
    compare(installed.query, "b")
    // `/` where nothing filters (Backups) says why it does nothing.
    keyClick("6")
    keyClick("/")
    compare(findChild(view, "toastLine").text, "Nothing to filter here")
  }

  // `S` (1n2p.22): by name, by recently updated, by source, and back; the cursor stays
  // on the same addon each time.
  function test_S_cycles_three_orders_and_the_cursor_stays_on_its_addon() {
    var listed = listing()
    var times = { "BigWigs": "2026-11-01T00:00:00Z", "DBM": "2026-11-02T00:00:00Z", "Foo": "2026-10-01T00:00:00Z", "WeakAuras": "2026-11-05T00:00:00Z" }
    listed.data.targets[1].addons.forEach(function(a) { a.installed_at = times[a.name]; a.drift = { "state": "clean", "files": 0, "folders": [] } })
    listed.data.targets[1].addons[2].ref = "wowi:1"
    svc.lists = Model.reduceList(listed)
    view.setFilter("retail")
    keyClick("2")
    var installed = content()
    var names = function() { return installed.rows.map(function(r) { return r.name }) }
    compare(json(names()), json(["DBM", "BigWigs", "Foo", "WeakAuras"]))
    keyClick("j")
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 2)
    compare(installed.rows[view.cursor].name, "Foo")
    keyClick("S")
    compare(installed.sort, "recent")
    compare(json(names()), json(["DBM", "WeakAuras", "BigWigs", "Foo"]))
    compare(installed.rows[view.cursor].name, "Foo")
    compare(findChild(installed, "sortLabel").text, "Sorted by recently updated")
    keyClick("S")
    compare(json(names()), json(["DBM", "BigWigs", "WeakAuras", "Foo"]), "GitHub before WoWInterface")
    compare(installed.rows[view.cursor].name, "Foo")
    keyClick("S")
    compare(installed.sort, "name")
    verify(!findChild(installed, "sortLabel").visible)
    compare(installed.rows[view.cursor].name, "Foo")
  }

  // Sharing (1n2p.37): the Addons view's share line copies the scope's list (`y`) and
  // pastes one (Ctrl+V); what went on the clipboard flashes, why nothing did is the
  // panel's notice, and both keys and buttons ask the service for the scope's.
  function test_the_share_line_copies_and_pastes_the_scopes_addon_list() {
    keyClick("2")
    var line = findChild(content(), "shareLine")
    verify(line.visible)
    compare(findChild(line, "shareText").text, "Share your addon list, or use one someone shared")
    var copyButton = findChild(line, "copyList")
    var pasteButton = findChild(line, "pasteList")
    // The keys are the tooltips', never the labels' (omad-xdwy).
    compare(copyButton.text, "Copy addon list")
    compare(pasteButton.text, "Paste addon list…")
    verify(/\(y\)$/.test(copyButton.tooltipText), copyButton.tooltipText)
    verify(/\(Ctrl\+V\)$/.test(pasteButton.tooltipText), pasteButton.tooltipText)
    compare(copyButton.Accessible.name, copyButton.text)
    verify(copyButton.enabled && pasteButton.enabled)
    // `y` under All: every client's list; the corner toast's flash says what went on
    // the clipboard (omad-92d5), and no view draws a flash of its own.
    keyClick("y")
    compare(json(svc.shareCopies), json([""]))
    svc.shareDone(true, "Copied the addon list of every client: 6 addons")
    compare(findChild(content(), "copyFlash"), null)
    var toast = findChild(view, "toast")
    verify(toast.showing)
    compare(findChild(view, "toastLine").text, "Copied the addon list of every client: 6 addons")
    verify(!findChild(toast, "alertGlyph").visible)
    compare(view.notice, "")
    // A copy that did not happen says so there, with the alert glyph.
    copyButton.clicked()
    compare(json(svc.shareCopies), json(["", ""]))
    svc.shareDone(false, "Couldn't copy (wl-copy is missing)")
    compare(findChild(view, "toastLine").text, "Couldn't copy (wl-copy is missing)")
    verify(findChild(toast, "alertGlyph").visible)
    // Ctrl+V: the paste, its refusal the notice; a preview asked clears it.
    keyClick(Qt.Key_V, Qt.ControlModifier)
    compare(json(svc.sharePastes), json([""]))
    svc.shareDone("The clipboard doesn't hold an addon list. Ask for one copied with Copy addon list.")
    compare(view.notice, "The clipboard doesn't hold an addon list. Ask for one copied with Copy addon list.")
    compare(findChild(view, "toastLine").text, "The clipboard doesn't hold an addon list. Ask for one copied with Copy addon list.")
    pasteButton.clicked()
    svc.shareDone("")
    compare(view.notice, "")
    verify(!toast.showing)
    // On one client, that client's.
    view.setFilter("era")
    keyClick("y")
    keyClick(Qt.Key_V, Qt.ControlModifier)
    compare(json(svc.shareCopies.slice(-1).concat(svc.sharePastes.slice(-1))), json(["era", "era"]))
    // Elsewhere, Ctrl+V says where it works.
    keyClick("1")
    keyClick(Qt.Key_V, Qt.ControlModifier)
    compare(findChild(view, "toastLine").text, "Addon lists paste in Addons (2)")
    compare(svc.sharePastes.length, 3)
  }

  function test_the_share_line_pastes_with_no_addons_yet_and_copies_nothing() {
    svc.lists = Model.reduceList(listEnvelope([{ "id": "retail", "key": "k-retail", "label": "retail",
      "counts": { "managed": 0, "unmanaged": 0 }, "addons": [] }]))
    keyClick("2")
    var line = findChild(content(), "shareLine")
    verify(line.visible, "a new computer starts from a shared list")
    verify(!findChild(line, "copyList").enabled, "nothing to copy")
    verify(findChild(line, "pasteList").enabled)
    findChild(line, "pasteList").clicked()
    compare(json(svc.sharePastes), json([""]))
    // Before the list is read, no share line.
    svc.lists = null
    verify(!findChild(content(), "shareLine").visible)
  }

  // Folders omaddons does not manage are not Addons' to list (omad-3oru): Issues suggests
  // the take-over where there is something to take over.
  function test_unmanaged_folders_are_not_listed_and_an_empty_client_says_how_to_add() {
    keyClick("2")
    compare(findChild(content(), "unmanaged"), null, "no line for Retail's 2 unmanaged folders")
    compare(findChild(content(), "takeOver"), null, "no Take over… in Addons")
    // No text in the view reads as a command line.
    var texts = []
    var collect = function(item) {
      for (var i = 0; i < item.children.length; i++) {
        var c = item.children[i]
        if (typeof c.text === "string" && c.text !== "") texts.push(c.text)
        collect(c)
      }
    }
    collect(content())
    compare(json(texts.filter(function(t) { return t.indexOf("omaddons ") === 0 || t.indexOf(" -t ") >= 0 })), json([]))
    svc.lists = Model.reduceList(listEnvelope([{ "id": "era", "key": "k", "label": "era", "counts": { "unmanaged": 0 }, "addons": [] }]))
    var empty = findChild(content(), "stateEmpty")
    verify(empty.visible)
    compare([empty.sentence, empty.actionText], ["No addons yet. Find some in Search.", "Search"])
    empty.action()
    compare(view.view, "search", "Search opens Search")
  }

  function test_an_incompatible_binary_leaves_the_rows_inert() {
    keyClick("2")
    svc.phase = "incompatible"
    keyClick(Qt.Key_Return)
    keyClick("x")
    keyClick("p")
    compare(svc.runs.length, 0)
  }

  // ------------------------------------------------------------- Issues

  // `errors --json` as the binary prints it: era without BugGrabber; on retail, Foo
  // (an update waiting) with a Lua runtime error and a custom message, Bar current, one
  // report received from another player.
  function errorsEnvelope(partial) {
    var error = function(kind, message, chars, location, stack) {
      return { "fingerprint": "e:" + location, "database": "db-1", "origin": "local", "message_kind": kind, "message": message,
        "message_chars": chars, "location": location, "attribution": { "method": "first_addon_frame", "confidence": "high" },
        "type": "error", "count": 2, "sessions": 1, "new": true, "stack": stack }
    }
    return { "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": "errors", "exit_code": 0,
      "partial": false, "warnings": [],
      "commands": [{ "action": "install_buggrabber", "command": "omaddons install buggrabber -t era",
        "argv": ["omaddons", "install", "buggrabber", "-t", "era"], "destructive": false }],
      "data": { "targets": [
        { "target": { "id": "era" }, "capture": "none", "coverage": "complete", "files": { "partial": 0 }, "databases": [],
          "summary": { "new": 0, "received": 0 }, "addons": [], "dismiss": { "precondition": "pc1:" + "1".repeat(64) } },
        { "target": { "id": "retail" }, "capture": "buggrabber", "coverage": "complete", "files": { "partial": partial ? 1 : 0 },
          "databases": [{ "id": "db-1", "state": partial ? "partial" : "complete" }],
          "summary": { "new": 3, "received": 1 }, "dismiss": { "precondition": "pc1:" + "2".repeat(64) },
          "addons": [
            { "addon": "Foo", "managed": true, "kind": "addon", "version": "v1", "latest": { "version": "v2", "status": "update_available" },
              "count": 4, "new": 2, "errors": [error("lua_runtime", "attempt to index a nil value (field 'x')", null, "Foo/core.lua:12", ["Foo/core.lua:12: in function 'Refresh'"]),
                error("custom", null, 25, "Foo/boss.lua:3", [])] },
            { "addon": "Bar", "managed": true, "kind": "addon", "version": "v1", "latest": { "version": "v1", "status": "current" },
              "count": 1, "new": 1, "errors": [error("lua_runtime", "attempt to call a nil value", null, "Bar/x.lua:1", ["Bar/x.lua:1: in main chunk"])] }
          ] }
      ] } }
  }

  function issuesReady(partial) {
    var reinstall = ["omaddons", "install", "Plater", "-t", "era", "--reinstall"]
    svc.targets = [
      { "id": "era", "running": false, "runningUnknown": false, "clientDir": "/games/era", "updates": null, "issues": [
        { "id": "fm-1", "severity": "P2", "urgent": false, "title": "Plater's files changed on disk", "addon": "Plater",
          "remediation": Model.cliAction(reinstall.join(" "), reinstall, false), "explain": null, "forced": false, "autoFixable": false },
        { "id": "fm-2", "severity": "P0", "urgent": true, "title": "a journal waits for repair", "addon": null,
          "remediation": Model.cliAction("see README: Troubleshooting", null, false), "explain": null, "forced": false, "autoFixable": false }
      ] },
      { "id": "retail", "running": false, "runningUnknown": false, "clientDir": "/games/retail", "updates": null, "issues": [] }
    ]
    svc.errors = Model.reduceErrors(errorsEnvelope(partial))
    keyClick("5")
    compare(view.view, "issues")
  }

  // The Errors view (4, 1n2p.27) over the same clients and errors.
  function errorsReady(partial) {
    issuesReady(partial)
    keyClick("4")
    compare(view.view, "errors")
  }

  // Every text a view shows, depth first.
  function texts(item) {
    var out = []
    var collect = function(i) {
      for (var c = 0; c < i.children.length; c++) {
        var child = i.children[c]
        if (child.visible !== false && typeof child.text === "string" && child.text !== "") out.push(child.text)
        collect(child)
      }
    }
    collect(item)
    return out
  }

  // `status`'s warnings, reduced as the service reduces them.
  function statusWarnings(warnings) {
    return Model.reduceStatus({ "ok": true, "schema_version": "1.1", "tool_version": "0.1.0", "command": "status",
      "exit_code": 0, "data": { "targets": [] }, "warnings": warnings }).warnings
  }

  function test_issues_lists_each_clients_problems_in_words_then_notes_never_lua_errors() {
    svc.statusWarnings = statusWarnings([
      { "code": "journal_pending", "message": "unfinished transactions wait", "fix": "quit the game", "fix_argv": null },
      { "code": "unknown_flavor", "message": "xyz: an unknown flavor folder", "fix": "set its family: omaddons config set targets.xyz.family <family>", "fix_argv": null }])
    issuesReady(false)
    var issues = content()
    compare(json(issues.rows.map(function(r) { return [r.index, r.kind, r.severityWord] })),
      json([[0, "problem", "Broken"], [1, "problem", "Worth fixing"], [2, "note", ""]]))
    // The client's header: its name (these clients carry no player name: their ids) and
    // how many problems; its rows name no client (§2.2).
    var header = findChild(findChild(issues, "issuesSection_client/era"), "issuesSection")
    compare([header.title, header.count], ["era", "2 problems"])
    compare(findChild(row("issueRow", 0), "severity").text, "Broken")
    compare(findChild(row("issueRow", 0), "headline").text, "a journal waits for repair")
    // The broken one first; its prose remediation is a sentence, never a button.
    compare(findChild(row("issueRow", 0), "prose").text, "see README: Troubleshooting")
    verify(!findChild(row("issueRow", 0), "buttons").visible)
    verify(!findChild(row("issueRow", 0), "commandText").visible)
    keyClick(Qt.Key_Return)
    compare(svc.runs.length + svc.cliRuns.length, 0)
    // The reinstall is a named fix that previews through its template, as Addons' Repair.
    compare(findChild(row("issueRow", 1), "issueButton_fix").text, "Repair Plater")
    compare(findChild(row("issueRow", 1), "issueButton_fix").tooltipText, "", "a fix has no tooltip (omad-vqkd)")
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["reinstall", { "target": "era", "addon": "Plater" }, "Repair Plater"]))
    compare(json(words(svc.runs[0])), json(["install", "-t", "era", "--reinstall", "--", "Plater"]))
    compare(svc.cliRuns.length, 0, "never the remediation argv run as is")
    // The note: the code's words, under Notes; its prose fix (it names a config key) is not
    // shown, and journal_pending is the problems' to say.
    compare(findChild(row("issueRow", 2), "headline").text, "A client's game version isn't recognized, so omaddons treats it carefully.")
    verify(!findChild(row("issueRow", 2), "prose").visible)
    var shown = texts(issues)
    verify(shown.indexOf("unfinished transactions wait") < 0, json(shown))
    // No Lua error, and no command line, anywhere in the view.
    for (var t = 0; t < shown.length; t++) {
      verify(shown[t].indexOf("omaddons ") !== 0 && shown[t].indexOf("attempt to") < 0, shown[t])
    }
  }

  // omad-5cg5: a warning that names its client (`targets`) says it by name, and shows under
  // that client's scope; under another client's it does not.
  function test_a_note_about_a_client_names_it_and_follows_its_scope() {
    svc.statusWarnings = statusWarnings([
      { "code": "unknown_flavor", "message": "era: an unknown version", "fix": null, "fix_argv": null, "targets": ["era"] }])
    issuesReady(false)
    view.setFilter("era")
    var notes = content().rows.filter(function(r) { return r.kind === "note" })
    compare(json(notes.map(function(r) { return [r.title, r.target] })),
      json([["era's game version isn't recognized, so omaddons treats it carefully.", "era"]]))
    view.setFilter("retail")
    compare(content().rows.filter(function(r) { return r.kind === "note" }).length, 0)
    view.setFilter("")
  }

  // `status`'s recommendations (1n2p.28): suggestions in words with a named fix, each run
  // through its template, the frame's own action, or the view that does it.
  function test_suggestions_run_through_their_templates_and_views() {
    var adopt = ["omaddons", "adopt", "-t", "retail"]
    var check = ["omaddons", "outdated"]
    var backup = ["omaddons", "backup", "create", "-t", "retail"]
    var errors = ["omaddons", "errors"]
    var reduced = Model.reduceStatus({ "ok": true, "schema_version": "1.1", "tool_version": "0.1.0", "command": "status", "exit_code": 0,
      "data": { "summary": { "state": "idle", "targets": 2, "running": [], "updates": { "available": 0, "targets": [] },
          "issues": { "p0": 0, "p1": 0, "p2": 0, "p3": 0 }, "errors": { "new": 0, "targets": [] }, "check_due": true, "widget_outdated": false },
        "targets": [{ "id": "retail", "display_name": "Retail", "adoptable": 2, "issues": [] }, { "id": "era", "display_name": "Classic Era", "issues": [] }] },
      "commands": [{ "action": "adopt", "command": adopt.join(" "), "argv": adopt, "destructive": false },
        { "action": "check_updates", "command": check.join(" "), "argv": check, "destructive": false },
        { "action": "check_errors", "command": errors.join(" "), "argv": errors, "destructive": false },
        { "action": "backup_create", "command": backup.join(" "), "argv": backup, "destructive": false }] })
    svc.summary = reduced.summary
    svc.targets = reduced.targets
    svc.commands = reduced.commands
    keyClick("5")
    compare(view.view, "issues")
    var issues = content()
    // Nothing needs fixing, and the suggestions stay listed under it.
    var state = findChild(issues, "viewState")
    compare(state.viewState.state, "empty")
    compare(state.viewState.sentence, "Nothing needs fixing.")
    compare(json(issues.rows.map(function(r) { return findChild(row("issueRow", r.index), "headline").text })), json([
      "2 addons on Retail were installed by hand or by another addon manager.", "Not checked for updates lately.",
      "Lua errors changed since they were last read.", "Retail has no recent backup of its settings."]))
    // Back up Retail: the template `b` runs.
    compare(findChild(row("issueRow", 3), "issueButton_fix").text, "Back up Retail")
    findChild(row("issueRow", 3), "issueButton_fix").clicked()
    compare(json(svc.runs[0]), json(["backup_target", { "target": "retail" }, "Back up Retail"]))
    // Check for updates: Check now.
    keyClick("g")
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(svc.checks, 1)
    // Read Lua errors: the Errors view, read again.
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(view.view, "errors")
    compare(json(svc.readAgains), json(["errors"]))
    // Take over Retail's addons…: the take-over flow (1n2p.32), previewed through its template.
    keyClick("5")
    compare(findChild(row("issueRow", 0), "issueButton_fix").text, "Take over Retail's addons…")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[svc.runs.length - 1]), json(["adopt_target", { "target": "retail" }, "Take over the addons on Retail"]))
    compare(view.view, "issues")
    compare(svc.cliRuns.length, 0, "every one ran through the panel's own way")
    // Narrowed to Classic Era: none of these is for it, and the machine-wide ones go.
    view.setFilter("era")
    keyClick("5")
    compare(content().rows.length, 0)
  }

  // Errors (1n2p.27): per client, a card where nothing records (its install previewed
  // as any change), each addon's errors in words, the first error's message strict.
  function test_errors_lists_each_clients_addons_in_words_and_a_card_where_nothing_records() {
    errorsReady(false)
    var errors = content()
    compare(json(errors.rows.map(function(r) { return [r.index, r.addon, r.band] })), json([[0, "Foo", "urgent"], [1, "Bar", "urgent"]]))
    // era: no BugGrabber, a card whose button installs it.
    var card = findChild(findChild(errors, "errorSection_era"), "captureCard")
    verify(card.visible)
    // (These clients carry no player name: their ids.)
    compare(card.title, "Lua errors aren't recorded on era: install BugGrabber to record them.")
    findChild(card, "noticeButton_0").clicked()
    compare(json(svc.cliRuns[0]), json([["omaddons", "install", "buggrabber", "-t", "era"], "Install BugGrabber"]))
    // Retail's rows: who, how often, how many new; the message strict; received a count.
    var foo = findChild(row("errorRow", 0), "addonRow")
    compare(foo.meta, "probably caused by it · 4 times · 2 new")
    compare(json(foo.chips.map(function(c) { return c.text })), json(["update available (may fix it)"]))
    compare(findChild(row("errorRow", 0), "message").text, "attempt to index a nil value (field 'x')")
    var texts = []
    var collect = function(item) {
      for (var i = 0; i < item.children.length; i++) {
        var c = item.children[i]
        if (typeof c.text === "string" && c.text !== "") texts.push(c.text)
        collect(c)
      }
    }
    collect(errors)
    verify(texts.indexOf("1 error sent by other players (not counted)") >= 0, json(texts))
    // No raw detail, and no account name, anywhere in the view.
    for (var t = 0; t < texts.length; t++) verify(texts[t].indexOf("FakeChar") < 0 && texts[t].indexOf("account") < 0, texts[t])
  }

  // A missing client's two remediations (1n2p.43): forgetting it is a button; the purge,
  // which deletes backups and undo snapshots no undo brings back, is text with what it
  // deletes, copied with `y`, never run by Enter.
  // A missing client's scope (§2.2): every view shows one card, with Rescan clients and
  // Forget through the plain `targets rm` its own problem offers, and none says it is
  // empty or up to date.
  function test_a_missing_clients_scope_is_one_card_with_rescan_and_forget() {
    var forget = Model.cliAction("omaddons targets rm classic", ["omaddons", "targets", "rm", "classic"], true)
    svc.targets = svc.targets.concat([{ "id": "classic", "displayName": "Mists of Pandaria Classic", "missing": true,
      "running": false, "runningUnknown": false, "updates": null, "issues": [{ "id": "fm-x", "severity": "P2", "urgent": false,
        "title": "Mists of Pandaria Classic is no longer installed", "addon": null, "remediation": forget,
        "autoFixable": false, "explain": null, "forced": false }] }])
    findChild(view, "scope").choose("classic")
    compare(view.filter, "classic")
    for (var n = 1; n <= 6; n++) {
      keyClick(String(n))
      var card = findChild(content(), "stateNotice")
      verify(card !== null && card.visible, "a card on view " + n)
      compare(card.title, "Mists of Pandaria Classic isn't on this computer any more")
      compare(card.body, "Its folder is gone, or the game was uninstalled.")
      verify(!findChild(content(), "stateEmpty").visible, "no empty sentence on view " + n)
    }
    var notice = findChild(content(), "stateNotice")
    compare(findChild(notice, "noticeButton_0").text, "Rescan clients")
    findChild(notice, "noticeButton_0").clicked()
    compare(svc.rescans, 1)
    compare(findChild(notice, "noticeButton_1").text, "Forget Mists of Pandaria Classic…")
    findChild(notice, "noticeButton_1").clicked()
    compare(json(svc.cliRuns[svc.cliRuns.length - 1]), json([["omaddons", "targets", "rm", "classic"], "Forget Mists of Pandaria Classic"]))
    // t leaves it for All, and the views read as before.
    keyClick("t")
    compare(view.filter, "")
    verify(findChild(content(), "stateNotice") === null || !findChild(content(), "stateNotice").visible)
  }

  function test_a_purge_is_text_to_copy_never_a_button() {
    var purge = ["omaddons", "targets", "rm", "classic", "--purge"]
    var forget = ["omaddons", "targets", "rm", "classic"]
    var reduced = Model.reduceStatus({ "ok": true, "schema_version": "1.1", "tool_version": "0.1.0", "command": "status",
      "exit_code": 0, "data": { "targets": [{ "id": "classic", "issues": [
        { "id": "fm-a", "severity": "P2", "title": "classic is no longer installed", "addon": null,
          "remediation": { "command": forget.join(" "), "argv": forget, "destructive": true } },
        { "id": "fm-b", "severity": "P3", "title": "classic's backups and undo snapshots remain", "addon": null,
          "remediation": { "command": purge.join(" "), "argv": purge, "destructive": true } }
      ] }] } })
    svc.targets = reduced.targets
    keyClick("5")
    compare(view.view, "issues")
    // The forget row: its named fix, nothing to copy; Enter runs it through the
    // classifier, which previews it first (targets rm is destructive).
    compare(findChild(row("issueRow", 0), "issueButton_fix").text, "Forget classic…")
    compare(findChild(row("issueRow", 0), "issueButton_copy"), null)
    // The purge row: no fix, the command as text with what it deletes, and Copy command.
    var purgeRow = row("issueRow", 1)
    compare(findChild(purgeRow, "commandText").text, purge.join(" "))
    compare(findChild(purgeRow, "terminalNote").text, Model.IRREVERSIBLE_NOTE)
    compare(findChild(purgeRow, "issueButton_fix"), null)
    compare(findChild(purgeRow, "issueButton_copy").text, "Copy command")
    compare(findChild(purgeRow, "issueButton_copy").tooltipText, "Copy the command a row shows, to run in a terminal (y)")
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick(Qt.Key_Return)
    compare(svc.cliRuns.length, 0)
    keyClick("y")
    compare(json(svc.copies), json([["wl-copy", "--", purge.join(" ")]]))
    compare(findChild(row("issueRow", 1), "issueButton_copy").text, "Copied")
    // The copy flash goes after a moment (UX-36).
    tryCompare(findChild(row("issueRow", 1), "issueButton_copy"), "text", "Copy command", 3000)
    // wl-copy missing (omad-phcg): the flash says nothing was copied, never "Copied".
    svc.copyOk = false
    keyClick("y")
    compare(svc.copies.length, 2)
    compare(findChild(row("issueRow", 1), "issueButton_copy").text, "Couldn't copy (wl-copy is missing)")
    tryCompare(findChild(row("issueRow", 1), "issueButton_copy"), "text", "Copy command", 3000)
    compare(svc.cliRuns.length, 0)
    keyClick("k")
    keyClick(Qt.Key_Return)
    compare(json(svc.cliRuns), json([[["omaddons", "targets", "rm", "classic"], "Forget classic…"]]))
  }

  // ---- the doctor's repairs (G2): Enter previews a repair through its bound template,
  // a forced one is a terminal command with the doctor's explanation, and a command that
  // needs consent or deletes what repairs keep is text.

  readonly property string credentialFm: "fm-secrets-credential-file-mode"

  // `status`'s issues reduced as the service reduces them: these on retail, `machine` of
  // no client.
  function statusIssues(issues, machine) {
    return Model.reduceStatus({ "ok": true, "schema_version": "1.1", "tool_version": "0.1.0", "command": "status",
      "exit_code": 0, "data": { "targets": [{ "id": "retail", "issues": issues }], "issues": machine || [] } })
  }

  function credentialIssue() {
    return { "id": credentialFm, "severity": "P2", "title": "the GitHub token file is readable by other users", "addon": null,
      "remediation": { "command": "omaddons doctor fix --only " + credentialFm, "argv": ["omaddons", "doctor", "fix", "--only", credentialFm], "destructive": false },
      "auto_fixable": true,
      "explain": { "command": "omaddons doctor explain " + credentialFm, "argv": ["omaddons", "doctor", "explain", credentialFm] } }
  }

  function test_enter_on_an_auto_fixable_problem_previews_its_repair() {
    var reduced = statusIssues([credentialIssue()], [credentialIssue()])
    svc.targets = reduced.targets
    svc.machineIssues = reduced.issues
    keyClick("5")
    compare(view.view, "issues")
    // The machine's row first ("This computer"), then retail's: each previews its repair.
    compare(findChild(findChild(content(), "issuesSection_machine"), "issuesSection").title, "This computer")
    for (var i = 0; i < 2; i++) {
      var r = row("issueRow", i)
      compare(findChild(r, "issueButton_fix").text, "Repair", "row " + i)
      compare(findChild(r, "issueButton_fix").tooltipText, "", "row " + i)
      compare(findChild(r, "issueButton_explain").text, "Explain", "row " + i)
      verify(!findChild(r, "commandText").visible, "row " + i + ": the preview lists what a repair does")
      compare(findChild(r, "issueButton_copy"), null, "row " + i)
      verify(!findChild(r, "terminalNote").visible, "row " + i)
    }
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["doctor_fix", { "fm": credentialFm }, "Repair: the GitHub token file is readable by other users"]))
    compare(json(words(svc.runs[0])), json(["doctor", "fix", "--only=" + credentialFm]))
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[1]), json(["doctor_fix_target", { "target": "retail", "fm": credentialFm }, "Repair: the GitHub token file is readable by other users"]))
    compare(json(words(svc.runs[1])), json(["doctor", "fix", "-t", "retail", "--only=" + credentialFm]))
    compare(svc.cliRuns.length, 0, "the remediation argv the CLI printed is never run")
    compare(svc.explains.length, 0)
    // The button runs the same.
    findChild(row("issueRow", 0), "issueButton_fix").clicked()
    compare(json(svc.runs[2]), json(svc.runs[0]))
    // A change in progress: no repair starts, and the button says so.
    svc.action = { "label": "Update all", "current": 0, "total": 0, "message": "" }
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 3, "no repair while a change runs")
    verify(!findChild(row("issueRow", 0), "issueButton_fix").enabled)
    verify(findChild(row("issueRow", 0), "issueButton_explain").enabled, "reading why is no change")
  }

  function test_a_set_aside_row_has_no_fix_and_enter_shows_why() {
    var fm = "fm-concurrency_primitives-journal-too-new"
    var forced = ["omaddons", "doctor", "fix", "--only", fm, "-t", "retail", "--force", "--yes"]
    var explain = ["omaddons", "doctor", "explain", fm, "-t", "retail"]
    svc.targets = statusIssues([{ "id": fm, "severity": "P0", "title": "An interrupted change was started by a newer omaddons", "addon": null,
      "remediation": { "command": forced.join(" "), "argv": forced, "destructive": true }, "auto_fixable": false,
      "explain": { "command": explain.join(" "), "argv": explain } }]).targets
    keyClick("5")
    var key = "p/retail/" + fm + "/"
    var r = row("issueRow", 0)
    compare(findChild(r, "severity").text, "Broken")
    compare(findChild(r, "terminalNote").text, "This needs your go-ahead, which the panel never gives: read the explanation, then run it in a terminal.")
    verify(findChild(r, "commandText").visible)
    compare(findChild(r, "commandText").text, forced.join(" "), "the exact remediation, --force --yes included")
    compare(findChild(r, "issueButton_fix"), null, "a forced repair is never a button")
    compare(json(content().rows[0].buttons.map(function(b) { return b.text })), json(["Explain", "Copy command"]))
    // Enter, with nothing to run, opens the problem's page, which reads its explanation.
    keyClick(Qt.Key_Return)
    compare(view.pages.length, 1)
    compare(view.backText(), "Issues")
    var page = findChild(view, "page").item
    compare(findChild(page, "pageSeverity").text, "Broken")
    compare(findChild(page, "pageTitle").text, "An interrupted change was started by a newer omaddons")
    compare(findChild(page, "pageClient").text, "retail")
    compare(findChild(page, "pageCommand").text, forced.join(" "))
    compare(findChild(page, "pageButton_fix"), null)
    // The label is its words, the key its tooltip's (omad-vqkd).
    compare(findChild(page, "pageButton_copy").text, "Copy command")
    compare(findChild(page, "pageButton_copy").tooltipText, "Copy command (y)")
    compare(json(svc.explains), json([[key, explain]]))
    compare(svc.runs.length, 0, "no repair from a forced row")
    compare(svc.cliRuns.length, 0)
    verify(findChild(page, "explanationReading").visible, "Reading… until the explanation answers")
    // An answer for another row is not shown here.
    svc.explanation = { "selection": { "key": "p/era/x/" }, "lines": ["elsewhere"], "failed": "" }
    compare(findChild(page, "explanationLines").count, 0)
    var lines = ["An interrupted change was started by a newer omaddons", "Its repair needs your deliberate consent: read this, then run the command below in a terminal",
      "  In a terminal: " + forced.join(" ")]
    svc.explanation = { "selection": { "key": key }, "lines": lines, "failed": "" }
    compare(findChild(page, "explanationLines").count, 3)
    compare(findChild(page, "explanationLines").itemAt(2).text, lines[2])
    verify(!findChild(page, "explanationReading").visible)
    // `y` copies the terminal command, from the page.
    keyClick("y")
    compare(json(svc.copies), json([["wl-copy", "--", forced.join(" ")]]))
    compare(findChild(page, "pageButton_copy").text, "Copied")
    // wl-copy missing (omad-phcg): the page's button says so instead.
    svc.copyOk = false
    keyClick("y")
    compare(svc.copies.length, 2)
    compare(findChild(page, "pageButton_copy").text, "Couldn't copy (wl-copy is missing)")
    // Enter on the page runs nothing: there is no fix to run here.
    keyClick(Qt.Key_Return)
    compare(svc.runs.length + svc.cliRuns.length, 0)
    // Esc goes back, and drops the explanation.
    keyClick(Qt.Key_Escape)
    compare(view.pages.length, 0)
    compare(svc.explainClears, 1)
    // `e` opens it again; a failed read says so.
    keyClick("e")
    compare(view.pages.length, 1)
    page = findChild(view, "page").item
    compare(svc.explains.length, 2)
    svc.explanation = { "selection": { "key": key }, "lines": null, "failed": "omaddons doctor explain: timeout" }
    compare(findChild(page, "explanationFailed").text, "Couldn't read the explanation: omaddons doctor explain: timeout")
    verify(findChild(page, "explanationFailed").visible)
    verify(!findChild(page, "explanationReading").visible)
    // The row's Explain button opens it too, after back.
    keyClick(Qt.Key_Escape)
    findChild(row("issueRow", 0), "issueButton_explain").clicked()
    compare(view.pages.length, 1)
    compare(svc.runs.length, 0)
    compare(svc.cliRuns.length, 0)
  }

  // A repair from a problem's page is the row's own: previewed through its bound
  // template, by the Issues view; once status no longer reports it, the page says so.
  function test_a_problems_page_repairs_through_its_row_and_says_when_it_is_gone() {
    var reduced = statusIssues([credentialIssue()], [])
    svc.summary = Model.reduceStatus({ "ok": true, "schema_version": "1.1", "tool_version": "0.1.0", "command": "status", "exit_code": 0,
      "data": { "summary": { "state": "idle", "targets": 1, "running": [], "updates": { "available": 0, "targets": [] },
        "issues": { "p0": 0, "p1": 0, "p2": 1, "p3": 0 }, "errors": { "new": 0, "targets": [] }, "check_due": false, "widget_outdated": false },
        "targets": [] } }).summary
    svc.targets = reduced.targets
    keyClick("5")
    keyClick("e")
    compare(view.pages.length, 1)
    var page = findChild(view, "page").item
    compare(findChild(page, "pageButton_fix").text, "Repair")
    compare(findChild(page, "pageButton_fix").tooltipText, "")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs), json([["doctor_fix_target", { "target": "retail", "fm": credentialFm }, "Repair: the GitHub token file is readable by other users"]]))
    verify(!findChild(page, "pageGone").visible)
    // The repair landed: status reports it no more.
    svc.targets = statusIssues([], []).targets
    verify(findChild(page, "pageGone").visible)
    compare(findChild(page, "pageGone").text, "omaddons no longer reports this problem.")
    verify(!findChild(page, "pageButtons").visible)
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 1, "nothing more to run")
  }

  function test_a_doctor_gc_or_forced_remediation_is_text() {
    var gc = ["omaddons", "doctor", "gc", "--before", "30d", "--dry-run"]
    var forced = ["omaddons", "install", "Foo", "-t", "retail", "--force"]
    svc.targets = statusIssues([
      { "id": "fm-a", "severity": "P2", "title": "doctor runs take space", "addon": null,
        "remediation": { "command": gc.join(" "), "argv": gc, "destructive": true } },
      { "id": "fm-b", "severity": "P3", "title": "Foo's folder belongs to another addon", "addon": "Foo",
        "remediation": { "command": forced.join(" "), "argv": forced, "destructive": false } }
    ]).targets
    keyClick("5")
    var commands = [gc, forced]
    for (var i = 0; i < 2; i++) {
      var r = row("issueRow", i)
      verify(findChild(r, "commandText").visible, "row " + i)
      compare(findChild(r, "commandText").text, commands[i].join(" "))
      compare(findChild(r, "issueButton_fix"), null, "row " + i)
      compare(findChild(r, "issueButton_copy").text, "Copy command", "row " + i)
      // Enter on this row: the first j highlights row 0, the next moves to row 1.
      keyClick("j")
      compare(view.cursor, i)
      keyClick(Qt.Key_Return)
      compare(view.pages.length, 0, "row " + i + ": no explanation to open")
    }
    // Each says why it is no button: the panel never runs doctor gc, nor gives consent.
    compare(findChild(row("issueRow", 0), "terminalNote").text, "The panel never runs this one: run it in a terminal.")
    compare(findChild(row("issueRow", 1), "terminalNote").text, "This needs your go-ahead, which the panel never gives: run it in a terminal.")
    compare(svc.runs.length, 0)
    compare(svc.cliRuns.length, 0)
    compare(svc.explains.length, 0)
  }

  // Enter opens the addon's error page; `u` updates it from the view or the page; `y`
  // copies a report that holds no raw detail (1n2p.27).
  function test_enter_opens_the_error_page_and_u_and_y_act_from_view_and_page() {
    errorsReady(false)
    // `u` on Foo (an update waits): the update, with what Addons shows of Foo.
    keyClick("j")
    keyClick("k")
    compare(view.cursor, 0)
    keyClick("u")
    compare(json(svc.runs[0]), json(["update_addon", { "target": "retail", "addon": "Foo" }, "Update Foo",
      [shownRow("k-retail", "retail", "Foo", "v1", "v1")]]))
    // `y`: the report, strict text only, and the flash says what was copied.
    keyClick("y")
    compare(svc.copies.length, 1)
    var report = svc.copies[0][2]
    verify(report.indexOf("Foo v1 on retail") === 0, report)
    verify(report.indexOf("attempt to index a nil value (field 'x')") > 0, report)
    verify(report.indexOf("a message from the addon (25 characters)") > 0, report)
    // The flash is the corner toast's (omad-92d5), never a line atop the list.
    compare(findChild(content(), "copyFlash"), null)
    var toast = findChild(view, "toast")
    compare(findChild(view, "toastLine").text, "Copied the report on Foo")
    verify(toast.showing)
    // wl-copy missing (omad-phcg): the flash says nothing was copied, then goes.
    svc.copyOk = false
    keyClick("y")
    compare(svc.copies.length, 2)
    compare(findChild(view, "toastLine").text, "Couldn't copy (wl-copy is missing)")
    verify(findChild(toast, "alertGlyph").visible)
    tryVerify(function() { return !toast.showing }, Model.NOTE_HOLD_MS + 1500, "the flash goes")
    svc.copyOk = true
    // Bar (no update waits): `u` does nothing.
    keyClick("j")
    keyClick("u")
    compare(svc.runs.length, 1)
    // Enter on Foo: its page, every error with its stack, the addon's own frames bold.
    keyClick("k")
    keyClick(Qt.Key_Return)
    compare(view.pages.length, 1)
    compare(findChild(view, "backButton").text, "Errors")
    var page = findChild(view, "page").item
    compare(findChild(page, "pageTitle").text, "Foo")
    compare(findChild(page, "errors").count, 2)
    var first = findChild(page, "errors").itemAt(0)
    compare(findChild(first, "errorMessage").text, "attempt to index a nil value (field 'x')")
    compare(findChild(first, "errorMeta").text, "Foo/core.lua:12 · 2 times · new")
    var frame = findChild(first, "stack").itemAt(0)
    compare([frame.text, frame.font.bold], ["Foo/core.lua:12: in function 'Refresh'", true])
    compare(findChild(findChild(page, "errors").itemAt(1), "errorMessage").text, "a message from the addon (25 characters)")
    // The page's keys: `u` updates, `y` copies, `r` the restore chooser.
    keyClick("u")
    compare(json(svc.runs[1].slice(0, 3)), json(["update_addon", { "target": "retail", "addon": "Foo" }, "Update Foo"]))
    keyClick("y")
    compare(svc.copies.length, 3)
    compare(findChild(page, "copyFlash"), null)
    compare(findChild(view, "toastLine").text, "Copied the report on Foo")
    verify(toast.showing)
    // wl-copy missing (omad-phcg): the flash says so instead.
    svc.copyOk = false
    keyClick("y")
    compare(svc.copies.length, 4)
    compare(findChild(view, "toastLine").text, "Couldn't copy (wl-copy is missing)")
    // Esc goes back to the view.
    keyClick(Qt.Key_Escape)
    compare(view.pages.length, 0)
  }

  // Raw details only behind their labelled button on the page, read for that addon only.
  function test_raw_details_are_on_the_page_behind_their_label_and_for_that_addon_only() {
    errorsReady(false)
    keyClick("j")
    keyClick("k")
    compare(view.cursor, 0)
    keyClick(Qt.Key_Return)
    var page = findChild(view, "page").item
    var button = findChild(page, "rawButton")
    compare(button.text, "Show raw details…")
    verify(!findChild(page, "raw").visible)
    button.clicked()
    compare(json(svc.rawReads), json([["retail", "Foo"]]))
    verify(findChild(page, "raw").visible)
    compare(findChild(page, "rawLabel").text, "Raw details may contain other players' names and your own code.")
    var raw = function(addon) {
      var env = errorsEnvelope(false)
      env.data.raw_details = true
      env.data.targets[1].addons[0].errors[1].raw = { "message": "[Foo/boss.lua]:3: FakeChar pulled", "stack": [], "locals": null, "chunks": [] }
      return { "selection": { "target": "retail", "addon": addon }, "data": Model.reduceErrors(env) }
    }
    svc.rawErrors = raw("Bar")
    compare(findChild(page, "rawLines").count, 0, "a late read for another addon is not shown")
    svc.rawErrors = raw("Foo")
    compare(findChild(page, "rawLines").count, 1)
    compare(findChild(page, "rawLines").itemAt(0).text, "[Foo/boss.lua]:3: FakeChar pulled")
    // The report never holds them, raw view open or not.
    keyClick("y")
    verify(svc.copies[0][2].indexOf("FakeChar") < 0, svc.copies[0][2])
    compare(button.text, "Hide raw details")
    button.clicked()
    compare(svc.rawClears, 1)
    verify(!findChild(page, "raw").visible)
  }

  function test_d_dismisses_what_the_panel_showed_and_a_stale_one_says_so() {
    errorsReady(false)
    verify(findChild(content(), "dismiss").visible)
    keyClick("d")
    compare(json(svc.dismissals[0]), json([{ "target": "retail", "token": "pc1:" + "2".repeat(64) }]))
    // era has nothing new to dismiss.
    keyClick("t")
    compare(view.filter, "era")
    verify(!findChild(content(), "dismiss").visible)
    keyClick("d")
    compare(json(svc.dismissals[1]), json([]))
    svc.lastError = { "message": "New errors arrived; review them", "code": "plan_stale", "fixes": [], "fixText": "" }
    compare(findChild(view, "errorMessage").text, "New errors arrived; review them")
  }

  function test_an_incomplete_read_says_so() {
    errorsReady(true)
    var texts = []
    var lines = findChild(content(), "sections")
    verify(lines !== null)
    var all = function(item) {
      for (var i = 0; i < item.children.length; i++) {
        if (typeof item.children[i].text === "string") texts.push(item.children[i].text)
        all(item.children[i])
      }
    }
    all(content())
    verify(texts.indexOf("Some errors couldn't be read: the game was writing.") >= 0, json(texts))
  }

  // ------------------------------------------------------------- Search

  function searchReady(hints) {
    keyClick("3")
    compare(view.view, "search")
    var field = findChild(content(), "searchField")
    keyClick("s")
    tryVerify(function() { return field.activeFocus })
    verify(findChild(view, "keyCatcher").blocked)
    keyClick("u")
    compare(svc.runs.length, 0, "typing is not a command")
    field.text = "damage meter"
    keyClick(Qt.Key_Return)
    compare(json(svc.searches[svc.searches.length - 1]), json(["damage meter", view.filter]))
    tryVerify(function() { return !field.activeFocus })
    svc.search = { "selection": { "query": "damage meter", "target": view.filter }, "skipped": [], "query": "damage meter",
      "rows": [{ "name": "Details!", "ref": "wowi:23056", "source": "wowi", "downloads": 515870, "stars": null, "hints": hints, "compat": "" }] }
  }

  // The meta line of a Search row's AddonRow.
  function searchMeta(index) {
    return findChild(findChild(row("searchRow", index), "addonRow"), "meta").text
  }

  function test_search_installs_into_the_filtered_client_when_its_hint_allows() {
    keyClick("t")
    keyClick("t")
    compare(view.filter, "retail")
    searchReady({ "retail": "likely", "era": "no" })
    // The scope's client's verdict, in words, first (1n2p.25).
    compare(searchMeta(0), "Probably works on Retail · WoWInterface · 515.9k downloads")
    keyClick("i")
    compare(json(svc.runs[0]), json(["install", { "target": "retail", "ref": "wowi:23056" }, "Install Details!", []]))
    // A result for another filter is not this view's; the shown query is searched again
    // for the new scope once the pause ends.
    var before = svc.searches.length
    keyClick("t")
    compare(findChild(content(), "searchRow_0"), null)
    compare(svc.searches.length, before, "not before the pause")
    tryVerify(function() { return svc.searches.length === before + 1 })
    compare(json(svc.searches[before]), json(["damage meter", view.filter]))
  }

  // The view shows no release for a search result, so the install's preview always asks.
  function test_search_install_passes_nothing_shown() {
    keyClick("t")
    keyClick("t")
    searchReady({ "retail": "yes", "era": "no" })
    keyClick("i")
    compare(svc.runs.length, 1)
    compare(json(svc.runs[0][3]), json([]), "nothing shown")
  }

  // Enter on a result opens its page (1n2p.23), read by its ref: whether it loads on
  // each client, and Install here where it likely does.
  function test_enter_on_a_result_opens_its_page() {
    searchReady({ "retail": "likely", "era": "no" })
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 0, "Enter installs nothing")
    compare(view.pages.length, 1)
    var page = findChild(view, "page").item
    compare([page.name, page.ref], ["Details!", "wowi:23056"])
    compare(json(svc.infoReads), json(["wowi:23056"]))
    compare(findChild(page, "pageState").text, "Reading about it…")
    // The read answers: retail likely, era not.
    svc.addonInfo = { "selection": { "ref": "wowi:23056" }, "failed": "", "data": { "name": "Details!", "ref": "wowi:23056",
      "about": { "summary": "A damage meter", "author": "Tercio", "updatedAt": "", "homepage": "https://www.wowinterface.com/downloads/info23056",
        "stars": null, "downloads": 515870 }, "aboutWhy": "", "latest": { "version": "1.0", "compat": { "retail": "likely", "era": "unlikely" } },
      "installed": [], "changelog": null, "changelogWhy": "not_supported" } }
    compare(findChild(page, "pageSummary").text, "A damage meter")
    compare(findChild(page, "pageFacts").text, "WoWInterface · by Tercio · 515.9k downloads")
    compare(json(page.rows.map(function(l) { return [l.client, l.meta, l.primary] })), json([
      ["Classic Era", "not installed · not made for this client", ""], ["Retail", "not installed · probably works here", "install"]]))
    // `i` on Retail's line: Install here, as Search's install runs.
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick("i")
    compare(json(svc.runs[0]), json(["install", { "target": "retail", "ref": "wowi:23056" }, "Install Details!", []]))
    // `w` opens its web page; `y` copies its source.
    keyClick("w")
    compare(json(Quickshell.detachedCommands[Quickshell.detachedCommands.length - 1]), json(["omarchy-launch-browser", "https://www.wowinterface.com/downloads/info23056"]))
    keyClick("y")
    compare(json(svc.copies), json([["wl-copy", "--", "wowi:23056"]]))
    compare(findChild(page, "pageCopy").text, "Copied wowi:23056")
    // wl-copy missing (omad-phcg): the button says so, then reads as before.
    tryCompare(findChild(page, "pageCopy"), "text", Model.copy("page.copy_ref"), 3000)
    compare(findChild(page, "pageCopy").tooltipText, Model.withKey(Model.copy("page.copy_ref"), "copy", "page"))
    svc.copyOk = false
    keyClick("y")
    compare(svc.copies.length, 2)
    compare(findChild(page, "pageCopy").text, "Couldn't copy (wl-copy is missing)")
  }

  // A late answer for another addon never lands on this page; a dependency's page goes
  // on top, and back walks the trail (1n2p.23).
  function test_a_page_shows_only_its_own_read_and_walks_dependencies() {
    keyClick("2")
    keyClick(Qt.Key_Return)
    var page = findChild(view, "page").item
    compare(page.name, "Bar")
    svc.addonInfo = { "selection": { "ref": "gh:o/other" }, "failed": "", "data": { "name": "Other", "ref": "gh:o/other",
      "about": { "summary": "Not Bar", "author": "", "updatedAt": "", "homepage": "", "stars": null, "downloads": null }, "aboutWhy": "",
      "latest": null, "installed": [], "changelog": null, "changelogWhy": "" } }
    verify(!findChild(page, "pageSummary").visible, "another addon's answer")
    // Bar's own answer: its TOC needs BigWigs, whose own needs DBM. A page opened from a
    // link is read by the name it was asked by.
    var dep = function(name, asked, needs) {
      return { "selection": { "ref": asked }, "failed": "", "data": { "name": name, "ref": "gh:o/" + name.toLowerCase(), "about": null,
        "aboutWhy": "", "latest": null, "installed": [{ "target": "retail", "version": "v1", "folders": [name], "leftOut": [],
          "toc": { "title": name, "notes": "", "author": "", "dependencies": needs } }], "changelog": null, "changelogWhy": "" } }
    }
    svc.addonInfo = dep("Bar", "gh:o/bar", ["BigWigs"])
    var link = findChild(page, "link_BigWigs")
    verify(link !== null && link.visible)
    link.clicked()
    compare(view.pages.length, 2)
    page = findChild(view, "page").item
    compare(page.name, "BigWigs")
    compare(view.backText(), "Addons › Bar")
    tryVerify(function() { return svc.infoReads[svc.infoReads.length - 1] === "BigWigs" })
    svc.addonInfo = dep("BigWigs", "BigWigs", ["DBM"])
    compare(json(page.rows.filter(function(l) { return l.installed }).map(function(l) { return l.client })), json(["Retail"]))
    findChild(page, "link_DBM").clicked()
    compare(view.pages.length, 3)
    tryCompare(findChild(view, "page").item, "name", "DBM")
    keyClick(Qt.Key_Escape)
    keyClick(Qt.Key_Escape)
    compare(view.pages.length, 1)
    tryCompare(findChild(view, "page").item, "name", "Bar")
    tryVerify(function() { return svc.infoReads[svc.infoReads.length - 1] === "gh:o/bar" })
    keyClick(Qt.Key_Escape)
    compare(view.pages.length, 0)
  }

  // omad-z3ur: a dependency no client has offers Install it, on the cursored client
  // line's client only, previewed through the plan sheet (nothing shown, so it asks);
  // with no line, or a line where the addon is not installed, nothing runs.
  function test_a_missing_dependency_installs_on_the_cursored_client_line_only() {
    keyClick("2")
    keyClick(Qt.Key_Return)
    var page = findChild(view, "page").item
    compare(page.name, "Bar")
    svc.addonInfo = { "selection": { "ref": "gh:o/bar" }, "failed": "", "data": { "name": "Bar", "ref": "gh:o/bar", "about": null,
      "aboutWhy": "", "latest": null, "installed": [{ "target": "era", "version": "v1", "folders": ["Bar"], "leftOut": [],
        "toc": { "title": "Bar", "notes": "", "author": "", "dependencies": ["LibNowhere", "BigWigs"] } }],
      "changelog": null, "changelogWhy": "" } }
    // BigWigs is installed (on Retail): no Install it beside it.
    verify(!findChild(page, "installDependency_BigWigs").visible)
    var install = findChild(page, "installDependency_LibNowhere")
    verify(install !== null && install.visible)
    // No line under the cursor: nothing runs, and the panel says to choose one.
    view.cursorActive = false
    compare(install.text, "Install it")
    install.clicked()
    compare(svc.runs.length, 0)
    compare(findChild(view, "toastLine").text, Model.copy("page.needs.choose"))
    // Retail's line, where Bar is not installed: still nothing.
    var targets = page.rows.map(function(l) { return l.target })
    view.cursor = targets.indexOf("retail")
    view.cursorActive = true
    compare(install.text, "Install it")
    install.clicked()
    compare(svc.runs.length, 0)
    // Classic Era's line: Install it there, by the name its TOC requires.
    view.cursor = targets.indexOf("era")
    compare(install.text, "Install it on Classic Era")
    install.clicked()
    compare(json(svc.runs), json([["install", { "target": "era", "ref": "LibNowhere" }, "Install LibNowhere", []]]))
  }

  // omad-z3ur: each release note is a line after the client lines, folded to its first
  // line but the newest; Enter or a click shows all of it, or folds it again, and runs
  // nothing.
  function test_release_notes_fold_and_unfold_by_enter_and_click() {
    keyClick("2")
    keyClick(Qt.Key_Return)
    var page = findChild(view, "page").item
    svc.addonInfo = { "selection": { "ref": "gh:o/bar" }, "failed": "", "data": { "name": "Bar", "ref": "gh:o/bar", "about": null,
      "aboutWhy": "", "latest": null, "installed": [], "changelog": { "entries": [
        { "version": "v3", "publishedAt": "", "text": "Third\n- faster" },
        { "version": "v2", "publishedAt": "", "text": "Second\n- more" }], "truncated": false, "moreUrl": "" },
      "changelogWhy": "" } }
    var clients = page.page.clients.length
    compare(page.rowCount, clients + 2)
    // Each fold makes the page anew: a note's line is found again after every change.
    var note = function(n) { return findChild(page, "note_n/" + n) }
    var shown = function(n) { return findChild(note(n), "noteText").text }
    compare(shown(0), "Third\n- faster")
    compare(shown(1), "Second …")
    // The cursor on the older note: its fold says what Enter does; Enter shows it all.
    view.moveCursor(clients + 1)
    compare(view.cursor, clients + 1)
    compare(page.rowItem(view.cursor), note(1))
    verify(findChild(note(1), "noteHint").visible)
    compare(findChild(note(1), "noteHint").text, "Show all (Enter)")
    verify(!findChild(note(0), "noteHint").visible, "the hint is the cursored line's")
    keyClick(Qt.Key_Return)
    compare(shown(1), "Second\n- more")
    compare(findChild(note(1), "noteHint").text, "Fold (Enter)")
    // A click folds the newest, the cursor on it; Enter shows it all again.
    // The page laid out again after the fold, so the click lands where the line now is.
    view.showItem(note(0))
    verify(waitForPolish(testCase.Window.window), "the page is laid out")
    mouseClick(note(0), 40, Math.min(12, note(0).height / 2))
    compare(view.cursor, clients)
    compare(shown(0), "Third …")
    keyClick(Qt.Key_Return)
    compare(shown(0), "Third\n- faster")
    compare(svc.runs.length, 0, "a note runs nothing")
  }

  // A hint of no installs nothing, and `i` says why instead of doing nothing (1n2p.25).
  function test_a_hint_of_no_offers_no_install() {
    keyClick("t")
    compare(view.filter, "era")
    searchReady({ "retail": "likely", "era": "no" })
    compare(searchMeta(0), "Not made for Classic Era · WoWInterface · 515.9k downloads")
    keyClick("i")
    compare(svc.runs.length, 0)
    compare(findChild(view, "toastLine").text, "Not made for Classic Era: nothing to install there.")
  }

  // bmcx.6: a result no client has evidence for offers no install button and no install
  // key; its page (Check where it works) decides.
  function test_a_result_nobody_knows_offers_only_its_page() {
    keyClick("t")
    keyClick("t")
    searchReady({ "retail": "unknown", "era": "unknown" })
    view.cursorActive = true
    view.cursor = 0
    var addonRow = findChild(row("searchRow", 0), "addonRow")
    compare(json(addonRow.actions.map(function(a) { return a.name })), json(["enter"]))
    compare(searchMeta(0).split(" · ")[0], "Not known yet for Retail")
    keyClick("i")
    compare(svc.runs.length, 0)
    compare(findChild(view, "toastLine").text, "Not known yet where it works: Enter opens its page, which checks.")
    keyClick(Qt.Key_Return)
    compare(view.pages.length, 1)
    compare(json(svc.infoReads), json(["wowi:23056"]))
  }

  function test_under_all_a_chooser_preselects_no_client() {
    searchReady({ "retail": "likely", "era": "no" })
    compare(searchMeta(0), "Probably works on 1 of 2 clients · WoWInterface · 515.9k downloads")
    keyClick("i")
    var search = content()
    verify(findChild(search, "chooser").visible)
    compare(search.chooserCursor, -1)
    tryVerify(function() { return findChild(search, "chooserKeys").activeFocus })
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 0, "nothing chosen, nothing installed")
    // era (hint no) is greyed, says why, and is never under the cursor.
    var targets = findChild(search, "chooserTargets")
    compare(findChild(search, "chooserTitle").text, "Install Details! on: (j/k, Enter, Esc)")
    compare([findChild(targets.itemAt(0), "choice").text, findChild(targets.itemAt(1), "choice").text],
      ["Not made for Classic Era", "Probably works on Retail"])
    compare(targets.itemAt(0).opacity, 0.4)
    keyClick("j")
    compare(search.chooserCursor, 1)
    keyClick("j")
    compare(search.chooserCursor, 1)
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["install", { "target": "retail", "ref": "wowi:23056" }, "Install Details!", []]))
    verify(!findChild(search, "chooser").visible)
    tryVerify(function() { return findChild(view, "keyCatcher").activeFocus })
  }

  // omad-3rre: with two or more clients it works on, the chooser's first line installs it
  // on each of them (each client's preview, then one sheet), never preselected.
  function test_the_chooser_offers_every_client_it_works_on_first() {
    searchReady({ "retail": "likely", "era": "yes" })
    keyClick("i")
    var search = content()
    compare(search.chooserCursor, -1)
    var targets = findChild(search, "chooserTargets")
    compare([findChild(targets.itemAt(0), "choice").text, findChild(targets.itemAt(1), "choice").text,
      findChild(targets.itemAt(2), "choice").text], ["Every client it works on (2)", "Works on Classic Era", "Probably works on Retail"])
    keyClick(Qt.Key_Return)
    compare(svc.eachInstalls.length, 0, "nothing chosen, nothing installed")
    keyClick("j")
    compare(search.chooserCursor, 0)
    keyClick(Qt.Key_Return)
    compare(json(svc.eachInstalls), json([["wowi:23056", "Details!", ["era", "retail"]]]))
    compare(svc.runs.length, 0)
    verify(!findChild(search, "chooser").visible)
    // One client it works on: no such line.
    searchReady({ "retail": "likely", "era": "no" })
    keyClick("i")
    compare(findChild(findChild(content(), "chooserTargets").itemAt(0), "choice").text, "Not made for Classic Era")
  }

  // The search box, focused (`s` in Search).
  function searchBox() {
    keyClick("3")
    keyClick("s")
    var field = findChild(content(), "searchField")
    tryVerify(function() { return field.activeFocus })
    return field
  }

  // Live search (1n2p.25): typing searches once it pauses, from three characters, once
  // per pause whatever was typed in between.
  function test_typing_searches_once_it_pauses() {
    var field = searchBox()
    compare(findChild(content(), "searchDebounce").interval, Model.SEARCH_DEBOUNCE_MS)
    field.text = "we"
    wait(Model.SEARCH_DEBOUNCE_MS + 150)
    compare(svc.searches.length, 0, "two characters wait for Enter")
    field.text = "wea"
    field.text = "weak"
    compare(svc.searches.length, 0, "nothing before the pause")
    tryVerify(function() { return svc.searches.length === 1 })
    compare(json(svc.searches[0]), json(["weak", ""]))
    // While it runs, the same query again is no new search; Enter on a short one searches
    // at once.
    svc.reading = ["search"]
    field.text = "weak "
    wait(Model.SEARCH_DEBOUNCE_MS + 150)
    compare(svc.searches.length, 1)
    field.text = "wa"
    keyClick(Qt.Key_Return)
    compare(json(svc.searches[1]), json(["wa", ""]))
    tryVerify(function() { return !field.activeFocus }, 1000, "Enter goes to the results")
    // An empty box clears the search: the view says what to type.
    svc.reading = []
    field.text = ""
    compare(content().asked, "")
    compare(findChild(findChild(content(), "viewState"), "stateEmpty").sentence,
      "Type a name, or paste a link from GitHub, WoWInterface, Tukui or WowAce.")
  }

  // The last query's rows stay under "Searching…" while the next one runs, with none of
  // their notices; when it ends without them, they go.
  function test_the_last_results_stay_while_the_next_search_runs() {
    searchReady({ "retail": "likely", "era": "no" })
    svc.search = { "selection": { "query": "damage meter", "target": "" }, "query": "damage meter",
      "skipped": [{ "source": "wowi", "reason": "no_index" }], "rows": svc.search.rows }
    var search = content()
    var state = findChild(search, "viewState")
    compare(findChild(state, "stateNotices").count, 1)
    // A source that didn't answer: Try again searches the shown query again.
    compare(findChild(state, "stateNotices").itemAt(0).title, "WoWInterface couldn't be searched.")
    findChild(state, "stateNotices").itemAt(0).clicked("retry")
    compare(json(svc.searches[svc.searches.length - 1]), json(["damage meter", ""]))
    // A newer query runs: the rows stay, under "Searching…", without their notices.
    svc.reading = ["search"]
    var field = findChild(search, "searchField")
    field.text = "damage meters"
    keyClick("s")
    keyClick(Qt.Key_Return)
    compare(search.asked, "damage meters")
    compare(search.rowCount, 1)
    verify(findChild(state, "refreshingText").visible)
    compare(findChild(state, "refreshingText").text, "Searching…")
    compare(findChild(state, "stateNotices").count, 0)
    // It ended with no answer for it: the rows go.
    svc.reading = []
    compare(search.rowCount, 0)
  }

  // A pasted link is one row and no search; its page reads it and names the addon, and an
  // install from there takes the ref the read answered, never the text (M3-D2).
  function test_a_pasted_link_is_one_row_whose_page_installs_by_the_read_ref() {
    var field = searchBox()
    var link = "https://github.com/tullamods/OmniCC"
    field.text = link
    wait(Model.SEARCH_DEBOUNCE_MS + 150)
    compare(svc.searches.length, 0, "a link is no search")
    var addonRow = findChild(row("searchRow", 0), "addonRow")
    compare([addonRow.title, addonRow.meta], ["Install from GitHub", link])
    compare(content().rowCount, 1)
    keyClick(Qt.Key_Return)
    compare(svc.searches.length, 0)
    tryVerify(function() { return !field.activeFocus })
    compare([view.cursorActive, view.cursor], [true, 0])
    keyClick(Qt.Key_Return)
    compare(view.pages.length, 1)
    var page = findChild(view, "page").item
    compare([page.name, page.ref, page.link], ["", "", link])
    compare(json(svc.infoReads), json([link]))
    compare(page.page.title, link)
    verify(page.rows.every(function(l) { return l.primary !== "install" }), "nothing to install before the read")
    svc.addonInfo = { "selection": { "ref": link }, "failed": "", "data": { "name": "OmniCC", "ref": "gh:tullamods/OmniCC",
      "about": null, "aboutWhy": "", "latest": { "version": "11.2.8", "compat": { "retail": "likely", "era": "unlikely" } },
      "installed": [], "changelog": null, "changelogWhy": "" } }
    compare([page.page.title, page.page.name, page.page.ref], ["OmniCC", "OmniCC", "gh:tullamods/OmniCC"])
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick("i")
    compare(json(svc.runs[0]), json(["install", { "target": "retail", "ref": "gh:tullamods/OmniCC" }, "Install OmniCC", []]))
    // `i` on the link row opens its page too: it never installs the text.
    keyClick(Qt.Key_Escape)
    compare(view.pages.length, 0)
    keyClick("i")
    compare(view.pages.length, 1)
    compare(svc.runs.length, 1)
  }

  // The box's keys (KEYMAP's field rows): Down and Up move the results' cursor while the
  // box keeps the keys; Esc leaves it with the text kept; Esc again clears the search.
  function test_down_and_up_move_the_results_and_esc_twice_clears() {
    searchReady({ "retail": "likely", "era": "no" })
    svc.search = { "selection": { "query": "damage meter", "target": "" }, "query": "damage meter", "skipped": [],
      "rows": [svc.search.rows[0], { "name": "Recount", "ref": "wowi:7000", "source": "wowi", "downloads": 2100000, "stars": null,
        "hints": { "retail": "yes", "era": "yes" } }] }
    var search = content()
    var field = findChild(search, "searchField")
    keyClick("s")
    tryVerify(function() { return field.activeFocus })
    view.cursorActive = false
    keyClick(Qt.Key_Down)
    compare([view.cursorActive, view.cursor], [true, 0])
    keyClick(Qt.Key_Down)
    compare(view.cursor, 1)
    verify(field.activeFocus, "the box keeps the keys")
    keyClick(Qt.Key_Up)
    compare(view.cursor, 0)
    compare(field.text, "damage meter")
    keyClick(Qt.Key_Escape)
    tryVerify(function() { return !field.activeFocus })
    compare([field.text, search.asked, search.rowCount], ["damage meter", "damage meter", 2])
    keyClick(Qt.Key_Escape)
    compare([field.text, search.asked, search.rowCount], ["", "", 0])
  }

  // The catalog `search --catalog` answers for a scope, reduced: Bartender4 fits both clients,
  // Questie only Classic Era, and WeakAuras (installed on Retail in listing()) only Retail.
  function catalogAnswer(scope) {
    var row = function(name, ref, hints) {
      return { "name": name, "ref": ref, "source": "catalog", "author": null, "summary": null, "downloads": null,
        "stars": null, "updatedAt": null, "hints": hints }
    }
    var all = [row("Bartender4", "gh:Nevcairiel/Bartender4", { "era": "yes", "retail": "yes" }),
      row("Questie", "gh:Questie/Questie", { "era": "yes", "retail": "no" }),
      row("WeakAuras", "gh:o/weakauras", { "era": "no", "retail": "yes" })]
    var rows = all.filter(function(r) { return !scope || r.hints[scope] !== "no" })
      .map(function(r) {
        var only = {}
        if (scope) only[scope] = r.hints[scope]
        return scope ? Object.assign({}, r, { "hints": only }) : r
      })
    return { "query": "", "rows": rows, "skipped": [] }
  }

  // An empty Search box browses the built-in catalog for the scope (1n2p.26): one read
  // per scope, the rows in their words, the installed ones last, and no caption above
  // them (omad-xdwy).
  function test_an_empty_search_box_browses_the_catalog_for_the_scope() {
    keyClick("3")
    var search = content()
    tryVerify(function() { return svc.catalogReads.indexOf("") !== -1 })
    var state = findChild(search, "viewState")
    compare(findChild(state, "stateEmpty").sentence, "Type a name, or paste a link from GitHub, WoWInterface, Tukui or WowAce.",
      "what to type until the catalog answers")
    svc.catalogs = { "": catalogAnswer("") }
    compare(json(search.rows.map(function(r) { return [r.name, r.meta.split(" · ")[0]] })), json([
      ["Bartender4", "Works on 2 of 2 clients"], ["Questie", "Works on 1 of 2 clients"], ["WeakAuras", "Works on 1 of 2 clients"]]))
    verify(!findChild(state, "stateEmpty").visible)
    compare(findChild(search, "browseHeader"), null, "no caption over the catalog")
    // Retail: its own read; WeakAuras, installed there, comes last.
    keyClick("t")
    keyClick("t")
    compare(view.filter, "retail")
    tryVerify(function() { return svc.catalogReads.indexOf("retail") !== -1 })
    svc.catalogs = { "": catalogAnswer(""), "retail": catalogAnswer("retail") }
    compare(json(search.rows.map(function(r) { return [r.name, r.chips.map(function(c) { return c.text })] })), json([
      ["Bartender4", []], ["WeakAuras", ["installed 5.19.2"]]]))
    compare(svc.searches.length, 0, "browsing asks no search")
  }

  // Typing replaces the catalog with live results; clearing the box brings it back, the
  // cursor at its top.
  function test_typing_leaves_the_catalog_and_clearing_returns_to_it() {
    svc.catalogs = { "": catalogAnswer("") }
    var field = searchBox()
    var search = content()
    compare(search.rowCount, 3)
    keyClick(Qt.Key_Down)
    keyClick(Qt.Key_Down)
    compare(view.cursor, 1)
    field.text = "damage meter"
    compare(search.rowCount, 0)
    tryVerify(function() { return svc.searches.length === 1 })
    svc.search = { "selection": { "query": "damage meter", "target": "" }, "query": "damage meter", "skipped": [],
      "rows": [{ "name": "Details!", "ref": "wowi:23056", "source": "wowi", "downloads": 515870, "stars": null, "hints": { "retail": "likely" } }] }
    compare(search.rows[0].name, "Details!")
    field.text = ""
    compare(search.rowCount, 3)
    compare(view.cursor, 0)
  }

  // A catalog row installs and opens its page exactly as a result does.
  function test_a_catalog_row_installs_and_opens_its_page_as_a_result_does() {
    svc.catalogs = { "": catalogAnswer(""), "era": catalogAnswer("era") }
    keyClick("t")
    compare(view.filter, "era")
    keyClick("3")
    var search = content()
    compare(json(search.rows.map(function(r) { return r.name })), json(["Bartender4", "Questie"]))
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick("i")
    compare(json(svc.runs[0]), json(["install", { "target": "era", "ref": "gh:Questie/Questie" }, "Install Questie", []]))
    keyClick(Qt.Key_Return)
    compare(view.pages.length, 1)
    var page = findChild(view, "page").item
    compare([page.name, page.ref], ["Questie", "gh:Questie/Questie"])
  }

  // ------------------------------------------------------------- Backups

  // `backup list` as the binary prints it: two backups on retail (the older one in use,
  // the newer verified), one on era.
  function backupsEnvelope(verifiedOld) {
    var backup = function(id, at, extra) {
      var b = { "id": id, "created_at": at, "captured_at": at, "kind": "manual", "trigger": null, "files": 412, "bytes": 15937536,
        "reflinked": "none", "client_running": false, "unsettled": 0, "consistency": "stable", "integrity": "metadata_only",
        "verified_at": null, "baseline": null, "restorable": true, "qualifying": true, "busy": false }
      for (var key in extra) b[key] = extra[key]
      return b
    }
    return { "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": "backup list", "exit_code": 0,
      "partial": false, "warnings": [], "commands": [],
      "data": { "targets": [
        { "target": { "id": "era" }, "auto": { "mode": "off", "capability": "supported", "last": null }, "bytes": 100, "free_bytes": 1073741824,
          "shown": 1, "total": 1, "backups": [backup("01ERA00000000000000000000A", "2099-12-31T23:00:00Z", {})] },
        { "target": { "id": "retail" }, "auto": { "mode": "daily", "capability": "supported", "last": { "at": "2099-12-31T21:00:00Z", "result": "created" } },
          "bytes": 31875072, "free_bytes": 227633266688, "shown": 2, "total": 2, "backups": [
            backup("01RET00000000000000000000B", "2099-12-31T22:00:00Z", { "integrity": "verified", "baseline": "capture" }),
            backup("01RET00000000000000000000A", "2099-12-30T00:00:00Z", { "busy": true, "consistency": "best_effort", "kind": "auto", "trigger": "post_session" })
          ] }
      ] } }
  }

  function backupsReady() {
    svc.backups = Model.reduceBackups(backupsEnvelope())
    view.nowMs = Date.parse("2100-01-01T00:00:00Z")
    keyClick("6")
    compare(view.view, "backups")
  }

  // 1n2p.31: `,` opens the settings sheet over the view; its keys change the setting under
  // the cursor, and each change goes out to the shell (BarWidget saves it), which hands
  // the settings back through the service.
  function test_comma_opens_the_settings_and_its_keys_change_them() {
    var requested = []
    view.settingRequested.connect(function(key, value) {
      requested.push([key, value])
      var next = Object.assign({}, svc.settings)
      next[key] = value
      svc.settings = next
    })
    keyClick(",")
    verify(view.settingsOpen)
    var sheet = findChild(view, "settings")
    tryVerify(function() { return findChild(sheet, "sheetKeys").activeFocus })
    compare(sheet.choice, 0)
    // Behind it the frame does nothing.
    keyClick("2")
    compare(view.view, "updates")
    keyClick(Qt.Key_Return)
    keyClick("l")
    keyClick("h")
    compare(json(requested), json([["panelWidth", 620], ["panelWidth", 640], ["panelWidth", 620]]))
    compare(findChild(findChild(sheet, "setting_panelWidth"), "settingValue").text, "‹ 620 px ›")
    keyClick("j")
    keyClick("j")
    verify(findChild(findChild(sheet, "setting_autoCheck"), "settingAbout").visible)
    keyClick(Qt.Key_Space)
    compare(json(requested[requested.length - 1]), json(["autoCheck", "Off"]))
    // The binary (the last of the panel's): Enter opens its box, Enter there saves, the
    // sheet keeps the keys after.
    var binary = Model.PANEL_SETTING_KEYS.indexOf("binary")
    while (sheet.choice < binary) keyClick("j")
    compare(sheet.choice, binary)
    keyClick(Qt.Key_Return)
    var field = findChild(sheet, "settingEdit")
    tryVerify(function() { return field.activeFocus })
    field.text = ""
    keyClick("/")
    keyClick("x")
    keyClick(Qt.Key_Return)
    compare(json(requested[requested.length - 1]), json(["binary", "/x"]))
    tryVerify(function() { return findChild(sheet, "sheetKeys").activeFocus })
    compare(findChild(findChild(sheet, "setting_binary"), "settingValue").text, "/x")
    // Esc closes it; the frame has the keys again.
    keyClick(Qt.Key_Escape)
    verify(!view.settingsOpen)
    tryVerify(function() { return findChild(view, "keyCatcher").activeFocus })
  }

  // 1n2p.31: the settings sheet's Clients. A name typed and run through its template by
  // the client's key (else its id); the default client stepped (`targets default`); Scan
  // for clients; Add a client by its folder opens its page under the closed sheet.
  function test_the_settings_clients_rename_choose_the_default_scan_and_add() {
    keyClick(",")
    var sheet = findChild(view, "settings")
    tryVerify(function() { return findChild(sheet, "sheetKeys").activeFocus })
    var at = function(key) {
      for (var i = 0; i < sheet.rows.length; i++) if (sheet.rows[i].key === key) return i
      return -1
    }
    sheet.choice = at("name:retail")
    keyClick(Qt.Key_Return)
    compare(sheet.editing, "name:retail")
    var field = findChild(sheet, "settingEdit")
    tryVerify(function() { return field.activeFocus })
    compare(field.text, "Retail")
    field.text = " My main "
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[svc.runs.length - 1]), json(["config_client_name", { "target": "retail", "name": "My main" }, "Rename Retail"]))
    compare(json(words(svc.runs[svc.runs.length - 1])), json(["config", "set", "targets.retail.name", "--", "My main"]))
    tryVerify(function() { return findChild(sheet, "sheetKeys").activeFocus })
    sheet.choice = at("default")
    keyClick("l")
    compare(json(svc.runs[svc.runs.length - 1]), json(["targets_default", { "target": "era" }, "Default client: Classic Era"]))
    // An action acts on Enter only: h and l pass over it.
    sheet.choice = at("rescan")
    keyClick("l")
    compare(svc.rescans, 0)
    keyClick(Qt.Key_Return)
    compare(svc.rescans, 1)
    sheet.choice = at("add")
    keyClick(Qt.Key_Return)
    verify(!view.settingsOpen)
    compare(view.topPage.title, Model.copy("addclient.title"))
    // The page opened over the clients already here stays, and the stack says so: back
    // from it is the view, never the panel closing (omad-5k9k).
    compare(view.pages.length, 1)
    compare(view.topPage, view.pages[0])
    // It goes back once `status` lists one more live client: the one added.
    svc.targets = svc.targets.concat([Object.assign({}, svc.targets[0],
      { "id": "classic", "displayName": "Mists of Pandaria Classic", "missing": false })])
    tryVerify(function() { return view.topPage === null }, 1000, "the page goes back once the client is here")
    compare(view.pages.length, 0)
  }

  // omad-r0to: the GitHub section (the token's state, the gh login, forgetting it, the
  // command to set one in a terminal) and the sheet's last line, config.toml in the
  // player's editor through its builder only.
  function test_the_settings_github_section_and_config_toml() {
    svc.github = { "configured": true, "source": "file" }
    svc.config = { "settings": {}, "path": "/home/fixture/.config/omaddons/config.toml", "exists": true }
    keyClick(",")
    var sheet = findChild(view, "settings")
    tryVerify(function() { return findChild(sheet, "sheetKeys").activeFocus })
    var at = function(key) {
      for (var i = 0; i < sheet.rows.length; i++) if (sheet.rows[i].key === key) return i
      return -1
    }
    compare(sheet.rows[at("github:state")].label, "A token omaddons stored")
    sheet.choice = at("github:from_gh")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[svc.runs.length - 1]), json(["auth_from_gh", {}, "Use the gh login for GitHub"]))
    compare(json(words(svc.runs[svc.runs.length - 1])), json(["auth", "github", "--from-gh"]))
    sheet.choice = at("github:clear")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[svc.runs.length - 1]), json(["auth_clear", {}, "Forget the GitHub token"]))
    // Setting one by hand: the command copied, the token for standard input only.
    sheet.choice = at("github:set")
    keyClick(Qt.Key_Return)
    compare(json(svc.copies[svc.copies.length - 1]), json(["wl-copy", "--", Model.GITHUB_SET_COMMAND]))
    compare(findChild(findChild(sheet, "setting_github:set"), "settingValue").text, "Copied: run it in a terminal")
    // wl-copy missing (omad-phcg): the row says nothing was copied.
    svc.copyOk = false
    keyClick(Qt.Key_Return)
    compare(json(svc.copies[svc.copies.length - 1]), json(["wl-copy", "--", Model.GITHUB_SET_COMMAND]))
    compare(findChild(findChild(sheet, "setting_github:set"), "settingValue").text, "Couldn't copy (wl-copy is missing)")
    // config.toml, where `config` said, in the player's editor.
    Quickshell.resetDetachedCommands()
    sheet.choice = at("other:config")
    keyClick(Qt.Key_Return)
    compare(json(Quickshell.detachedCommands), json([["omarchy-launch-config-editor", "/home/fixture/.config/omaddons/config.toml"]]))
    // A token from the environment: only its state, nothing here would change it.
    svc.github = { "configured": true, "source": "env" }
    compare([sheet.rows[at("github:state")].label, at("github:from_gh"), at("github:clear")], ["From OMADDONS_GITHUB_TOKEN", -1, -1])
  }

  // A client no longer installed: forgotten from the sheet through the plain `targets rm`
  // its own problem offers (the plan sheet previews it), never a purge.
  function test_the_settings_forget_a_client_no_longer_installed() {
    var forget = Model.cliAction("omaddons targets rm classic", ["omaddons", "targets", "rm", "classic"], true)
    svc.targets = svc.targets.concat([{ "id": "classic", "displayName": "Mists of Pandaria Classic", "missing": true,
      "running": false, "runningUnknown": false, "updates": null, "issues": [{ "id": "fm-x", "severity": "P2", "urgent": false,
        "title": "Mists of Pandaria Classic is no longer installed", "addon": null, "remediation": forget,
        "autoFixable": false, "explain": null, "forced": false }] }])
    keyClick(",")
    var sheet = findChild(view, "settings")
    var row = -1
    for (var i = 0; i < sheet.rows.length; i++) if (sheet.rows[i].key === "forget:classic") row = i
    compare(sheet.rows[row].label, "Mists of Pandaria Classic · not found")
    sheet.choice = row
    keyClick(Qt.Key_Return)
    compare(json(svc.cliRuns[svc.cliRuns.length - 1]), json([["omaddons", "targets", "rm", "classic"], "Forget Mists of Pandaria Classic"]))
  }

  // The items under `item` named `name`, in the order they are drawn.
  function named(item, name) {
    var out = []
    var walk = function(at) {
      for (var i = 0; i < at.children.length; i++) {
        var c = at.children[i]
        if (c.objectName === name) out.push(c)
        walk(c)
      }
    }
    walk(item)
    return out
  }

  // 1n2p.29: a card per client (its name and whether it runs, its automatic backups and
  // free space, its backups and their room), then two-line rows with markers as chips.
  function test_backups_are_a_card_per_client_then_two_line_rows() {
    backupsReady()
    var backups = content()
    compare(json(backups.rows.map(function(r) { return [r.target, r.id] })), json([
      ["era", "01ERA00000000000000000000A"], ["retail", "01RET00000000000000000000B"], ["retail", "01RET00000000000000000000A"]]))
    var at = function(iso) { return Model.whenText(iso, view.nowMs) }
    compare([row("backupRow", 0).title, row("backupRow", 0).meta], [at("2099-12-31T23:00:00Z") + " · made by you", "412 files · 15.9 MB"])
    compare([row("backupRow", 1).title, row("backupRow", 1).meta],
      [at("2099-12-31T22:00:00Z") + " · made by you", "412 files · 15.9 MB · checked when made"])
    var busy = row("backupRow", 2)
    compare([busy.title, busy.meta, json(busy.chips)], [at("2099-12-30T00:00:00Z") + " · after play",
      "412 files · 15.9 MB · taken while the game was writing", json([{ "text": "in use", "urgent": false }])])
    compare(json(named(backups, "clientHeader").map(function(h) { return [h.title, h.count] })), json([["Classic Era", ""], ["Retail", "running"]]))
    compare(json(named(backups, "cardLine").map(function(l) { return l.text })), json([
      "Automatic backups: off · 1.1 GB free", "1 backup · 100 B · newest " + at("2099-12-31T23:00:00Z"),
      "Automatic backups: on · 228 GB free", "2 backups · 31.9 MB · newest " + at("2099-12-31T22:00:00Z")]))
    // The cursored row shows its actions: restore and, for one not in use, delete.
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    compare(json(row("backupRow", 1).shownActions.map(function(a) { return a.name })), json(["restore", "remove"]))
    keyClick("j")
    compare(json(row("backupRow", 2).shownActions.map(function(a) { return a.name })), json(["restore"]))
  }

  // omad-4h0d.20: before `backup list` answers, each card counts what `status` counts,
  // never "none yet" for a client that has some; a list that never answered says so,
  // never "showing what was read earlier".
  function test_backups_before_the_list_answers_count_statuss_and_a_failed_read_says_so() {
    var auto = { "mode": "daily", "capability": "supported", "last": null }
    svc.targets = [{ "id": "retail", "displayName": "Retail", "issues": [], "undoSnapshots": null,
      "backups": { "count": 5, "stale": false, "latest": null, "auto": auto, "freeBytes": 2000000000, "bytes": 31875072 } }]
    svc.backups = null
    keyClick("6")
    compare(view.view, "backups")
    var lines = function() { return named(content(), "cardLine").map(function(l) { return l.text }) }
    compare(json(lines()), json(["Automatic backups: on · 2.0 GB free", "5 backups · 31.9 MB"]))
    svc.outcomes = { "backups": "failed" }
    var notices = findChild(findChild(content(), "viewState"), "stateNotices")
    compare(notices.count, 1)
    compare(notices.itemAt(0).title, "Couldn't read your backups.")
    compare(json(lines()), json(["Automatic backups: on · 2.0 GB free", "5 backups · 31.9 MB"]), "the cards stay, still counted")
  }

  function test_b_backs_up_the_filtered_client_or_all_and_each_card_its_own() {
    backupsReady()
    keyClick("b")
    compare(json(svc.runs[0]), json(["backup_all", {}, "Back up all"]))
    named(content(), "backUp")[1].clicked()
    compare(json(svc.runs[1]), json(["backup_target", { "target": "retail" }, "Back up Retail"]))
    keyClick("t")
    keyClick("t")
    compare(view.filter, "retail")
    keyClick("b")
    compare(json(svc.runs[2]), json(["backup_target", { "target": "retail" }, "Back up Retail"]))
    compare(json(words(svc.runs[2])), json(["backup", "create", "-t", "retail"]))
    // In the client's scope, the card's button is `b`'s: its tooltip says so, and its
    // label is the one it has under All (omad-xdwy).
    var button = named(content(), "backUp")[0]
    compare(button.text, "Back up")
    compare(button.tooltipText, "Back up Retail (b)")
    compare(button.Accessible.name, "Back up Retail")
    // Right-anchored and bordered: a whole-pixel width, even when its label measures a
    // fraction, so its right border is never clipped (omad-kldz).
    button.horizontalPadding = 10.25
    verify(button.implicitWidth !== Math.ceil(button.implicitWidth), "a fractional label")
    compare(button.width, Math.ceil(button.implicitWidth))
  }

  // Never one in use (M3-D48); the label names it by date, never its id (UX-29).
  function test_x_deletes_the_exact_backup_and_never_one_in_use() {
    backupsReady()
    keyClick("j")
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 2)
    keyClick("x")
    compare(svc.runs.length, 0)
    compare(findChild(view, "toastLine").text, "This backup is in use. Try again when it's free.")
    keyClick("k")
    keyClick("x")
    compare(json(svc.runs[0]), json(["prune_backup", { "target": "retail", "backup": "01RET00000000000000000000B" },
      "Delete Retail's backup from " + Model.dateText("2099-12-31T22:00:00Z")]))
    compare(json(words(svc.runs[0])), json(["backup", "prune", "-t", "retail", "--", "01RET00000000000000000000B"]))
  }

  function test_r_on_a_backup_opens_the_chooser_on_it() {
    backupsReady()
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick("r")
    var chooser = findChild(view, "chooser")
    verify(chooser.opened)
    compare([chooser.target, chooser.backup, chooser.step, chooser.origin], ["retail", "01RET00000000000000000000B", "scope", "backup"])
    compare(json(svc.detailReads), json(["retail"]))
  }

  // The `-v` read of retail's backups, with the addon versions that wrote them.
  function backupsDetail() {
    var env = backupsEnvelope()
    env.data.targets = [env.data.targets[1]]
    env.data.targets[0].backups[0].addons = [{ "name": "WeakAuras", "folders": ["WeakAuras"], "version": "5.19.2" },
      { "name": "DBM", "folders": ["DBM-Core"], "version": "12.1.14" }]
    env.data.targets[0].backups[1].addons = []
    return { "selection": { "target": "retail" }, "data": Model.reduceBackups(env) }
  }

  function page() {
    return findChild(view, "page").item
  }

  // 1n2p.29's three steps: Enter on the backup, Enter on the addon, then the plan sheet.
  function test_enter_opens_the_backup_page_and_enter_on_an_addon_restores_its_settings() {
    // Retail stopped: its restores are on.
    svc.targets = [svc.targets[0], Object.assign({}, svc.targets[1], { "running": false })]
    backupsReady()
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick(Qt.Key_Return)
    var p = page()
    verify(p !== null, "the backup's page")
    compare([p.target, p.backupId], ["retail", "01RET00000000000000000000B"])
    tryVerify(function() { return json(svc.detailReads) === json(["retail"]) }, 1000, "the page reads what its backup holds")
    compare(findChild(p, "pageTitle").text, "Retail · " + Model.whenText("2099-12-31T22:00:00Z", view.nowMs) + " · made by you")
    compare(findChild(p, "heldState").text, "Reading which addons' settings it holds…")
    svc.backupsDetail = backupsDetail()
    compare(p.rowCount, 2)
    verify(!findChild(p, "heldState").visible)
    compare(findChild(findChild(p, "heldAddon_0"), "heldName").text, "WeakAuras 5.19.2")
    // Choosing the addon: Enter does nothing until the cursor is on one.
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 0)
    keyClick("j")
    keyClick("k")
    compare([view.cursor, view.cursorActive], [0, true])
    verify(findChild(findChild(p, "heldAddon_0"), "restoreHint").visible)
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["restore_addon", { "target": "retail", "addon": "WeakAuras", "backup": "01RET00000000000000000000B" },
      "Restore WeakAuras' settings"]))
    compare(json(words(svc.runs[0])), json(["backup", "restore", "-t", "retail", "--addon=WeakAuras", "--", "01RET00000000000000000000B"]))
    // Back to Backups: the page's -v read goes with it.
    keyClick(Qt.Key_Escape)
    verify(page() === null || !findChild(view, "page").visible)
    compare(svc.detailClears, 1)
  }

  function test_a_backup_page_restores_nothing_while_the_game_runs() {
    backupsReady()
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick(Qt.Key_Return)
    svc.backupsDetail = backupsDetail()
    svc.targets = [svc.targets[0], { "id": "retail", "displayName": "Retail", "running": true, "runningUnknown": false, "clientDir": "/games/retail",
      "updates": { "checkedAt": "2100-01-01T00:00:00Z" }, "issues": [] }]
    var p = page()
    compare(findChild(p, "blocked").text, "Quit Retail first.")
    verify(!findChild(p, "restore").enabled)
    // The first addon under the cursor (a fresh page's first j highlights it).
    keyClick("j")
    compare([view.cursor, view.cursorActive], [0, true])
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 0)
    compare(findChild(view, "toastLine").text, "Quit Retail first.")
  }

  function test_a_backup_page_deletes_it_or_opens_the_chooser_and_never_deletes_one_in_use() {
    backupsReady()
    keyClick("j")
    keyClick("j")
    compare(view.cursor, 1)
    keyClick(Qt.Key_Return)
    keyClick("r")
    var chooser = findChild(view, "chooser")
    verify(chooser.opened)
    compare([chooser.origin, chooser.backup], ["backup", "01RET00000000000000000000B"])
    // The page holds the -v read: the chooser asks for none and drops none.
    compare(chooser.ownsDetail, false)
    keyClick(Qt.Key_Escape)
    verify(!chooser.opened)
    compare([svc.detailReads.length, svc.detailClears], [1, 0])
    keyClick("x")
    compare(json(svc.runs[0]), json(["prune_backup", { "target": "retail", "backup": "01RET00000000000000000000B" },
      "Delete Retail's backup from " + Model.dateText("2099-12-31T22:00:00Z")]))
    // The older one is in use: its page refuses.
    keyClick(Qt.Key_Escape)
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(page().backupId, "01RET00000000000000000000A")
    verify(!findChild(page(), "delete").enabled)
    keyClick("x")
    compare(svc.runs.length, 1)
    compare(findChild(view, "toastLine").text, "This backup is in use. Try again when it's free.")
    // Deleted meanwhile: the page says so.
    var env = backupsEnvelope()
    env.data.targets[1].backups = [env.data.targets[1].backups[0]]
    svc.backups = Model.reduceBackups(env)
    verify(findChild(page(), "backupGone").visible)
  }

  // bmcx.4: the card states the undo data `status -v` measured; past the cleanup rules
  // its repair is in Issues (the widget never names a problem by its id, M4-D48).
  function test_a_card_states_its_undo_data_and_its_cleanup_is_in_issues() {
    svc.targets = [svc.targets[0], Object.assign({}, svc.targets[1], {
      "undoSnapshots": { "count": 30, "prunable": 12, "oldestAt": null, "bytes": 891000000, "truncated": false } })]
    backupsReady()
    var lines = named(content(), "cardLine").map(function(l) { return l.text })
    compare(lines[lines.length - 1], "Undo data: 30 changes · 891 MB · 12 due for cleanup")
    var cleanUps = named(content(), "cleanUp")
    compare([cleanUps[0].visible, cleanUps[1].visible], [false, true])
    cleanUps[1].clicked()
    compare(view.view, "issues")
    compare(svc.runs.length, 0)
  }

  // ------------------------------------------------------------- the real service

  readonly property string planId: "01M8Q2ZZZZZZZZZZZZZZZZZZZZ"

  // omaddons launches whose argv holds `word`.
  function launches(word) {
    return ProcessRegistry.launches.filter(function(l) {
      return l.command[1] === "--json" && l.command.indexOf(word) >= 0
    })
  }

  // A storable dry run of `command` that stored planId: `data` beside its approval, and
  // the offer to apply it.
  function planPreviewJson(command, data, approval, destructive, warnings) {
    var body = { "dry_run": true, "plan_id": planId, "plan_expires_at": "2100-01-02T00:00:00Z", "precondition": "pc1:" + "a".repeat(64),
      "approval": approval }
    for (var key in data) body[key] = data[key]
    return JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": command, "exit_code": 0,
      "partial": false, "warnings": warnings || [], "data": body,
      "commands": [{ "action": "apply", "command": "omaddons apply " + planId, "argv": ["omaddons", "apply", planId], "destructive": destructive }] })
  }

  function approvalOf(op, addon, from, to, reasons, scopeFields) {
    var scope = { "dependencies_added": [], "folders_removed": [], "local_changes_replaced": { "count": 0, "folders": [] },
      "source_changed": [], "while_running": false }
    for (var key in scopeFields) scope[key] = scopeFields[key]
    return { "routine": reasons.length === 0, "reasons": reasons, "scope": scope, "selection": [{ "target_key": "k-retail", "target_id": "retail",
      "addon": addon, "source": "github", "channel": "stable", "release_id": null, "asset": null, "from_version": from, "to_version": to,
      "op": op, "fields": [], "tx": null, "backup": null }] }
  }

  // The real Service on the mock Process, at rest with one client, its frame showing
  // Installed with `addons` (list -v's): `{service, frame}`.
  function realFrame(addons) {
    svc.destroy()
    svc = null
    ProcessRegistry.reset()
    var service = realService.createObject(testCase, { "bellDebounceMs": 10, "localDeadlineMs": 2000 })
    verify(service !== null)
    made = made.concat([service])
    var statusJson = JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION,
      "command": "status", "exit_code": 0, "partial": false, "warnings": [], "commands": [],
      "data": { "targets": [{ "id": "retail", "running": false, "updates": { "available": 1 }, "issues": [] }],
        "summary": { "targets": 1, "running": [], "updates": { "available": 1, "targets": ["retail"] },
          "issues": { "p0": 0, "p1": 0, "p2": 0, "p3": 0 }, "errors": { "new": 0, "targets": [] },
          "check_due": false, "widget_outdated": false, "state": "updates" },
        "last_change": null, "last_change_state": "ok" } })
    launches("status")[0].process.finish(0, statusJson)
    tryCompare(service, "phase", "ok")
    // The catch-up checkpoint the first status queues (F8), done, and the refresh it rings.
    launches("--auto")[0].process.finish(0, JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION,
      "command": "backup create", "exit_code": 0, "partial": false, "warnings": [], "commands": [], "data": { "targets": [] } }))
    launches("status")[1].process.finish(0, statusJson)
    var frame = make(service)
    keyClick("2")
    var list = launches("list")
    verify(list.length > 0, "switching to Installed reads list")
    list[list.length - 1].process.finish(0, JSON.stringify(listEnvelope([{ "id": "retail", "key": "k-retail", "label": "retail",
      "counts": { "unmanaged": 0 }, "addons": addons }])))
    tryVerify(function() { return frame.rowCount === addons.length })
    return { "service": service, "frame": frame }
  }

  // `x` on an Installed row: the dry run first, its text and effects in the dialog, Yes
  // applies that preview's plan; a stale plan previews again instead of removing.
  function test_x_previews_the_removal_and_yes_runs_its_plan() {
    var real = realFrame([addon("Baz", "v1", "v1", "current")])
    var frame = real.frame
    keyClick("x")
    var preview = launches("remove")
    compare(preview.length, 1)
    verify(preview[0].command.indexOf("--dry-run") >= 0)
    compare(preview[0].command.slice(-5), ["remove", "-t", "retail", "--", "Baz"])
    var removal = function(version) {
      return planPreviewJson("remove", { "target": { "id": "retail" }, "items": [{ "name": "Baz", "version": version, "folders": ["Baz"] }],
        "reload_required": "none" }, approvalOf("remove", "Baz", version, null, ["folders_removed", "destructive"], { "folders_removed": ["Baz"] }),
        true, [{ "code": "still_required", "message": "LittleWigs still requires Baz", "fix": null }])
    }
    preview[0].process.finish(0, removal("v1"))
    tryVerify(function() { return frame.confirming })
    var sheet = findChild(frame, "planSheet")
    // The removal by name, its folder, and who else needs it: urgent, first, by the
    // warning's code (its text never shows).
    var text = Model.dialogText(sheet.plan)
    compare(text.split("\n").slice(1), ["Baz v1 (1 folder)", "Removes the folder Baz. Undo brings it back.",
      "Other addons still need Baz and won't load without it."])
    verify(text.indexOf("Remove Baz from ") === 0, text)
    verify(sheet.destructive)
    compare(findChild(sheet, "sheetConfirm").text, String.fromCodePoint(0xF01B4) + " Remove Baz")
    compare(launches("apply").length, 0, "nothing runs before Yes")
    tryVerify(function() { return sheet.keyItem.activeFocus })
    keyClick("y")
    var apply = launches("apply")
    compare(apply.length, 1, "Yes applies the preview's plan")
    compare(apply[0].command.slice(-2), ["apply", planId])
    verify(apply[0].command.indexOf("--if-match") < 0)
    verify(apply[0].command.indexOf("--dry-run") < 0)
    compare(launches("remove").length, 1, "the removal runs only through its plan")
    // Something changed since the preview: nothing is removed, and it previews again.
    apply[0].process.finish(1, JSON.stringify({ "ok": false, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION,
      "command": "apply", "exit_code": 1, "warnings": [], "commands": [],
      "error": { "code": "plan_stale", "message": "plan " + planId + " is no longer what its preview showed", "fix": "omaddons remove Baz -t retail --dry-run",
        "fix_argv": ["omaddons", "remove", "Baz", "-t", "retail", "--dry-run"], "alternatives": [], "retryable": false, "details": { "changed": ["Baz"] } } }))
    tryVerify(function() { return launches("remove").length === 2 })
    var again = launches("remove")[1]
    verify(again.command.indexOf("--dry-run") >= 0)
    again.process.finish(0, removal("v2"))
    tryVerify(function() { return frame.confirming })
    // The banner says it changed (a part this build does not know is no word to show),
    // over the new preview's items.
    verify(findChild(sheet, "planStale").visible)
    compare(sheet.plan.stale, "Changed since you looked: something it depends on. Nothing ran; here is what would happen now.")
    compare(sheet.plan.items.map(function(i) { return i.text }), ["Baz v2 (1 folder)"])
    compare(sheet.selectedIndex, 0)
    compare(launches("apply").length, 1, "apply never runs twice for one preview")
    real.service.destroy()
  }

  // Repair on a drifted Installed addon: the reinstall previews and asks first, always
  // (`preview: confirm`); Yes applies its plan.
  function test_repair_of_an_installed_drifted_addon_confirms_the_reinstall() {
    var real = realFrame([addon("Foo", "v1", "v1", "current", { "drift": { "state": "modified", "files": 1, "folders": [] } })])
    var frame = real.frame
    frame.cursorActive = true
    frame.currentView.handleKey("repair", 0)
    var preview = launches("--reinstall")
    compare(preview.length, 1)
    verify(preview[0].command.indexOf("--dry-run") >= 0)
    compare(preview[0].command.slice(-6), ["install", "-t", "retail", "--reinstall", "--", "Foo"])
    compare(real.service.action.message, "Checking what it would do…")
    preview[0].process.finish(0, planPreviewJson("install", { "target": { "id": "retail" },
      "items": [{ "name": "Foo", "status": "updated", "from_version": "v1", "to_version": "v1" }] },
      approvalOf("update", "Foo", "v1", "v1", ["reinstall"], {}), false))
    tryVerify(function() { return frame.confirming })
    var sheet = findChild(frame, "planSheet")
    // A reinstall is a repair of the addon's files, in the player's words.
    verify(sheet.plan.title.indexOf("Repair Foo on ") === 0, sheet.plan.title)
    compare(sheet.plan.confirmLabel, "Repair Foo")
    compare(sheet.plan.effects.map(function(e) { return e.text }), ["Puts back Foo's files exactly as released."])
    compare(launches("apply").length, 0, "a reinstall asks first")
    tryVerify(function() { return sheet.keyItem.activeFocus })
    keyClick("y")
    compare(launches("apply").length, 1, "Yes applies the plan")
    real.service.destroy()
  }
}
