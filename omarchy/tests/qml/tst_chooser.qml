import QtQuick
import QtTest
import Quickshell
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build

// The restore chooser over the frame (pipeline step 4, 1n2p.29), against a stub service:
// opened from a Backups row's `r`, from Installed's and Errors' `r` and from a backup's
// page; from an addon, the newest backup with the addon's own settings first (J6: `r`,
// Enter, `y`) and the backup a choice of its own; from a backup, one addon's settings or
// all of them with what else that changes; each addon's captured version from the
// view-bound `-v` read; "Quit Retail first" and nothing restored while the client runs;
// Esc going back a step, then closing; and the footer's buttons for a restore whose
// folders changed owner.
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
      property var targets: [{ "id": "retail", "displayName": "Retail", "running": false, "runningUnknown": false, "clientDir": "/games/retail",
        "updates": null, "issues": [] }]
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
      // An addon's page (1n2p.23): its own read, answered with nothing.
      property var addonInfo: null
      property var history: null
      function readAddonInfo(ref) { return true }
      function clearAddonInfo() { addonInfo = null }
      function readHistory() { return true }
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

  function when(iso) {
    return Model.whenText(iso, view.nowMs)
  }

  function test_from_a_backup_one_addon_or_all_and_all_says_what_else_it_changes() {
    keyClick("6")
    keyClick("r")
    var c = chooser()
    verify(c.opened)
    tryVerify(function() { return findChild(c, "sheetKeys").activeFocus })
    compare([c.title, findChild(c, "chooserFrom").text], ["Restore on Retail", "From: " + when("2099-12-31T22:00:00Z") + " · made by you"])
    compare(json(c.choiceTexts), json(["One addon's settings…", "All of Retail's settings…"]))
    compare(findChild(findChild(c, "restoreChoice_1"), "choiceNote").text, "Keybindings, macros, chat and every addon's settings")
    // Behind the chooser the frame does nothing, and `y` chooses nothing.
    keyClick("b")
    keyClick("y")
    compare(svc.runs.length, 0)
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["restore", { "target": "retail", "backup": newer }, "Restore all of Retail's settings"]))
    verify(!c.opened)
    compare(svc.detailClears, 1)
    tryVerify(function() { return findChild(view, "keyCatcher").activeFocus })
  }

  function test_one_addons_settings_asks_which_addon_from_the_backups_own_list() {
    keyClick("6")
    keyClick("r")
    var c = chooser()
    keyClick(Qt.Key_Return)
    compare([c.step, c.title], ["addon", "Whose settings, on Retail?"])
    // Until the -v read answers: the managed addons.
    compare(json(c.choiceTexts), json(["WeakAuras", "Details"]))
    svc.backupsDetail = { "selection": { "target": "retail" }, "data": listing(true) }
    compare(json(c.choiceTexts), json(["WeakAuras 5.20.0"]))
    keyClick(Qt.Key_Escape)
    compare(c.step, "scope", "Esc goes back a step")
    svc.backupsDetail = null
    keyClick(Qt.Key_Return)
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["restore_addon", { "target": "retail", "addon": "Details", "backup": newer }, "Restore Details' settings"]))
  }

  // Design doc J6: `r` on the addon, Enter ("WeakAuras' settings", first), then `y`.
  function test_installed_r_opens_on_the_newest_backup_with_the_addons_own_settings_first() {
    keyClick("2")
    // WeakAuras, after Details by name (1n2p.22).
    keyClick("j")
    compare(findChild(view, "content").item.rows[view.cursor].name, "WeakAuras")
    keyClick("r")
    var c = chooser()
    verify(c.opened)
    compare([c.step, c.addon, c.origin, c.choice, c.title], ["scope", "WeakAuras", "addon", 1, "Restore WeakAuras' settings on Retail"])
    compare(json(svc.detailReads), json(["retail"]))
    compare(json(c.choiceTexts), json(["From: " + when("2099-12-31T22:00:00Z") + " · made by you", "WeakAuras' settings", "All of Retail's settings…"]))
    // A -v read for another client is not this chooser's.
    svc.backupsDetail = { "selection": { "target": "era" }, "data": listing(true) }
    compare(c.choiceTexts[1], "WeakAuras' settings")
    svc.backupsDetail = { "selection": { "target": "retail" }, "data": listing(true) }
    compare(c.choiceTexts[1], "WeakAuras' settings (written by 5.20.0)")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["restore_addon", { "target": "retail", "addon": "WeakAuras", "backup": newer }, "Restore WeakAuras' settings"]))
    verify(!c.opened)
  }

  // Backups' own list is read with its view (1n2p.38): from Addons, the chooser waits for
  // its -v read, never saying there is none, and opens on the narrow fix once it lands.
  function test_from_an_addon_it_reads_the_backups_itself_and_starts_on_the_addons_settings() {
    svc.backups = null
    keyClick("2")
    keyClick("j")
    keyClick("r")
    var c = chooser()
    verify(c.opened)
    compare(json(c.choiceTexts), json([]))
    compare(findChild(c, "chooserEmpty").text, "Reading backups…")
    svc.backupsDetail = { "selection": { "target": "retail" }, "data": listing(true) }
    compare([c.choiceTexts.length, c.choice], [3, 1])
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["restore_addon", { "target": "retail", "addon": "WeakAuras", "backup": newer }, "Restore WeakAuras' settings"]))
  }

  function test_the_backup_is_a_choice_of_its_own_with_the_version_each_holds() {
    svc.backupsDetail = null
    keyClick("2")
    keyClick("j")
    keyClick("r")
    var c = chooser()
    svc.backupsDetail = { "selection": { "target": "retail" }, "data": listing(true) }
    keyClick("k")
    keyClick(Qt.Key_Return)
    compare([c.step, c.title, c.choice], ["backup", "Restore WeakAuras' settings from which backup?", 0])
    compare(json(c.choiceTexts), json([when("2099-12-31T22:00:00Z") + " · made by you · WeakAuras 5.20.0",
      when("2099-12-30T00:00:00Z") + " · made by you · WeakAuras 5.19.2"]))
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare([c.step, c.backup, c.choice], ["scope", older, 1])
    compare(c.choiceTexts[1], "WeakAuras' settings (written by 5.19.2)")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["restore_addon", { "target": "retail", "addon": "WeakAuras", "backup": older }, "Restore WeakAuras' settings"]))
  }

  // `r` on an addon's page (its client line): the backups with the version that wrote
  // that addon's settings, from the client's -v read.
  function test_an_addon_pages_r_lists_backups_with_the_version_that_wrote_its_settings() {
    keyClick("2")
    keyClick("j")
    keyClick(Qt.Key_Return)
    var page = findChild(view, "page").item
    verify(page !== null, "WeakAuras' page")
    keyClick("j")
    keyClick("r")
    var c = chooser()
    verify(c.opened)
    compare([c.origin, c.target, c.addon], ["addon", "retail", "WeakAuras"])
    svc.backupsDetail = { "selection": { "target": "retail" }, "data": listing(true) }
    keyClick("k")
    keyClick(Qt.Key_Return)
    compare(json(c.choiceTexts), json([when("2099-12-31T22:00:00Z") + " · made by you · WeakAuras 5.20.0",
      when("2099-12-30T00:00:00Z") + " · made by you · WeakAuras 5.19.2"]))
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
    compare([c.target, c.addon, c.step, c.origin], ["retail", "WeakAuras", "scope", "addon"])
  }

  // M3-D29: while the client runs, or nobody can tell, nothing restores; the way stays shown.
  function test_restores_are_off_while_the_client_runs() {
    svc.targets = [{ "id": "retail", "displayName": "Retail", "running": true, "runningUnknown": false, "clientDir": "/games/retail", "updates": null,
      "issues": [] }]
    keyClick("6")
    keyClick("r")
    var c = chooser()
    verify(findChild(c, "chooserBlocked").visible)
    compare(findChild(c, "chooserBlocked").text, "Quit Retail first.")
    keyClick(Qt.Key_Return)
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare([svc.runs.length, c.step], [0, "scope"])
    keyClick(Qt.Key_Escape)
    verify(!c.opened)
    svc.targets = [{ "id": "retail", "displayName": "Retail", "running": false, "runningUnknown": true, "clientDir": "/games/retail", "updates": null,
      "issues": [] }]
    keyClick("r")
    compare(findChild(c, "chooserBlocked").text, "Quit Retail first: omaddons can't tell if it's running.")
  }

  function test_esc_goes_back_a_step_then_closes() {
    keyClick("2")
    keyClick("r")
    var c = chooser()
    keyClick("k")
    keyClick(Qt.Key_Return)
    compare(c.step, "backup")
    keyClick(Qt.Key_Escape)
    compare([c.step, c.choice], ["scope", 1])
    verify(c.opened)
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
