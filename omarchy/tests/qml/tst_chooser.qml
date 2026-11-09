import QtQuick
import QtTest
import Quickshell
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build

// The restore chooser over the frame (pipeline step 4), against a stub service: opened
// from a Backups row, from Installed's `r` and from Issues' `r`; the addon scope first,
// the whole WTF/ second with what else it changes; each addon's captured version from
// the view-bound `-v` read; restores off while the client runs; Esc going back; and the
// footer's buttons for a restore whose folders changed owner.
TestCase {
  id: testCase
  name: "Chooser"
  when: windowShown
  visible: true
  width: 600
  height: 720

  property var svc: null
  property var view: null

  readonly property string newer: "01RET00000000000000000000B"
  readonly property string older: "01RET00000000000000000000A"

  function backup(id, at) {
    return { "id": id, "created_at": at, "captured_at": at, "kind": "manual", "trigger": null, "files": 5, "bytes": 1218,
      "reflinked": "none", "client_running": false, "unsettled": 0, "consistency": "stable", "integrity": "metadata_only",
      "verified_at": null, "baseline": null, "restorable": true, "qualifying": true, "busy": false }
  }

  function listing(withVersions) {
    var b = [backup(newer, "2099-12-31T22:00:00Z"), backup(older, "2099-12-30T00:00:00Z")]
    if (withVersions) {
      b[0].addons = [{ "name": "WeakAuras", "folders": ["WeakAuras"], "version": "5.20.0" }]
      b[1].addons = [{ "name": "WeakAuras", "folders": ["WeakAuras"], "version": "5.19.2" }]
    }
    return Model.reduceBackups({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": "backup list",
      "exit_code": 0, "partial": false, "warnings": [], "commands": [],
      "data": { "targets": [{ "target": { "id": "retail" }, "auto": null, "bytes": 2436, "free_bytes": 1073741824, "shown": 2, "total": 2, "backups": b }] } })
  }

  Component {
    id: serviceComponent
    QtObject {
      property string phase: "ok"
      readonly property bool canAct: phase === "ok"
      property var summary: null
      property var targets: [{ "id": "retail", "running": false, "runningUnknown": false, "clientDir": "/games/retail", "updates": null, "issues": [] }]
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
      property var errors: null
      property var statusWarnings: []
      property var backups: null
      property var backupsDetail: null

      property var runs: []
      property var detailReads: []
      property int detailClears: 0
      property var folders: []

      function run(name, slots, label) { runs = runs.concat([[name, slots, label]]); return "" }
      function runCliArgv(argv, label) { return "" }
      function readView(view) {}
      function readBackupsDetail(target) { detailReads = detailReads.concat([target]) }
      function clearBackupsDetail() { detailClears++; backupsDetail = null }
      function restoreFolder(addon) { folders = folders.concat([addon]); return "" }
      function check() {}
      function refresh() {}
      function reinstallWidget() { return "ok" }
      function confirmYes() { return "" }
      function confirmNo() {}
      function dismissErrors(plan) { return "" }
    }
  }

  Component {
    id: frameComponent
    PanelView {
      width: 540
      height: 700
    }
  }

  function init() {
    svc = serviceComponent.createObject(testCase)
    svc.backups = listing(false)
    svc.lists = Model.reduceList({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": "list", "exit_code": 0,
      "partial": false, "warnings": [], "commands": [], "data": { "targets": [{ "id": "retail", "key": "k", "label": "retail", "counts": { "unmanaged": 0 },
        "addons": [{ "name": "WeakAuras", "version": "5.20.0", "ref": "gh:o/WeakAuras", "pinned": false, "state": "ok", "reason": "explicit",
          "required_by": [], "orphan": false, "latest": { "version": "5.20.0", "status": "current" }, "folders": ["WeakAuras"],
          "drift": { "state": "clean" } },
          { "name": "Details", "version": "1", "ref": "wowi:1", "pinned": false, "state": "ok", "reason": "explicit",
          "required_by": [], "orphan": false, "latest": { "version": "1", "status": "current" }, "folders": ["Details"],
          "drift": { "state": "clean" } }] }] } })
    view = frameComponent.createObject(testCase, { "svc": svc })
    view.opened = true
    view.nowMs = Date.parse("2100-01-01T00:00:00Z")
    view.focusItem.forceActiveFocus()
    tryVerify(function() { return view.focusItem.activeFocus })
  }

  function cleanup() {
    view.destroy()
    svc.destroy()
    view = null
    svc = null
  }

  function chooser() {
    return findChild(view, "chooser")
  }

  function json(value) {
    return JSON.stringify(value)
  }

  function test_from_a_backup_the_addon_scope_comes_first_and_whole_says_what_else_it_changes() {
    keyClick("6")
    keyClick(Qt.Key_Return)
    var c = chooser()
    verify(c.opened)
    tryVerify(function() { return findChild(c, "chooserKeys").activeFocus })
    compare(json(c.choices), json(["One addon's settings…",
      "Whole WTF/: also restores keybindings, macros, chat settings and every other addon's settings"]))
    // Behind the chooser the frame does nothing.
    keyClick("b")
    compare(svc.runs.length, 0)
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["restore", { "target": "retail", "backup": newer }, "Restore the whole WTF/"]))
    verify(!c.opened)
    compare(svc.detailClears, 1)
    tryVerify(function() { return findChild(view, "keyCatcher").activeFocus })
  }

  function test_one_addons_settings_asks_which_addon() {
    keyClick("6")
    keyClick(Qt.Key_Return)
    var c = chooser()
    keyClick(Qt.Key_Return)
    compare(c.step, "addon")
    compare(json(c.choices), json(["WeakAuras", "Details"]))
    keyClick(Qt.Key_Escape)
    compare(c.step, "scope", "Esc goes back a step")
    keyClick(Qt.Key_Return)
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["restore_addon", { "target": "retail", "addon": "Details", "backup": newer }, "Restore Details's settings"]))
  }

  function test_installed_r_picks_a_backup_by_its_captured_version_then_the_scope() {
    keyClick("2")
    // WeakAuras, after Details by name (1n2p.22).
    keyClick("j")
    compare(findChild(view, "content").item.rows[view.cursor].name, "WeakAuras")
    keyClick("r")
    var c = chooser()
    verify(c.opened)
    compare([c.step, c.addon], ["backup", "WeakAuras"])
    compare(json(svc.detailReads), json(["retail"]))
    compare(c.choices[0], "2 hours ago · manual")
    // A -v read for another client is not this chooser's.
    svc.backupsDetail = { "selection": { "target": "era" }, "data": listing(true) }
    compare(c.choices[0], "2 hours ago · manual")
    svc.backupsDetail = { "selection": { "target": "retail" }, "data": listing(true) }
    compare(json(c.choices), json(["2 hours ago · manual · WeakAuras 5.20.0", "2 days ago · manual · WeakAuras 5.19.2"]))
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare([c.step, c.backup], ["scope", older])
    compare(c.choices[0], "WeakAuras's settings")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["restore_addon", { "target": "retail", "addon": "WeakAuras", "backup": older }, "Restore WeakAuras's settings"]))
  }

  // `r` in the Errors view (4, 1n2p.27) on the erroring addon.
  function test_errors_r_opens_on_the_erroring_addon() {
    svc.errors = Model.reduceErrors({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": "errors", "exit_code": 0,
      "partial": false, "warnings": [], "commands": [], "data": { "targets": [{ "target": { "id": "retail" }, "capture": "buggrabber",
        "coverage": "complete", "files": { "partial": 0 }, "databases": [], "summary": { "new": 1, "received": 0 }, "dismiss": null,
        "addons": [{ "addon": "WeakAuras", "managed": true, "kind": "addon", "version": "5.20.0", "latest": null, "count": 1, "new": 1,
          "errors": [] }] }] } })
    keyClick("4")
    keyClick("r")
    var c = chooser()
    verify(c.opened)
    compare([c.target, c.addon, c.step], ["retail", "WeakAuras", "backup"])
  }

  function test_restores_are_off_while_the_client_runs() {
    svc.targets = [{ "id": "retail", "running": true, "runningUnknown": false, "clientDir": "/games/retail", "updates": null, "issues": [] }]
    keyClick("6")
    keyClick(Qt.Key_Return)
    var c = chooser()
    verify(findChild(c, "chooserBlocked").visible)
    compare(findChild(c, "chooserBlocked").text, "Restores are off: quit the game first")
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 0)
    keyClick(Qt.Key_Escape)
    verify(!c.opened)
  }

  function test_esc_from_the_first_step_closes() {
    keyClick("2")
    keyClick("r")
    var c = chooser()
    keyClick(Qt.Key_Return)
    compare(c.step, "scope")
    keyClick(Qt.Key_Escape)
    compare([c.step, c.backup], ["backup", ""])
    keyClick(Qt.Key_Escape)
    verify(!c.opened)
  }

  function test_a_folder_that_changed_owner_offers_one_button_per_folder() {
    svc.lastError = { "message": "WeakAurasOptions was WeakAuras's when the backup was taken", "code": "ref_ambiguous", "fixes": [],
      "fixText": "", "choices": Model.ambiguityChoices({ "ok": false, "error": { "code": "ref_ambiguous", "alternatives": [
        { "argv": ["omaddons", "--actor", "widget", "backup", "restore", "-t", "retail", "--addon", "folder:WeakAuras", "--", older] },
        { "argv": ["omaddons", "--actor", "widget", "backup", "restore", "-t", "retail", "--addon", "folder:WeakAurasOptions", "--", older] }] } }),
      "folder": "" }
    compare(findChild(view, "choice_0").text, "Restore the WeakAuras folder")
    findChild(view, "choice_1").clicked()
    compare(json(svc.folders), json(["folder:WeakAurasOptions"]))
    svc.lastError = { "message": "this filesystem cannot exchange", "code": "filesystem_unsupported", "fixes": [], "choices": [],
      "fixText": "with the game closed, copy what you need by hand from the backup's own folder", "folder": "/games/retail/Interface/.omaddons/backups/01M/WTF" }
    compare(findChild(view, "backupFolder").text, "The backup's own folder: /games/retail/Interface/.omaddons/backups/01M/WTF")
  }
}
