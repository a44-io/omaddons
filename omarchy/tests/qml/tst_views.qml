import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "../../Store.js" as Store

// The Updates and Installed views inside the frame (pipeline step 4). Against a stub
// service: rows, groups and markers, what Enter, `p`, `i` and `x` run (template and
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
        { "id": "era", "running": false, "runningUnknown": false, "clientDir": "/games/era", "updates": { "checkedAt": "2100-01-01T00:00:00Z" }, "issues": [] },
        { "id": "retail", "running": true, "runningUnknown": false, "clientDir": "/games/retail", "updates": { "checkedAt": "2100-01-01T00:00:00Z" }, "issues": [] }
      ]
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

      property var runs: []
      property var notes: []
      property var reads: []

      function run(name, slots, label) { runs = runs.concat([[name, slots, label]]); return "" }
      function runCliArgv(argv, label) { return "" }
      function readView(view) { reads = reads.concat([view]) }
      function readChangelog(target, addon) { notes = notes.concat([[target, addon]]) }
      function check() {}
      function refresh() {}
      function reinstallWidget() { return "ok" }
      function confirmYes() { confirm = null; return "" }
      function confirmNo() { confirm = null }
    }
  }

  Component {
    id: frameComponent
    PanelView {
      width: 540
      height: 700
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
    v.focusItem.forceActiveFocus()
    tryVerify(function() { return v.focusItem.activeFocus })
    return v
  }

  function init() {
    Quickshell.resetDetachedCommands()
    svc = serviceComponent.createObject(testCase)
    svc.lists = Model.reduceList(listing())
    view = make(svc)
  }

  function cleanup() {
    for (var i = 0; i < made.length; i++) made[i].destroy()
    made = []
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

  // The command words of the argv Model.buildArgv makes of a recorded run.
  function words(run) {
    var built = Model.buildArgv(run[0], run[1], { "binary": "/opt/omaddons", "label": run[2], "context": Build.CONTEXT })
    var argv = built.argv
    var at = argv.indexOf("--reason") + 2
    while (argv[at] === "--progress" || argv[at] === "--dry-run" || argv[at] === "--if-match") at += argv[at] === "--dry-run" ? 1 : 2
    return argv.slice(at)
  }

  // ------------------------------------------------------------- Updates

  function test_updates_are_grouped_by_client_with_the_count_u_runs() {
    var updates = content()
    compare(json(updates.rows.map(function(r) { return [r.group, r.name, r.kind] })),
      json([["era", "Bar", "update"], ["", "Plater", "failed"], ["retail", "DBM", "update"], ["", "WeakAuras", "pinned"]]))
    compare(findChild(updates, "updateAll").text, "Update all (2)")
    // A running client's update loads after a /reload.
    verify(findChild(row("updateRow", 2), "reloadChip").visible)
    verify(!findChild(row("updateRow", 0), "reloadChip").visible)
    keyClick("t")
    keyClick("t")
    compare(view.filter, "retail")
    compare(findChild(content(), "updateAll").text, "Update retail (1)")
    findChild(content(), "updateAll").clicked()
    compare(json(svc.runs[0]), json(["update_target", { "target": "retail" }, "Update retail"]))
  }

  function test_enter_updates_one_addon_and_p_pins_by_its_state() {
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["update_addon", { "target": "era", "addon": "Bar" }, "Update Bar"]))
    compare(json(words(svc.runs[0])), json(["update", "-t", "era", "--", "Bar"]))
    // A failed check has nothing to update.
    keyClick("j")
    keyClick(Qt.Key_Return)
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
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 3)
  }

  function test_i_shows_the_notes_of_its_own_row_and_safe_links_only() {
    keyClick("j")
    keyClick("j")
    keyClick("i")
    compare(json(svc.notes), json([["retail", "DBM"]]))
    var entry = row("updateRow", 2)
    verify(findChild(entry, "notes").visible)
    compare(findChild(entry, "notesText").text, "Reading the release notes…")
    // A late answer for another row is never shown here.
    svc.changelog = { "selection": { "target": "era", "addon": "Bar" }, "data": { "changelog": { "entries": [{ "version": "v2", "text": "Bar notes" }], "more_url": "https://github.com/o/bar/releases" } } }
    compare(findChild(entry, "notesText").text, "Reading the release notes…")
    svc.changelog = { "selection": { "target": "retail", "addon": "DBM" }, "data": { "changelog": { "entries": [{ "version": "12.1.15", "text": "Fixes the pull timer." }], "more_url": "https://github.com/o/dbm/releases" } } }
    compare(findChild(entry, "notesText").text, "Fixes the pull timer.")
    var link = findChild(entry, "notesLink")
    verify(link.visible)
    compare(link.text, "more on GitHub")
    link.clicked()
    compare(json(Quickshell.detachedCommands[0]), json(["omarchy-launch-browser", "https://github.com/o/dbm/releases"]))
    svc.changelog = { "selection": { "target": "retail", "addon": "DBM" }, "data": { "changelog": { "entries": [], "more_url": "http://evil.example/x" } } }
    verify(!findChild(entry, "notesLink").visible)
    keyClick("i")
    verify(!findChild(entry, "notes").visible)
  }

  function test_nothing_to_update_says_when_it_was_checked() {
    svc.lists = Model.reduceList(listEnvelope([{ "id": "era", "key": "k", "label": "era", "counts": { "unmanaged": 0 },
      "addons": [addon("Bar", "v2", "v2", "current")] }]))
    view.nowMs = Date.parse("2100-01-01T00:25:00Z")
    var empty = findChild(content(), "updatesEmpty")
    verify(empty.visible)
    compare(empty.text, "Everything is current · checked 25 min ago")
    verify(!findChild(content(), "updateAll").visible)
  }

  // ------------------------------------------------------------- Installed

  function test_installed_rows_carry_their_markers_and_enter_repairs_or_updates() {
    keyClick("2")
    var installed = content()
    compare(json(installed.rows.map(function(r) { return [r.group, r.name, r.markers, r.primary] })), json([
      ["era", "Bar", [], "update"], ["", "Plater", [], ""],
      ["retail", "BigWigs", ["dep"], ""], ["", "DBM", [], "update"], ["", "Foo", ["modified"], "reinstall"], ["", "WeakAuras", ["pinned"], ""]]))
    compare(findChild(row("installedRow", 2), "markers").text, "dep")
    // Foo: Enter reinstalls its modified files.
    for (var i = 0; i < 4; i++) keyClick("j")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["reinstall", { "target": "retail", "addon": "Foo" }, "Reinstall Foo"]))
    compare(json(words(svc.runs[0])), json(["install", "-t", "retail", "--reinstall", "--", "Foo"]))
    // BigWigs: nothing waits, so Enter does nothing.
    keyClick("k")
    keyClick("k")
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 1)
    keyClick("x")
    compare(json(svc.runs[1]), json(["remove", { "target": "retail", "addon": "BigWigs" }, "Remove BigWigs"]))
    keyClick("p")
    compare(json(svc.runs[2]), json(["pin", { "target": "retail", "addon": "BigWigs" }, "Pin BigWigs"]))
  }

  function test_unmanaged_folders_are_a_hint_and_an_empty_client_says_how_to_add() {
    keyClick("2")
    var lines = findChild(content(), "unmanaged")
    compare(lines.count, 1)
    compare(lines.itemAt(0).text, "2 unmanaged folders on retail: omaddons adopt -t retail")
    svc.lists = Model.reduceList(listEnvelope([{ "id": "era", "key": "k", "label": "era", "counts": { "unmanaged": 0 }, "addons": [] }]))
    verify(findChild(content(), "installedEmpty").visible)
  }

  function test_an_incompatible_binary_leaves_the_rows_inert() {
    keyClick("2")
    svc.phase = "incompatible"
    keyClick(Qt.Key_Return)
    keyClick("x")
    keyClick("p")
    compare(svc.runs.length, 0)
  }

  // ------------------------------------------------------------- the real service

  // `x` on an Installed row: the dry run first, its text in the dialog, Yes bound to its
  // token; a stale token previews again instead of removing.
  function test_x_previews_the_removal_and_yes_runs_it_bound() {
    svc.destroy()
    svc = null
    ProcessRegistry.reset()
    var service = realService.createObject(testCase, { "bellDebounceMs": 10, "localDeadlineMs": 2000 })
    verify(service !== null)
    made = made.concat([service])
    var launches = function(word) {
      return ProcessRegistry.launches.filter(function(l) {
        return l.command[1] === "--json" && l.command.indexOf(word) >= 0
      })
    }
    var status = launches("status")[0]
    status.process.finish(0, JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION,
      "command": "status", "exit_code": 0, "partial": false, "warnings": [], "commands": [],
      "data": { "targets": [{ "id": "retail", "running": false, "updates": { "available": 1 }, "issues": [] }],
        "summary": { "targets": 1, "running": [], "updates": { "available": 1, "targets": ["retail"] },
          "issues": { "p0": 0, "p1": 0, "p2": 0, "p3": 0 }, "errors": { "new": 0, "targets": [] },
          "check_due": false, "widget_outdated": false, "state": "updates" },
        "last_change": null, "last_change_state": "ok" } }))
    tryCompare(service, "phase", "ok")
    var frame = make(service)
    keyClick("2")
    var list = launches("list")
    verify(list.length > 0, "switching to Installed reads list")
    list[list.length - 1].process.finish(0, JSON.stringify(listEnvelope([{ "id": "retail", "key": "k-retail", "label": "retail",
      "counts": { "unmanaged": 0 }, "addons": [addon("Baz", "v1", "v1", "current")] }])))
    tryVerify(function() { return frame.rowCount === 1 })
    keyClick("x")
    var preview = launches("remove")
    compare(preview.length, 1)
    verify(preview[0].command.indexOf("--dry-run") >= 0)
    compare(preview[0].command.slice(-5), ["remove", "-t", "retail", "--", "Baz"])
    var token = "pc1:" + "a".repeat(64)
    preview[0].process.finish(0, JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION,
      "command": "remove", "exit_code": 0, "partial": false, "commands": [],
      "warnings": [{ "code": "still_required", "message": "LittleWigs still requires Baz", "fix": null }],
      "data": { "dry_run": true, "target": { "id": "retail" }, "items": [{ "name": "Baz", "version": "v1", "folders": ["Baz"] }],
        "reload_required": "none", "precondition": token } }))
    tryVerify(function() { return frame.confirming })
    var dialog = findChild(frame, "confirmDialog")
    compare(dialog.message, "Remove Baz v1 from retail (1 folder)\nLittleWigs still requires Baz")
    tryVerify(function() { return findChild(frame, "confirmKeys").activeFocus })
    keyClick("y")
    var real = launches("remove")
    compare(real.length, 2)
    var at = real[1].command.indexOf("--if-match")
    verify(at > 0)
    compare(real[1].command[at + 1], token)
    verify(real[1].command.indexOf("--dry-run") < 0)
    // Something changed since the preview: nothing is removed, and it previews again.
    real[1].process.finish(1, JSON.stringify({ "ok": false, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION,
      "command": "remove", "exit_code": 1, "warnings": [], "commands": [],
      "error": { "code": "plan_stale", "message": "this remove is no longer what its preview showed", "fix": "omaddons list",
        "fix_argv": ["omaddons", "list"], "alternatives": [], "retryable": false, "details": { "changed": ["Baz"] } } }))
    tryVerify(function() { return launches("remove").length === 3 })
    var again = launches("remove")[2]
    verify(again.command.indexOf("--dry-run") >= 0)
    again.process.finish(0, JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION,
      "command": "remove", "exit_code": 0, "partial": false, "commands": [], "warnings": [],
      "data": { "dry_run": true, "target": { "id": "retail" }, "items": [{ "name": "Baz", "version": "v2", "folders": ["Baz"] }],
        "reload_required": "none", "precondition": "pc1:" + "b".repeat(64) } }))
    tryVerify(function() { return frame.confirming })
    verify(dialog.message.indexOf("Changed since you looked: Baz.") === 0)
    verify(dialog.message.indexOf("Remove Baz v2 from retail") > 0)
    compare(dialog.selectedIndex, 0)
    service.destroy()
  }
}
