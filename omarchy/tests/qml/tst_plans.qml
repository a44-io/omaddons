import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "Fixtures.js" as Fixtures

// The plan runner (M4-D28) over envelopes the real binary wrote (pipeline step 1's plan
// scenes, through the Fixtures.js step 4 generates): the Service on the mock Process,
// given what the views show of a `list` fixture, answered with the binary's previews and
// applies. The first three names are fixed: the slice-2 demo asserts them.
TestCase {
  id: testCase
  name: "Plans"

  property var service: null

  Component {
    id: serviceComponent
    Service {}
  }

  function make() {
    if (service) service.destroy()
    service = null
    wait(0)
    ProcessRegistry.reset()
    Quickshell.resetDetachedCommands()
    service = serviceComponent.createObject(testCase, { "bellDebounceMs": 10, "localDeadlineMs": 400, "networkDeadlineMs": 2000,
      "killGraceMs": 60, "streamGraceMs": 60 })
    verify(service !== null)
  }

  function init() {
    make()
    ready()
  }

  function cleanup() {
    if (service) service.destroy()
    service = null
    ProcessRegistry.reset()
    wait(0)
  }

  // ---- helpers (tst_service.qml's)

  // The command words and arguments after the standard flags.
  function words(command) {
    var i = 6
    while (i < command.length) {
      if (command[i] === "--progress" || command[i] === "--if-match") i += 2
      else if (command[i] === "--dry-run") i += 1
      else break
    }
    return command.slice(i)
  }

  function runs(word) {
    var out = []
    for (var i = 0; i < ProcessRegistry.launches.length; i++) {
      var launch = ProcessRegistry.launches[i]
      if (launch.command[1] !== "--json" || launch.command[2] !== "--actor") continue
      if (word === undefined || words(launch.command)[0] === word) out.push(launch)
    }
    return out
  }

  function lastRun(word) {
    var all = runs(word)
    return all.length ? all[all.length - 1] : null
  }

  // Every launch so far, by its words, for a failure's log.
  function launchLog() {
    return JSON.stringify(ProcessRegistry.launches.map(function(l) { return words(l.command) }))
  }

  function envelope(command, data) {
    return JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": command,
      "exit_code": 0, "partial": false, "data": data, "warnings": [], "commands": [] })
  }

  function statusEnvelope() {
    return envelope("status", { "cached": true,
      "targets": [{ "id": "retail", "label": "retail", "running": false, "updates": { "available": 1 }, "issues": [] }],
      "summary": { "targets": 1, "running": [], "updates": { "available": 1, "targets": ["retail"] },
        "issues": { "p0": 0, "p1": 0, "p2": 0, "p3": 0 }, "errors": { "new": 0, "targets": [] },
        "check_due": false, "widget_outdated": false, "state": "updates" },
      "last_change": null, "last_change_state": "ok" })
  }

  // The binary's fixture `name` as the answer of `launch` (exit `code`, 0 by default).
  function answer(launch, name, code) {
    verify(launch !== null, "a launch to answer with " + name + ": " + launchLog())
    launch.process.finish(code || 0, Fixtures.text(name))
  }

  // The startup status, the catch-up checkpoint it queues and the refresh after it.
  function ready() {
    lastRun("status").process.finish(0, statusEnvelope())
    compare(service.phase, "ok")
    lastRun("backup").process.finish(0, envelope("backup create", { "targets": [] }))
    lastRun("status").process.finish(0, statusEnvelope())
  }

  function planIdOf(name) {
    return JSON.parse(Fixtures.text(name)).data.plan_id
  }

  // The open plan sheet, and its effects' lines.
  function sheet() {
    verify(service.confirm !== null && !!service.confirm.sheet, "a plan sheet is open: " + launchLog())
    return service.confirm.sheet
  }

  function effects() {
    return sheet().effects.map(function(e) { return e.text })
  }

  // The list the views read, from the binary's `list` fixture `name`.
  function showList(name) {
    service.lists = Model.reduceList(JSON.parse(Fixtures.text(name)))
  }

  // Enter on Foo's Updates row on retail: its update, previewed with what the row shows.
  function updateFoo() {
    var rows = Model.updateRows(service.lists, "retail", []).filter(function(r) { return r.name === "Foo" })
    compare(rows.length, 1, "Foo's row on retail")
    var action = Model.rowAction(rows[0], "update")
    compare(service.run(action.template, action.slots, action.label, action.shown), "")
    var preview = lastRun("update")
    verify(preview.command.indexOf("--dry-run") !== -1, "the update's preview: " + launchLog())
    return preview
  }

  // ---- the routine rule

  function test_a_routine_matching_update_applies_with_no_dialog() {
    showList("plan-list")
    answer(updateFoo(), "plan-update-routine")
    compare(service.confirm, null, "no dialog for a routine plan the row showed")
    var apply = lastRun("apply")
    verify(apply !== null, "the routine preview's apply: " + launchLog())
    compare(words(apply.command), ["apply", planIdOf("plan-update-routine")], "the apply of the routine preview")
    verify(apply.command.indexOf("--dry-run") === -1 && apply.command.indexOf("--if-match") === -1)
    var statuses = runs("status").length
    answer(apply, "plan-update-routine-apply")
    compare(service.lastResult.line, "Updated 1 addon")
    compare(runs("status").length, statuses + 1, "the result refreshes the bar")
  }

  function test_a_matching_update_that_adds_a_dependency_opens_the_dialog_naming_it() {
    showList("plan-dependency-list")
    answer(updateFoo(), "plan-update-dependency")
    compare(runs("apply").length, 0, "a plan that adds a dependency asks first: " + launchLog())
    // The update is the item; the dependency it adds is an effect, named with its version.
    compare(sheet().items.map(function(i) { return i.text }), ["Foo v1 → v2"])
    compare(effects(), ["Also installs LibFoo 1.0, which Foo needs."])
    compare([sheet().title.indexOf("Update Foo on ") === 0, sheet().confirmLabel, sheet().destructive], [true, "Update Foo", false])
    compare(service.confirm.heading, "")
    compare(service.confirmYes(), "")
    compare(words(lastRun("apply").command), ["apply", planIdOf("plan-update-dependency")], "Yes applies the plan")
  }

  // An install from Search or an addon page (1n2p.25): the view shows no release, so the
  // sheet always opens, and it names the dependency the install adds.
  function test_an_install_opens_the_dialog_naming_the_dependency_it_adds() {
    showList("plan-dependency-list")
    compare(service.run("install", { "target": "retail", "ref": "gh:o/bar" }, "Install Bar", []), "")
    var preview = lastRun("install")
    verify(preview.command.indexOf("--dry-run") !== -1, "the install's preview: " + launchLog())
    compare(words(preview.command), ["install", "-t", "retail", "--", "gh:o/bar"])
    answer(preview, "plan-install-dependency")
    compare(runs("apply").length, 0, "nothing runs before Yes: " + launchLog())
    compare(effects(), ["Also installs LibFoo 1.0, which Bar needs."])
    compare([sheet().title.indexOf("Install Bar on ") === 0, sheet().confirmLabel, sheet().destructive], [true, "Install Bar", false])
    compare(service.confirmYes(), "")
    compare(words(lastRun("apply").command), ["apply", planIdOf("plan-install-dependency")], "Yes applies the plan")
  }

  // omad-3rre: Search's "Every client it works on". Each client's preview in turn (the
  // binary's, scene F), one sheet, nothing before Yes; then each client's own plan.
  function installTwo() {
    compare(service.installEach("gh:o/qux", "Qux", ["retail", "era"]), "")
    var first = lastRun("install")
    verify(first.command.indexOf("--dry-run") !== -1, "a preview: " + launchLog())
    compare(words(first.command), ["install", "-t", "retail", "--", "gh:o/qux"])
    answer(first, "plan-install-two-retail")
    compare(service.confirm, null, "no sheet before every client's preview answered")
    var second = lastRun("install")
    verify(second !== first, "the next client's preview: " + launchLog())
    compare(words(second.command), ["install", "-t", "era", "--", "gh:o/qux"])
    answer(second, "plan-install-two-era")
    compare(runs("apply").length, 0, "nothing runs before Yes: " + launchLog())
    compare([sheet().title, sheet().confirmLabel], ["Install Qux on retail and era?", "Install on 2 clients"])
    compare(sheet().items.map(function(i) { return i.header }), ["retail", "era"])
    compare(service.confirmYes(), "")
  }

  function test_an_install_on_two_clients_applies_each_clients_plan_in_turn() {
    installTwo()
    compare(words(lastRun("apply").command), ["apply", planIdOf("plan-install-two-retail")])
    answer(lastRun("apply"), "plan-install-two-retail-apply")
    compare(words(lastRun("apply").command), ["apply", planIdOf("plan-install-two-era")], "the next plan, once the first committed")
    answer(lastRun("apply"), "plan-install-two-era-apply")
    compare(runs("apply").length, 2)
    compare(service.lastResult.line, "Installed Qux on retail and era · Undo takes back era's install; History has each")
    verify(service.lastResult.ok)
  }

  function test_an_install_on_two_clients_stops_at_the_first_apply_that_does_not_commit() {
    installTwo()
    answer(lastRun("apply"), "plan-apply-not-found", 3)
    compare(runs("apply").length, 1, "the rest never runs: " + launchLog())
    verify(!service.lastResult.ok)
    var line = service.lastResult.line
    verify(line.indexOf("retail failed: ") === 0 && line.indexOf("; not run on era") > 0, line)
  }

  // A plan gone stale on one client: that client alone is previewed again.
  function test_an_install_on_two_clients_whose_plan_went_stale_previews_that_client_again() {
    installTwo()
    var installs = runs("install").length
    answer(lastRun("apply"), "plan-apply-stale", 1)
    compare(runs("install").length, installs + 1, "one preview again: " + launchLog())
    compare(words(lastRun("install").command), ["install", "-t", "retail", "--", "gh:o/qux"])
    compare(runs("apply").length, 1, "era's plan is not applied")
    compare(service.lastResult.line, "retail failed: changed since you looked, so it is shown again; not run on era")
  }

  function test_a_moved_release_opens_the_dialog_with_changed_since_you_looked() {
    showList("plan-moved-list")
    answer(updateFoo(), "plan-update-moved")
    compare(sheet().stale, "Changed since you looked: Foo v3 is now the newest", "the banner over the new item")
    compare(sheet().items.map(function(i) { return i.text }), ["Foo v1 → v3"])
    compare(runs("apply").length, 0, "nothing runs before Yes")
    compare(service.confirmYes(), "")
    compare(words(lastRun("apply").command), ["apply", planIdOf("plan-update-moved")])
  }

  function test_a_matching_update_that_replaces_local_edits_opens_the_dialog() {
    showList("plan-local-edits-list")
    answer(updateFoo(), "plan-update-local-edits")
    compare(runs("apply").length, 0, launchLog())
    compare(effects(), ["Replaces 1 file you changed in Foo. Undo brings it back."])
    compare(sheet().effects[0].severity, "urgent")
  }

  function test_a_matching_update_with_extra_removals_opens_the_dialog() {
    showList("plan-removal-list")
    answer(updateFoo(), "plan-update-removal")
    compare(runs("apply").length, 0, launchLog())
    compare(effects(), ["Removes the folder Foo_Options. Undo brings it back."])
  }

  function test_equal_versions_from_another_asset_open_the_dialog() {
    showList("plan-asset-list")
    answer(updateFoo(), "plan-update-asset")
    compare(runs("apply").length, 0, launchLog())
    compare(service.confirm.heading, "Changed since you looked: Foo v2 comes in another file now (Foo-retail.zip)")
  }

  // ---- a refused apply previews again, never applies again

  // The moved release's dialog, confirmed: the apply launch.
  function confirmedMove() {
    showList("plan-moved-list")
    answer(updateFoo(), "plan-update-moved")
    compare(service.confirmYes(), "")
    var apply = lastRun("apply")
    verify(apply !== null, "Yes applies: " + launchLog())
    return apply
  }

  function test_a_stale_apply_previews_again_and_never_reruns_apply() {
    answer(confirmedMove(), "plan-apply-stale", 1)
    var again = lastRun("update")
    compare(runs("update").length, 2, "the update is previewed again: " + launchLog())
    verify(again.command.indexOf("--dry-run") !== -1)
    answer(again, "plan-update-v4")
    verify(service.confirm.stale.indexOf("Changed since you looked") === 0, service.confirm.stale)
    // The sheet's banner says what moved; Yes applies only the new plan.
    verify(sheet().stale.indexOf("Changed since you looked: ") === 0, sheet().stale)
    compare(runs("apply").length, 1, "exactly one apply so far: " + launchLog())
    compare(service.confirmYes(), "")
    compare(words(lastRun("apply").command), ["apply", planIdOf("plan-update-v4")], "Yes applies the new plan: " + launchLog())
  }

  function test_an_expired_plan_previews_again() {
    answer(confirmedMove(), "plan-apply-expired", 1)
    compare(runs("update").length, 2, "the update is previewed again: " + launchLog())
    answer(lastRun("update"), "plan-update-v4")
    compare(service.confirm.stale, "Changed since you looked: that was more than a day ago. Nothing ran; here is what would happen now.")
    compare(runs("apply").length, 1, launchLog())
  }

  function test_a_missing_plan_previews_again() {
    answer(confirmedMove(), "plan-apply-not-found", 3)
    compare(runs("update").length, 2, "the update is previewed again: " + launchLog())
    answer(lastRun("update"), "plan-update-v4")
    compare(service.confirm.stale, "Changed since you looked: what you saw was cleared. Nothing ran; here is what would happen now.")
    compare(runs("apply").length, 1, launchLog())
  }

  // ---- nothing to do, Update all, a removal, Undo last

  function test_a_no_op_preview_refreshes_and_runs_nothing() {
    showList("plan-list-after")
    compare(service.run("update_addon", { "target": "retail", "addon": "Foo" }, "Update Foo", []), "")
    var statuses = runs("status").length
    answer(lastRun("update"), "plan-update-noop")
    compare(runs("apply").length, 0, launchLog())
    compare(service.confirm, null)
    compare(runs("status").length, statuses + 1, "the view is refreshed")
    compare(service.lastResult.line, "Already up to date; the view is refreshed")
  }

  function test_update_all_applies_when_every_row_matches() {
    showList("plan-list-after")
    var request = Model.updateRequest("", Model.updateRows(service.lists, "", []))
    compare(request.shown.length, 3, "Update all shows three updates")
    compare(service.run(request.template, request.slots, request.label, request.shown), "")
    answer(lastRun("update"), "plan-update-all")
    compare(service.confirm, null, "no dialog: every row matches")
    var apply = lastRun("apply")
    compare(words(apply.command), ["apply", planIdOf("plan-update-all")], launchLog())
    answer(apply, "plan-update-all-apply")
    compare(service.lastResult.line, "Updated 3 addons on 2 clients")
  }

  function test_a_removal_confirms_and_runs_apply_of_its_preview() {
    compare(service.run("remove", { "target": "retail", "addon": "Bar" }, "Remove Bar"), "")
    answer(lastRun("remove"), "plan-remove")
    verify(service.confirm !== null, "the dialog opens")
    // The removal by name, its version and folders, and the verb urgent.
    compare([sheet().title, sheet().items.map(function(i) { return i.text }), sheet().confirmLabel, sheet().destructive],
      ["Remove Bar from retail?", ["Bar v1 (1 folder)"], "Remove Bar", true])
    compare(runs("apply").length, 0, "nothing runs before Yes")
    compare(service.confirmYes(), "")
    var apply = lastRun("apply")
    compare(words(apply.command), ["apply", planIdOf("plan-remove")], launchLog())
    answer(apply, "plan-remove-apply")
    compare(service.lastResult.line, "Removed 1 addon")
  }

  function test_undo_last_confirms_through_apply() {
    var undo = JSON.parse(Fixtures.text("plan-undo-apply")).data.applied.display_argv
    compare(service.runCliArgv(undo, "Undo"), "")
    var preview = lastRun("undo")
    verify(preview.command.indexOf("--dry-run") !== -1, "Undo last previews: " + launchLog())
    answer(preview, "plan-undo")
    verify(sheet().title.indexOf("Undo the update on ") === 0, sheet().title)
    compare([sheet().items.map(function(i) { return i.text }), sheet().confirmLabel, sheet().destructive],
      [["Bar back as it was", "Foo back as it was"], "Undo", true])
    compare(service.confirmYes(), "")
    var apply = lastRun("apply")
    compare(words(apply.command), ["apply", planIdOf("plan-undo")], launchLog())
    answer(apply, "plan-undo-apply")
    verify(service.lastResult.line.indexOf("Undid update on") === 0, service.lastResult.line)
  }
}
