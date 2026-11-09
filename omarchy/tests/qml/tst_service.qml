import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "../../Store.js" as Store

// Service.qml against the mock Process (pipeline step 4): what runs, under which
// environment, in which lane and order, what an answer does to the model, and how a
// child is stopped. Timings are shrunk; Build.js is the committed fixture.
TestCase {
  id: testCase
  name: "Service"

  property var service: null

  Component {
    id: serviceComponent
    Service {}
  }

  // Fixture templates over commands that exist, so the lanes are tested before F8 and
  // F9 add `backup_auto` and `restore`.
  readonly property var fixtureTemplates: ({
    "pin_silent": { "argv": ["pin", "-t", "{target}", "--", "{addon}"], "mutates": true, "silent": true },
    "remove_confirm": { "argv": ["remove", "-t", "{target}", "--", "{addon}"], "mutates": true, "preview": "confirm" }
  })

  readonly property var standardFlags: ["--json", "--actor", "widget", "--reason"]

  function make(extra) {
    if (service) service.destroy()
    service = null
    wait(0)
    ProcessRegistry.reset()
    Quickshell.resetDetachedCommands()
    var props = { "bellDebounceMs": 10, "localDeadlineMs": 400, "networkDeadlineMs": 400, "killGraceMs": 60, "streamGraceMs": 60 }
    if (extra) for (var key in extra) props[key] = extra[key]
    service = serviceComponent.createObject(testCase, props)
    verify(service !== null)
  }

  function init() {
    make()
  }

  function cleanup() {
    if (service) service.destroy()
    service = null
    ProcessRegistry.reset()
    wait(0)
  }

  // ---- helpers

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

  // omaddons launches (any binary: they start with the standard flags), optionally only
  // those whose command words start with `word`.
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

  function envelope(command, data, extra) {
    var e = { "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": command,
      "exit_code": 0, "partial": false, "data": data, "warnings": [], "commands": [] }
    if (extra) for (var key in extra) e[key] = extra[key]
    return JSON.stringify(e)
  }

  function statusEnvelope(available, extra) {
    return envelope("status", {
      "cached": true,
      "targets": [{ "id": "retail", "label": "retail (battlenet)", "running": false, "updates": { "available": available }, "issues": [] }],
      "summary": { "targets": 1, "running": [], "updates": { "available": available, "targets": available ? ["retail"] : [] },
        "issues": { "p0": 0, "p1": 0, "p2": 0, "p3": 0 }, "errors": { "new": 0, "targets": [] },
        "check_due": false, "widget_outdated": false, "state": available ? "updates" : "idle" },
      "last_change": null, "last_change_state": "ok"
    }, extra)
  }

  function failure(command, code, message, details) {
    return JSON.stringify({ "ok": false, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": command,
      "exit_code": 1, "error": { "code": code, "message": message, "fix": "omaddons list", "fix_argv": ["omaddons", "list"],
        "alternatives": [], "retryable": false, "details": details || {} }, "warnings": [], "commands": [] })
  }

  function answer(launch, text, order) {
    verify(launch !== null, "a launch to answer")
    launch.process.finish(0, text, order)
  }

  // ---- stored plans (M4-D28)

  readonly property string planId: "01M8Q2ZZZZZZZZZZZZZZZZZZZZ"

  function merged(base, extra) {
    var out = {}
    for (var key in base) out[key] = base[key]
    if (extra) for (var k in extra) out[k] = extra[k]
    return out
  }

  // An approval's selection item as the CLI writes it: Foo v1 → v2 on retail.
  function planItem(fields) {
    return merged({ "target_key": "01M8Q2RRRRRRRRRRRRRRRRRRRR", "target_id": "retail", "addon": "Foo", "source": "github",
      "channel": "stable", "release_id": "gh:o/foo@v2", "asset": "Foo.zip", "from_version": "v1", "to_version": "v2",
      "op": "update", "fields": [], "tx": null, "backup": null }, fields)
  }

  // The same item as a row showed it (Model.shownOf's fields).
  function shownItem(fields) {
    var out = planItem(fields)
    delete out.op
    delete out.fields
    delete out.tx
    delete out.backup
    return out
  }

  function scope(fields) {
    return merged({ "dependencies_added": [], "folders_removed": [], "local_changes_replaced": { "count": 0, "folders": [] },
      "source_changed": [], "while_running": false }, fields)
  }

  // A storable dry run that stored plan `id` (planId), offering its apply.
  function planPreview(command, selection, approval, data, extra) {
    var id = extra && extra.id ? extra.id : planId
    var offerId = extra && extra.offerId ? extra.offerId : id
    var body = merged({ "dry_run": true, "plan_id": id, "plan_expires_at": "2100-01-02T00:00:00Z", "precondition": "pc1:" + "e".repeat(64),
      "approval": merged({ "routine": true, "reasons": [], "selection": selection, "scope": scope() }, approval) }, data)
    return envelope(command, body, { "commands": [{ "action": "apply", "command": "omaddons apply " + offerId,
      "argv": ["omaddons", "apply", offerId], "destructive": !!(extra && extra.destructive) }] })
  }

  // A dry run with nothing to do: no plan, an empty selection, no offer.
  function noopPreview(command) {
    return envelope(command, { "dry_run": true, "plan_id": null, "plan_expires_at": null, "precondition": "pc1:" + "f".repeat(64),
      "approval": { "routine": true, "reasons": [], "selection": [], "scope": scope() } })
  }

  // `apply`'s envelope around the executed command's data.
  function applyEnvelope(command, result, outcomes, extra) {
    return envelope("apply", { "plan_id": planId, "applied": { "command": command, "display_argv": ["omaddons", command] },
      "result": result, "outcomes": outcomes || [] }, extra)
  }

  function removePreview(version) {
    return planPreview("remove", [planItem({ "op": "remove", "addon": "Baz", "from_version": version, "to_version": null, "release_id": null, "asset": null })],
      { "routine": false, "reasons": ["folders_removed", "destructive"], "scope": scope({ "folders_removed": ["Baz"] }) },
      { "target": { "id": "retail" }, "items": [{ "name": "Baz", "version": version, "folders": ["Baz"] }], "reload_required": "none" },
      { "destructive": true })
  }

  // Every omaddons launch so far, by its words, for a failure's log.
  function launchLog() {
    return JSON.stringify(runs().map(function(l) { return words(l.command) }))
  }

  // The open plan sheet, a line each (Model.dialogText, as IPC's state() carries it).
  function sheetLines() {
    verify(service.confirm !== null && !!service.confirm.sheet, "a plan sheet is open")
    return Model.dialogText(service.confirm.sheet).split("\n")
  }

  // Previews `update_addon` (Foo on retail) with what the row showed; returns the launch.
  function previewFoo(shown) {
    compare(service.run("update_addon", { "target": "retail", "addon": "Foo" }, "Update Foo", shown === undefined ? [shownItem()] : shown), "")
    var launch = lastRun("update")
    verify(launch.command.indexOf("--dry-run") !== -1, "the update's preview: " + launchLog())
    return launch
  }

  // Answers the startup status with `available` updates, then the catch-up checkpoint it
  // queues for the stopped client (F8) and the refresh that change rings: the service at
  // rest.
  function ready(available) {
    answer(lastRun("status"), statusEnvelope(available || 0))
    compare(service.phase, "ok")
    var auto = lastRun("backup")
    compare(words(auto.command), ["backup", "create", "--auto", "-t", "retail"])
    answer(auto, envelope("backup create", { "targets": [] }))
    answer(lastRun("status"), statusEnvelope(available || 0))
  }

  function bell() {
    for (var i = 0; i < ProcessRegistry.launches.length; i++) {
      if (ProcessRegistry.launches[i].command[0] === "inotifywait") return ProcessRegistry.launches[i]
    }
    return null
  }

  // ---- startup and freshness

  function test_startup_runs_status_under_the_stamped_context() {
    var launch = lastRun("status")
    compare(launch.command, [Build.BINARY, "--json", "--actor", "widget", "--reason", "Refresh (bar widget)", "status", "--cached", "-v"])
    compare(launch.environment, Build.CONTEXT)
    var watcher = bell()
    compare(watcher.command, ["inotifywait", "-m", "-q", "-e", "moved_to", "--format", "%f", Build.CACHE_DIR])
    // A desktop helper keeps the desktop's own environment.
    compare(watcher.environment, {})
    compare(service.ipcTarget, Build.PLUGIN_ID)
    ready(3)
    compare(service.bar.badge, 3)
  }

  function test_a_doorbell_burst_is_one_status() {
    ready(0)
    var before = runs("status").length
    for (var i = 0; i < 5; i++) bell().process.emitStdout("doorbell.json")
    bell().process.emitStdout("discovery.json")
    wait(50)
    compare(runs("status").length, before + 1)
    bell().process.emitStdout("discovery.json")
    wait(50)
    compare(runs("status").length, before + 1)
  }

  function test_a_missing_directory_retries_the_watcher() {
    make({ "bellMissingRetryMs": 30, "bellRetryMs": 30 })
    var first = bell()
    first.process.exit(1, 0)
    wait(80)
    var watchers = 0
    for (var i = 0; i < ProcessRegistry.launches.length; i++) if (ProcessRegistry.launches[i].command[0] === "inotifywait") watchers++
    verify(watchers >= 2, "the watcher started again")
  }

  function test_a_missing_binary_is_phase_missing() {
    ready(0)
    ProcessRegistry.missingBinary = true
    service.refresh()
    wait(20)
    compare(service.phase, "missing")
    verify(service.bar.dimmed)
  }

  function test_a_malformed_status_is_unreachable_with_the_stderr_tail() {
    var launch = lastRun("status")
    launch.process.emitStderr("thread 'main' panicked at src/main.rs:1")
    launch.process.finish(101, "")
    compare(service.phase, "unreachable")
    verify(service.unreachableText.indexOf("panicked") !== -1)
    compare(service.outcomes.status, "malformed")
  }

  // A failed status says why (here a broken config.toml), never "no clients found";
  // the next status that answers takes the card away.
  function test_a_failed_status_shows_its_error_until_one_answers() {
    answer(lastRun("status"), failure("status", "state_corrupt", "config.toml does not parse"))
    compare(service.phase, "ok")
    compare(service.summary, null)
    verify(service.lastError !== null)
    compare(service.lastError.message, "config.toml does not parse")
    service.refresh()
    answer(lastRun("status"), statusEnvelope(0))
    compare(service.lastError, null)
  }

  function test_an_oversized_stdout_is_discarded() {
    var launch = lastRun("status")
    launch.process.finish(0, "x".repeat(Model.WIDGET_OUTPUT_MAX_BYTES + 1))
    compare(service.outcomes.status, "oversized")
    compare(service.phase, "unreachable")
  }

  // Quickshell reports `running` false after `exited`, by which time the lane may have
  // started its next request (seen live, not in the mock's own order).
  function test_a_finished_process_reporting_late_never_ends_the_next_request() {
    ready(0)
    service.readList()
    var list = lastRun("list").process
    service.refresh()
    list.finish(0, envelope("list", { "targets": [] }), "streamFirst")
    var status = lastRun("status").process
    verify(status.running)
    list.runningChanged()
    list.exited(0, 0)
    list.stdout.streamFinished()
    compare(service.phase, "ok")
    verify(status.running)
    status.finish(0, statusEnvelope(7))
    compare(service.bar.badge, 7)
  }

  function test_completion_waits_for_exit_and_stream_in_either_order() {
    var spy = Qt.createQmlObject('import QtTest; SignalSpy { signalName: "requestFinished" }', testCase)
    spy.target = service
    answer(lastRun("status"), statusEnvelope(1), "streamFirst")
    compare(spy.count, 1)
    service.refresh()
    answer(lastRun("status"), statusEnvelope(2))
    compare(spy.count, 2)
    compare(service.bar.badge, 2)
    // A stream that never ends (a child holding the pipe) completes after the grace.
    service.refresh()
    lastRun("status").process.exit(0, 0)
    compare(spy.count, 2)
    wait(120)
    compare(spy.count, 3)
    spy.destroy()
  }

  // ---- the compatibility gate (M3-D42)

  function test_an_incompatible_binary_makes_the_widget_read_only() {
    service.templates = Object.assign({}, Model.COMMANDS, fixtureTemplates)
    ready(2)
    compare(service.run("remove", { "target": "retail", "addon": "Baz" }, "Remove"), "")
    answer(lastRun("remove"), removePreview("v1"))
    verify(service.confirm !== null)
    compare(service.run("update_all", {}, "Update all"), "")
    compare(service.run("update_target", { "target": "retail" }, "Update retail"), "")
    compare(service.queuedAction, "Update retail")
    service.refresh()
    answer(lastRun("status"), statusEnvelope(2, { "tool_version": "9.9.9" }))
    compare(service.phase, "incompatible")
    verify(service.incompatibleText.indexOf("9.9.9") !== -1)
    compare(service.confirm, null)
    compare(service.queues.mutation.length, 0)
    compare(service.queuedAction, "")
    verify(service.bar.stale)
    verify(service.run("update_all", {}, "Update all") !== "")
    verify(service.confirmYes() !== "")
    // Retry and Reinstall stay.
    verify(service.refresh())
    compare(service.reinstallWidget(), "ok")
    lastRun("update").process.finish(0, envelope("update", { "targets": [], "summary": {} }))
    compare(runs("update").length, 1)
  }

  function test_a_confirmation_from_another_context_never_runs() {
    ready(0)
    service.run("remove", { "target": "retail", "addon": "Baz" }, "Remove")
    answer(lastRun("remove"), removePreview("v1"))
    verify(service.confirm !== null)
    // A new binary is a new context; the confirmation was made in the old one.
    service.applySettings({ "binary": "/opt/other/omaddons" })
    compare(service.contextGeneration, 1)
    answer(lastRun("status"), statusEnvelope(0))
    // The new binary's catch-up checkpoint (F8) holds the lane until it is done.
    answer(lastRun("backup"), envelope("backup create", { "targets": [] }))
    compare(service.confirmYes(), "")
    wait(20)
    compare(runs("remove").length, 1)
    compare(runs("apply").length, 0, "the old context's plan never runs: " + launchLog())
    verify(service.lastError !== null && service.lastError.code === "incompatible")
  }

  // ---- confirmations (M3-D9, M3-D46, M4-D28)

  function test_a_confirmed_removal_runs_its_previews_plan_and_a_stale_one_previews_again() {
    ready(0)
    compare(service.run("remove", { "target": "retail", "addon": "Baz" }, "Remove Baz"), "")
    var preview = lastRun("remove")
    verify(preview.command.indexOf("--dry-run") !== -1)
    verify(preview.command.indexOf("--if-match") === -1)
    compare(service.action.message, "Checking what it would do…")
    answer(preview, removePreview("v1"))
    compare(sheetLines(), ["Remove Baz from retail?", "Baz v1 (1 folder)", "Removes the folder Baz. Undo brings it back."])
    compare([service.confirm.sheet.confirmLabel, service.confirm.sheet.destructive], ["Remove Baz", true])
    compare(runs("apply").length, 0, "a removal asks first: " + launchLog())
    compare(service.confirmYes(), "")
    var real = lastRun("apply")
    verify(real !== null, "Yes applies the preview's plan: " + launchLog())
    compare(words(real.command), ["apply", planId], "the apply of the confirmed preview")
    compare(real.command.slice(6, 8), ["--progress", "json"])
    verify(real.command.indexOf("--dry-run") === -1)
    verify(real.command.indexOf("--if-match") === -1)
    answer(real, failure("apply", "plan_stale", "plan " + planId + " is no longer what its preview showed", { "changed": ["selection"] }))
    var again = lastRun("remove")
    verify(again !== preview, "the stale plan previews again: " + launchLog())
    verify(again.command.indexOf("--dry-run") !== -1)
    answer(again, removePreview("v2"))
    var banner = "Changed since you looked: a newer version came out, or the change is different now. Nothing ran; here is what would happen now."
    compare(service.confirm.stale, banner)
    compare(sheetLines().slice(0, 3), ["Remove Baz from retail?", banner, "Baz v2 (1 folder)"])
    compare(runs("remove").length, 2, launchLog())
    compare(runs("apply").length, 1, "apply never runs twice for one preview: " + launchLog())
  }

  // ---- the plan runner (M4-D28)

  function test_a_match_preview_runs_under_the_network_deadline_and_apply_under_none() {
    make({ "networkDeadlineMs": 400 })
    ready(0)
    var silent = previewFoo()
    tryVerify(function() { return service.outcomes.update_addon === "timeout" }, 1500, "a silent match preview times out")
    verify(String(service.lastError.message).slice(-7) === "timeout", service.lastError.message)
    compare(runs("apply").length, 0)
    // Progress keeps a preview alive past its deadline.
    // A preview started right after the timeout runs on (the timed-out child's kill timer
    // never reaches it).
    var busy = previewFoo()
    verify(busy !== silent)
    for (var i = 0; i < 4; i++) {
      wait(200)
      busy.process.emitStderr(JSON.stringify({ "event": "progress", "phase": "fetch", "message": "gh:o/foo", "current": i, "total": 4 }))
    }
    compare(busy.process.signals, [], "a preview with progress outlives 600 ms")
    answer(busy, planPreview("update", [planItem()]))
    var apply = lastRun("apply")
    verify(apply !== null, "the routine preview's apply: " + launchLog())
    // An apply is never killed for time.
    wait(600)
    compare(apply.process.signals, [], "an apply runs under no deadline")
    answer(apply, applyEnvelope("update", { "targets": [], "summary": {} }))
  }

  function test_a_planned_apply_goes_first_and_replaces_no_queued_change() {
    ready(0)
    var preview = previewFoo()
    compare(service.run("update_target", { "target": "era" }, "Update era", []), "")
    compare(service.queuedAction, "Update era")
    answer(preview, planPreview("update", [planItem()]))
    var apply = lastRun("apply")
    verify(apply !== null, "the apply goes first: " + launchLog())
    compare(words(ProcessRegistry.launches[ProcessRegistry.launches.length - 1].command), ["apply", planId], launchLog())
    compare(service.queues.mutation.length, 1, "the queued change stays")
    compare(service.queues.mutation[0].label, "Update era")
    answer(apply, applyEnvelope("update", { "targets": [], "summary": {} }))
    var next = lastRun("update")
    verify(next !== preview, "the queued preview starts: " + launchLog())
    compare(words(next.command), ["update", "-t", "era"])
    verify(next.command.indexOf("--dry-run") !== -1)
  }

  function test_an_apply_offer_for_another_plan_runs_nothing() {
    ready(0)
    answer(previewFoo(), planPreview("update", [planItem()], null, null, { "offerId": "01M8Q2YYYYYYYYYYYYYYYYYYYY" }))
    compare(runs("apply").length, 0, launchLog())
    compare(service.confirm, null)
    compare(service.lastError.message, "The preview offered no plan to apply; nothing ran")
    compare(service.lastError.code, "not_previewed")
  }

  function test_a_preview_without_an_approval_runs_nothing() {
    ready(0)
    answer(previewFoo(), envelope("update", { "dry_run": true, "plan_id": planId, "plan_expires_at": "2100-01-02T00:00:00Z" },
      { "commands": [{ "action": "apply", "command": "omaddons apply " + planId, "argv": ["omaddons", "apply", planId], "destructive": false }] }))
    compare(runs("apply").length, 0, launchLog())
    compare(service.lastError.message, "The preview did not say what it would change; nothing ran")
  }

  function test_a_full_plan_store_runs_nothing_and_says_so() {
    ready(0)
    var full = JSON.parse(planPreview("update", [planItem()], null, { "plan_id": null, "plan_expires_at": null }))
    full.commands = []
    full.warnings = [{ "code": "plan_not_stored", "message": "the plan store is full", "fix": "previews are kept again as plans expire", "fix_argv": null }]
    answer(previewFoo(), JSON.stringify(full))
    compare(runs("apply").length, 0, launchLog())
    compare(service.lastError.message, "The preview could not be kept as a plan; nothing ran")
    compare(service.lastError.fixText, "previews are kept again as plans expire")
  }

  function test_a_no_op_preview_refreshes_and_runs_nothing() {
    ready(0)
    var statuses = runs("status").length
    answer(previewFoo(), noopPreview("update"))
    compare(runs("apply").length, 0, launchLog())
    compare(service.confirm, null)
    compare(service.lastResult.line, "Already up to date; the view is refreshed")
    compare(runs("status").length, statuses + 1, "the view is refreshed")
  }

  function test_a_matching_preview_that_adds_a_dependency_asks_first() {
    ready(0)
    var lib = planItem({ "addon": "LibFoo", "op": "install", "from_version": null, "to_version": "1.0", "release_id": "gh:o/libfoo@1.0", "asset": "LibFoo.zip" })
    answer(previewFoo(), planPreview("update", [planItem(), lib],
      { "routine": false, "reasons": ["dependency_added"], "scope": scope({ "dependencies_added": ["LibFoo"] }) }))
    compare(runs("apply").length, 0, "a plan that does more asks first: " + launchLog())
    compare(service.confirm.heading, "")
    compare(sheetLines(), ["Update Foo on retail?", "Foo v1 → v2", "Also installs LibFoo 1.0, which Foo needs."])
    compare(service.confirm.sheet.confirmLabel, "Update Foo")
    compare(service.confirmYes(), "")
    compare(words(lastRun("apply").command), ["apply", planId], "Yes applies the plan")
  }

  function test_a_moved_release_asks_first_saying_what_changed() {
    ready(0)
    answer(previewFoo(), planPreview("update", [planItem({ "to_version": "v3", "release_id": "gh:o/foo@v3", "asset": "Foo-v3.zip" })]))
    compare(runs("apply").length, 0, launchLog())
    compare(service.confirm.heading, "Changed since you looked: Foo v3 is now the newest")
    compare(sheetLines(), ["Update Foo on retail?", "Changed since you looked: Foo v3 is now the newest", "Foo v1 → v3"])
  }

  function test_a_stale_apply_previews_again_and_never_applies_again() {
    ready(0)
    answer(previewFoo(), planPreview("update", [planItem()]))
    var apply = lastRun("apply")
    answer(apply, failure("apply", "plan_stale", "plan " + planId + " expired", { "reason": "expired" }))
    var again = lastRun("update")
    verify(again.command.indexOf("--dry-run") !== -1, "the update is previewed again: " + launchLog())
    // Previewed again after a refused plan: even a routine match asks first.
    answer(again, planPreview("update", [planItem()]))
    compare(runs("apply").length, 1, launchLog())
    var banner = "Changed since you looked: that was more than a day ago. Nothing ran; here is what would happen now."
    compare(service.confirm.stale, banner)
    compare(sheetLines(), ["Update Foo on retail?", banner, "Foo v1 → v2"])
  }

  function test_a_stale_client_in_an_applied_plan_previews_again() {
    ready(0)
    answer(previewFoo(), planPreview("update", [planItem()]))
    answer(lastRun("apply"), applyEnvelope("update", { "targets": [], "summary": {} },
      [{ "target": "retail", "key": "01M8Q2RRRRRRRRRRRRRRRRRRRR", "status": "stale", "tx": null, "error_code": "plan_stale" }], { "partial": true }))
    compare(runs("update").length, 2, "the update is previewed again: " + launchLog())
  }

  function test_locked_apply_shows_the_error_and_does_not_preview_again() {
    ready(0)
    answer(previewFoo(), planPreview("update", [planItem()]))
    var locked = JSON.parse(failure("apply", "locked", "another apply holds plan " + planId, { "lease": planId }))
    locked.exit_code = 5
    lastRun("apply").process.finish(5, JSON.stringify(locked))
    compare(service.lastError.code, "locked")
    compare(runs("update").length, 1, "no preview again: " + launchLog())
    compare(service.confirm, null)
  }

  // `targets rm` binds (omad-launch-week-fixes-bmcx.2): Yes forgets exactly the client its
  // preview named, by the preview's token, and stores no plan.
  function test_targets_rm_binds_to_its_preview() {
    ready(0)
    var token = "pc1:" + "4".repeat(64)
    compare(service.runCliArgv(["omaddons", "targets", "rm", "retail"], "Forget retail"), "")
    var preview = lastRun("targets")
    verify(preview.command.indexOf("--dry-run") !== -1)
    answer(preview, envelope("targets rm", { "id": "retail", "managed_addons": 0, "was_default": false, "dry_run": true, "precondition": token }))
    compare(sheetLines(), ["Forget retail?", "Nothing in its folder is touched."])
    compare([service.confirm.sheet.confirmLabel, service.confirm.sheet.destructive], ["Forget retail", true])
    compare(service.confirmYes(), "")
    var real = lastRun("targets")
    verify(real !== preview)
    compare(words(real.command), ["targets", "rm", "retail"])
    verify(real.command.indexOf("--dry-run") === -1)
    compare(real.command[real.command.indexOf("--if-match") + 1], token)
    compare(runs("apply").length, 0)
  }

  function test_a_dismissal_keeps_its_read_bound_token() {
    ready(0)
    var token = "pc1:" + "3".repeat(64)
    compare(service.dismissErrors([{ "target": "retail", "token": token }]), "")
    var launch = lastRun("errors")
    compare(words(launch.command), ["errors", "dismiss", "-t", "retail"])
    compare(launch.command[launch.command.indexOf("--if-match") + 1], token)
    verify(launch.command.indexOf("--dry-run") === -1, "no preview")
    compare(runs("apply").length, 0)
  }

  function test_state_reports_the_last_preview() {
    ready(0)
    answer(previewFoo(), planPreview("update", [planItem()]))
    var state = JSON.parse(service.stateJson())
    compare(state.lastPreview, { "resource": "update_addon", "decision": "direct", "planId": planId })
    compare(state.confirm, null)
    answer(lastRun("apply"), applyEnvelope("update", { "targets": [], "summary": {} }))
    answer(previewFoo(), planPreview("update", [planItem({ "to_version": "v3" })]))
    state = JSON.parse(service.stateJson())
    compare(state.lastPreview.decision, "dialog")
    // The open plan sheet: its heading and plan as before, its title, and itself as text.
    compare(state.confirm, { "heading": "Changed since you looked: Foo v3 is now the newest", "planId": planId,
      "title": "Update Foo on retail?", "text": "Update Foo on retail?\nChanged since you looked: Foo v3 is now the newest\nFoo v1 → v3" })
  }

  function test_a_destructive_cli_argv_previews_first() {
    ready(0)
    compare(service.runCliArgv(["omaddons", "undo", "--run", "01M5JHFF8X0FXBT1K068DQKCHG"], "Undo"), "")
    var preview = lastRun("undo")
    verify(preview.command.indexOf("--dry-run") !== -1)
    verify(service.runCliArgv(["omaddons", "--actor", "agent", "update"], "x").indexOf("widget_flag") !== -1)
    // Undo last asks first, and Yes applies the preview's plan (M4-D28).
    answer(preview, planPreview("undo", [planItem({ "op": "undo", "addon": null, "source": null, "release_id": null, "asset": null,
      "from_version": null, "to_version": null })], { "routine": false, "reasons": ["destructive"] },
      { "reverted": { "kind": "update" }, "transactions": [{ "target": { "id": "retail" } }], "folders": { "restored": ["Foo"], "removed": [] } },
      { "destructive": true }))
    compare(sheetLines(), ["Undo the update on retail from an earlier time?", "Foo back as it was"])
    compare([service.confirm.sheet.confirmLabel, service.confirm.sheet.destructive], ["Undo", true])
    compare(runs("apply").length, 0)
    compare(service.confirmYes(), "")
    compare(words(lastRun("apply").command), ["apply", planId], "Yes applies the undo's plan: " + launchLog())
    compare(runs("undo").length, 1, "the undo runs only through its plan")
  }

  // ---- the doctor's repairs (G2, M4-D44): a bound preview of `doctor fix --only`, Yes
  // the same argv bound to its token, the undo through `undo`'s stored plan. The doctor's
  // envelope has the report's keys at the top level and no `data` (M4-D34).

  readonly property string credentialFm: "fm-secrets-credential-file-mode"
  readonly property string credentialPath: Build.CONTEXT.XDG_CONFIG_HOME + "/omaddons/credentials/github"
  readonly property string repairToken: "pc1:" + "a".repeat(64)
  readonly property string repairRun: "01M9RVNRVNRVNRVNRVNRVNRVNR"

  function doctorEnvelope(command, exitCode, report) {
    var e = { "ok": exitCode === 0, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": command, "exit_code": exitCode }
    for (var key in report) e[key] = report[key]
    e.warnings = []
    e.commands = []
    return JSON.stringify(e)
  }

  // One chmod of the credential (E6's ActionPlanned, its primitive flattened).
  function chmodAction(extra) {
    return merged({ "seq": 0, "fix": 0, "fm": credentialFm, "op": "chmod", "path": credentialPath, "mode": "0600",
      "pre": { "kind": "file", "mode": "0644" }, "post": { "kind": "file", "mode": "0600" }, "guards": [], "bytes": 0 }, extra)
  }

  // A `doctor fix` report: `dryRun` plans `actions`, else takes them; `state` and `exit`
  // follow the doctor's dictionary unless given.
  function fixReport(o) {
    var dryRun = o.dryRun === true
    var state = o.state || (dryRun ? "DONE_FINDINGS" : "DONE_OK")
    var exits = { "DONE_OK": 0, "DONE_FINDINGS": 1, "DONE_PARTIAL": 2, "DONE_FAILED": 3 }
    var exit = o.exit !== undefined ? o.exit : exits[state]
    var actions = o.actions || [chmodAction()]
    var report = {
      "doctor_contract_version": "1.0", "run_id": "2026-11-08T07-23-32Z__01M9RVN0000000000000000000", "run_dir": "/r",
      "state": state, "dry_run": dryRun, "tx": dryRun ? null : "01M9TX00000000000000000000",
      "summary": { "total_findings": 1, "findings_before": 1, "findings_after": dryRun ? null : (o.findingsAfter || 0),
        "actions_taken": dryRun ? null : actions.length, "actions_failed": dryRun ? null : 0, "bytes_to_back_up": 0,
        "bytes_backed_up": dryRun ? null : 0, "backups_restored": dryRun ? null : 0 },
      "findings": [], "skipped": [],
      "actions": dryRun ? null : actions.map(function(a) { return merged(a, { "ok": true }) }),
      "delegated": dryRun ? null : [],
      "actions_planned": dryRun ? actions : null,
      "delegated_planned": dryRun ? [] : null,
      "precondition": dryRun ? (o.token === undefined ? repairToken : o.token) : null,
      "undo_command": dryRun ? null : "omaddons doctor undo r", "undo_argv": dryRun ? null : ["omaddons", "doctor", "undo", "r"]
    }
    if (o.error) report.error = o.error
    return doctorEnvelope("doctor fix", exit, report)
  }

  function doctorError(code, message, details) {
    return { "code": code, "message": message, "fix": "omaddons doctor fix --only " + credentialFm + " --dry-run",
      "fix_argv": null, "alternatives": [], "retryable": false, "details": details || {} }
  }

  // A status whose last change is the repair, with its Undo (`undo --run`).
  function repairStatus() {
    return statusEnvelope(0, { "data": merged(JSON.parse(statusEnvelope(0)).data, { "last_change": {
      "run": repairRun, "kind": "repair", "at": "2026-11-08T07:23:33Z", "actor": "widget", "reason": "Repair: the token file",
      "targets": [], "changes": {}, "undo": { "command": "omaddons undo --run " + repairRun, "argv": ["omaddons", "undo", "--run", repairRun] } } }) })
  }

  // Asks for the credential repair; returns its preview's launch.
  function previewRepair() {
    compare(service.run("doctor_fix", { "fm": credentialFm }, "Repair: the token file"), "")
    var launch = lastRun("doctor")
    verify(launch !== null, "the repair's preview: " + launchLog())
    return launch
  }

  // The repair previewed and confirmed: returns the bound run's launch.
  function confirmRepair() {
    answer(previewRepair(), fixReport({ "dryRun": true }))
    verify(service.confirm !== null, "the repair asks first: " + launchLog())
    compare(service.confirmYes(), "")
    var bound = lastRun("doctor")
    verify(bound.command.indexOf("--if-match") !== -1, "Yes runs bound: " + launchLog())
    return bound
  }

  // The `--if-match` launches of `doctor fix` so far.
  function boundRepairs() {
    return runs("doctor").filter(function(l) { return l.command.indexOf("--if-match") !== -1 })
  }

  function test_a_repair_previews_then_runs_bound_to_its_token() {
    ready(0)
    var preview = previewRepair()
    compare(words(preview.command), ["doctor", "fix", "--only=" + credentialFm])
    verify(preview.command.indexOf("--dry-run") !== -1, "a dry run first")
    verify(preview.command.indexOf("--if-match") === -1, "no token yet")
    compare(JSON.parse(service.stateJson()).lanes.mutation.running, "doctor_fix", "on the mutation lane")
    compare(service.action.message, "Checking what it would repair…")
    // The dry run plans one chmod and exits 1 (findings): a dialog, never a failure.
    answer(preview, fixReport({ "dryRun": true }))
    compare(service.lastError, null, "a planning dry run is no failure")
    verify(service.confirm !== null, "the repair asks first")
    compare(sheetLines(), ["Repair: the token file?", "chmod 0600 <config>/credentials/github (was 0644)", "Undo last reverts it."])
    compare([service.confirm.sheet.confirmLabel, service.confirm.sheet.destructive], ["Repair", false])
    compare(boundRepairs().length, 0, "nothing ran before Yes: " + launchLog())
    compare(service.confirmYes(), "")
    var bound = lastRun("doctor")
    verify(bound !== preview)
    compare(words(bound.command), ["doctor", "fix", "--only=" + credentialFm])
    compare(bound.command[bound.command.indexOf("--if-match") + 1], repairToken)
    verify(bound.command.indexOf("--dry-run") === -1)
    compare(bound.command.slice(4, 8), ["--reason", "Repair: the token file (bar widget)", "--progress", "json"])
    compare(bound.environment, Build.CONTEXT)
    compare(runs("apply").length, 0, "a repair stores no plan")
    answer(bound, fixReport({}))
    compare(service.lastResult.line, "Repaired: chmod 0600 <config>/credentials/github (was 0644)")
    verify(service.lastResult.ok)
    verify(service.lastResult.repair === true)
    verify(service.lastResult.statusSeen === false, "its Undo waits for a status asked after it")
    compare(service.lastError, null)
    // The status the repair's change rang for: from then on its Undo may show.
    answer(lastRun("status"), repairStatus())
    verify(service.lastResult.statusSeen === true)
    compare(service.lastChange.kind, "repair")
    var undo = Model.resultUndo(service.lastResult, service.lastChange)
    compare(undo.argv, ["omaddons", "undo", "--run", repairRun])
  }

  function test_a_status_read_before_the_repair_finished_offers_no_undo() {
    ready(0)
    var bound = confirmRepair()
    // A status already running when the repair commits read the history before it.
    service.refresh()
    var early = lastRun("status")
    answer(bound, fixReport({}))
    answer(early, repairStatus())
    verify(service.lastResult.statusSeen === false, "a status asked before the result is not its proof")
    answer(lastRun("status"), repairStatus())
    verify(service.lastResult.statusSeen === true)
  }

  function test_a_stale_repair_previews_again_and_runs_once() {
    ready(0)
    var bound = confirmRepair()
    answer(bound, fixReport({ "state": "REFUSING", "exit": 4,
      "error": doctorError("plan_stale", "the repair is no longer what its preview showed", { "changed": ["observed"] }) }))
    var again = lastRun("doctor")
    verify(again !== bound, "previewed again: " + launchLog())
    verify(again.command.indexOf("--dry-run") !== -1)
    verify(again.command.indexOf("--if-match") === -1)
    answer(again, fixReport({ "dryRun": true }))
    verify(service.confirm.stale.indexOf("Changed since you looked: the files this repair checks") === 0, service.confirm.stale)
    compare(service.confirm.sheet.stale, service.confirm.stale, "the sheet's banner")
    compare(boundRepairs().length, 1, "the bound run is never launched twice: " + launchLog())
  }

  function test_a_repair_with_nothing_left_refreshes_and_runs_nothing() {
    ready(0)
    var statuses = runs("status").length
    answer(previewRepair(), fixReport({ "dryRun": true, "state": "DONE_OK", "actions": [] }))
    compare(service.confirm, null)
    compare(service.lastResult.line, "Nothing to repair now; the view is refreshed")
    compare(runs("status").length, statuses + 1, "the view is refreshed")
    compare(boundRepairs().length, 0, launchLog())
    compare(JSON.parse(service.stateJson()).lastPreview.decision, "refresh")
  }

  function test_a_repair_preview_that_plans_another_failure_mode_runs_nothing() {
    ready(0)
    answer(previewRepair(), fixReport({ "dryRun": true, "actions": [chmodAction(), chmodAction({ "seq": 1, "fm": "fm-other" })] }))
    compare(service.confirm, null)
    compare(service.lastError.code, "not_previewed")
    compare(service.lastError.message, "The preview planned more than this repair; nothing ran")
    compare(boundRepairs().length, 0, launchLog())
  }

  function test_a_repair_preview_without_a_token_runs_nothing() {
    ready(0)
    answer(previewRepair(), fixReport({ "dryRun": true, "token": null }))
    compare(service.confirm, null)
    compare(service.lastError.message, "The preview gave no token to bind; nothing ran")
    compare(boundRepairs().length, 0, launchLog())
  }

  function test_a_refused_repair_preview_shows_its_error_and_fix() {
    ready(0)
    answer(previewRepair(), fixReport({ "dryRun": true, "state": "REFUSING", "exit": 4, "actions": [],
      "error": doctorError("doctor_refused", "retail is running; quit the game first", { "reason": "client_running" }) }))
    compare(service.confirm, null)
    compare(service.lastError.message, "retail is running; quit the game first")
    compare(service.lastError.code, "doctor_refused")
    compare(service.lastError.fixText, "omaddons doctor fix --only " + credentialFm + " --dry-run")
    compare(service.failedPreview.template, "doctor_fix")
    compare(JSON.parse(service.stateJson()).lastPreview.decision, "failed")
    compare(boundRepairs().length, 0)
  }

  function test_a_partial_repair_is_a_report_not_an_error() {
    ready(0)
    answer(confirmRepair(), fixReport({ "state": "DONE_PARTIAL", "findingsAfter": 1 }))
    verify(!service.lastResult.ok)
    verify(service.lastResult.partial)
    compare(service.lastResult.line, "Partly repaired (1 change); 1 finding remains")
    compare(service.lastError, null, "exit 2 is a report")
  }

  function test_a_rolled_back_repair_says_so_and_sets_no_error() {
    ready(0)
    answer(confirmRepair(), fixReport({ "state": "DONE_FAILED", "findingsAfter": 1 }))
    compare(service.lastResult.line, "The repair failed and was rolled back; nothing changed")
    verify(service.lastResult.repair === false, "nothing changed, so there is nothing to undo")
    compare(service.lastError, null, "exit 3 is a report")
  }

  function test_a_locked_repair_shows_its_error() {
    ready(0)
    var locked = JSON.parse(fixReport({ "state": "REFUSING", "exit": 5,
      "error": doctorError("locked", "another omaddons holds the lock", {}) }))
    locked.error.retryable = true
    var bound = confirmRepair()
    bound.process.finish(5, JSON.stringify(locked))
    compare(service.lastError.code, "locked")
    compare(service.lastError.message, "another omaddons holds the lock")
    compare(boundRepairs().length, 1, "nothing re-runs by itself: " + launchLog())
    compare(runs("doctor").length, 2, launchLog())
  }

  function test_a_repair_finished_after_the_panel_closed_says_so_once() {
    ready(0)
    service.setPanelOpen("p", true)
    var toasts = function() {
      return Quickshell.detachedCommands.filter(function(c) { return c[0] === "omarchy-notification-send" })
    }
    var bound = confirmRepair()
    compare(toasts().length, 0, "the preview raises none")
    service.setPanelOpen("p", false)
    answer(bound, fixReport({}))
    compare(toasts().length, 1, JSON.stringify(Quickshell.detachedCommands))
    verify(toasts()[0].indexOf("Repaired: chmod 0600 <config>/credentials/github (was 0644)") !== -1, JSON.stringify(toasts()[0]))
  }

  function test_state_reports_the_repair_preview() {
    ready(0)
    answer(previewRepair(), fixReport({ "dryRun": true }))
    var state = JSON.parse(service.stateJson())
    compare(state.lastPreview, { "resource": "doctor_fix", "decision": "dialog", "planId": null })
    compare(state.confirm, { "heading": "", "planId": null, "title": "Repair: the token file?",
      "text": "Repair: the token file?\nchmod 0600 <config>/credentials/github (was 0644)\nUndo last reverts it." })
  }

  function test_undo_last_reverts_a_repair_through_its_stored_plan() {
    ready(0)
    compare(service.runCliArgv(["omaddons", "undo", "--run", repairRun], "Undo repair"), "")
    var preview = lastRun("undo")
    verify(preview.command.indexOf("--dry-run") !== -1)
    var undone = { "kind": "repair", "reverted": { "tx": "01M9TX00000000000000000000", "kind": "repair", "targets": [] }, "folders": null,
      "repairs": [{ "seq": 0, "fix": 0, "fm": credentialFm, "op": "chmod", "path": credentialPath, "to_rel": null, "octal_mode": "0644", "activation": false }] }
    answer(preview, planPreview("undo", [planItem({ "op": "undo", "addon": null, "source": null, "release_id": null, "asset": null,
      "from_version": null, "to_version": null })], { "routine": false, "reasons": ["destructive"] }, undone, { "destructive": true }))
    var lines = sheetLines()
    compare(lines[0], "Undo the repair from an earlier time?")
    compare(lines[1], "chmod 0644 <config>/credentials/github (readable by other users again)", "the paths are shortened")
    compare([service.confirm.sheet.confirmLabel, service.confirm.sheet.destructive], ["Undo", true])
    compare(runs("apply").length, 0)
    compare(service.confirmYes(), "")
    var apply = lastRun("apply")
    compare(words(apply.command), ["apply", planId], "Yes applies the undo's plan: " + launchLog())
    answer(apply, applyEnvelope("undo", merged(undone, { "dry_run": false })))
    compare(service.lastResult.line, "Undid repair (1 change)")
    compare(runs("undo").length, 1, "the undo runs only through its plan")
  }

  function test_the_explanation_is_view_bound_and_dropped_once_closed() {
    ready(0)
    var explainEnv = function(title) {
      return doctorEnvelope("doctor explain", 0, { "doctor_contract_version": "1.0",
        "failure_mode": { "id": credentialFm, "title": title, "description": "d", "why": "w", "fixer": null, "manual": null },
        "instances": [], "skipped": [], "incomplete": null })
    }
    var argv = ["omaddons", "doctor", "explain", "fm-x"]
    verify(service.readExplanation("p//0", argv))
    var first = lastRun("doctor")
    compare(words(first.command), ["doctor", "explain", "fm-x"])
    verify(first.command.indexOf("--progress") === -1, "a read")
    compare(JSON.parse(service.stateJson()).lanes.local.running, "explain")
    service.clearExplanation()
    answer(first, explainEnv("stale"))
    compare(service.explanation, null, "a late answer for a closed row is dropped")
    verify(service.readExplanation("p//0", argv))
    answer(lastRun("doctor"), explainEnv("The GitHub token file is readable by other users"))
    compare(service.explanation.selection, { "key": "p//0" })
    compare(service.explanation.lines[0], "The GitHub token file is readable by other users")
    // A failed read says so for its row (an unknown id is the doctor's usage exit, 64).
    verify(service.readExplanation("p//0", argv))
    var failed = JSON.parse(doctorEnvelope("doctor explain", 64, {}))
    failed.error = { "code": "usage", "message": "unknown failure mode fm-x", "fix": "omaddons help doctor explain",
      "fix_argv": ["omaddons", "help", "doctor", "explain"], "alternatives": [], "retryable": false, "details": {} }
    lastRun("doctor").process.finish(64, JSON.stringify(failed))
    compare(service.explanation.lines, null)
    compare(service.explanation.failed, "unknown failure mode fm-x")
    // Only a `doctor explain` argv the classifier accepts is read.
    verify(!service.readExplanation("p//0", ["omaddons", "doctor", "fix", "--only", "fm-x"]))
    verify(!service.readExplanation("p//0", null))
  }

  // ---- actions and lanes (M3-D11)

  function test_an_action_reports_progress_then_its_result() {
    ready(2)
    var bar = { "addon": "Bar", "release_id": "gh:o/bar@v2", "asset": "Bar.zip" }
    compare(service.run("update_all", {}, "Update all", [shownItem(), shownItem(bar)]), "")
    var preview = lastRun("update")
    verify(service.bar.spinning)
    compare(service.action.label, "Update all")
    compare(service.action.message, "Preparing…")
    answer(preview, planPreview("update", [planItem(), planItem(bar)]))
    // A routine plan that is what the rows showed applies with no dialog.
    compare(service.confirm, null)
    var launch = lastRun("apply")
    verify(launch !== null, "the routine preview's apply: " + launchLog())
    verify(service.bar.spinning)
    compare(service.action.label, "Update all")
    launch.process.emitStderr(JSON.stringify({ "event": "progress", "phase": "download", "message": "gh:o/bar@v2 on era", "current": 1, "total": 4 }))
    compare(service.action.message, "download 1/4 · gh:o/bar@v2 on era")
    compare(service.action.current, 1)
    launch.process.finish(0, applyEnvelope("update",
      { "targets": [{ "target": { "id": "retail" }, "items": [{ "status": "updated" }, { "status": "updated" }], "reload_required": "reload" }], "summary": { "changed": 2 } },
      [{ "target": "retail", "key": "01M8Q2RRRRRRRRRRRRRRRRRRRR", "status": "committed", "tx": "01M8Q2TTTTTTTTTTTTTTTTTTTT", "error_code": null }],
      { "commands": [{ "action": "undo_run", "command": "omaddons undo --run 01M", "argv": ["omaddons", "undo", "--run", "01M"], "destructive": false }] }))
    compare(service.action, null)
    verify(!service.bar.spinning)
    // The apply reads as the update it ran.
    compare(service.lastResult.line, "Updated 2 addons · /reload in game")
    compare(service.lastResult.commands[0].runnable, true)
    // The result refreshes the bar.
    compare(words(lastRun("status").command)[0], "status")
  }

  // ---- the activity bar (1n2p.18)

  function shownText() {
    var view = Model.activityView(service.activity, service.targets, Date.now())
    return view ? view.text : null
  }

  function test_check_now_shows_at_once_counts_its_progress_and_spins_the_bar() {
    ready(0)
    verify(!service.bar.spinning)
    verify(service.check())
    var launch = lastRun("outdated")
    verify(launch !== null, "the check runs: " + launchLog())
    compare(launch.command.slice(6, 8), ["--progress", "json"])
    compare(service.activity.kind, "check")
    compare(shownText(), "Checking for updates…")
    verify(service.bar.spinning, "a check spins the bar (UX-01)")
    launch.process.emitStderr(JSON.stringify({ "event": "progress", "phase": "fetch", "message": "gh:o/foo", "current": 12,
      "total": 42, "target": null, "ref": "gh:o/foo" }))
    compare(shownText(), "Checking for updates (12 of 42)")
    compare(service.action, null, "a check is no change")
    answer(launch, envelope("outdated", { "checked_at": "2100-01-01T00:00:00Z", "requests": { "fetched": 1, "probed": 0, "reused": 0 },
      "targets": [], "summary": {} }))
    compare(service.activity, null)
    verify(!service.bar.spinning)
  }

  function test_an_automatic_check_shows_too_but_a_background_read_does_not() {
    ready(0)
    service.request({ "resource": "check", "template": "check", "lane": "network", "priority": "background", "label": "Automatic update check" })
    compare(service.activity.kind, "check")
    verify(service.bar.spinning)
    answer(lastRun("outdated"), envelope("outdated", { "checked_at": "2100-01-01T00:00:00Z", "targets": [], "summary": {} }))
    // The refresh the check asks for, then a background read of Lua errors.
    answer(lastRun("status"), statusEnvelope(0))
    service.readErrorsOf("retail")
    verify(lastRun("errors") !== null, "the errors read runs: " + launchLog())
    compare(service.activity, null, "a background read is silent (M3-D11)")
    verify(!service.bar.spinning)
  }

  function test_a_check_waiting_behind_a_search_spins_the_bar_and_is_named() {
    ready(0)
    service.searchFor("dbm", "")
    compare(service.activity.kind, "read")
    compare(shownText(), "Searching…")
    verify(!service.bar.spinning, "a search does not spin the bar")
    verify(service.check())
    compare(runs("outdated").length, 0, "the check waits for the network lane")
    verify(service.bar.spinning, "the check the player asked for spins at once")
    compare(service.queuedText, "Next: Checking for updates")
  }

  function test_a_read_can_be_cancelled_and_its_lane_frees() {
    ready(0)
    service.lastError = null
    service.check()
    var launch = lastRun("outdated")
    verify(service.activity.cancellable)
    verify(service.cancelActivity())
    compare(launch.process.signals, [15])
    compare(service.activity, null)
    compare(service.lastError, null, "a read the player stopped is no failure")
    compare(service.lastCheck, null, "nor a check that did not answer")
    // The lane is free: the next check runs at once.
    service.check()
    verify(lastRun("outdated") !== launch, "a second check runs")
  }

  function test_a_change_in_flight_is_never_cancelled() {
    ready(2)
    compare(service.run("update_all", {}, "Update all", [shownItem()]), "")
    var preview = lastRun("update")
    compare(service.activity.kind, "change")
    verify(!service.activity.cancellable)
    verify(!service.cancelActivity())
    compare(preview.process.signals, [])
  }

  function test_rows_show_their_own_phase_and_say_when_they_changed() {
    make({ "rowDoneMs": 60 })
    ready(2)
    // The rows' refs, as `list -v` gives them.
    service.lists = Model.reduceList(JSON.parse(envelope("list", { "targets": [{ "id": "retail", "key": "01M8Q2RRRRRRRRRRRRRRRRRRRR",
      "addons": [{ "name": "Foo", "ref": "gh:o/foo" }, { "name": "Bar", "ref": "gh:o/bar" }] }] })))
    var bar = { "addon": "Bar", "release_id": "gh:o/bar@v2", "asset": "Bar.zip" }
    compare(service.run("update_all", {}, "Update all", [shownItem(), shownItem(bar)]), "")
    // The preview moves no row: it only asks what it would do.
    compare(Model.activityRow(service.activity, "retail/Foo", null), null)
    answer(lastRun("update"), planPreview("update", [planItem(), planItem(bar)]))
    var launch = lastRun("apply")
    compare(Model.activityRow(service.activity, "retail/Foo", null).text, "Updating…")
    compare(shownText(), "Updating 2 addons on retail…")
    launch.process.emitStderr(JSON.stringify({ "event": "progress", "phase": "download", "message": "gh:o/bar@v2 on retail",
      "current": 1, "total": 2, "target": "retail", "ref": "gh:o/bar" }))
    compare(Model.activityRow(service.activity, "retail/Bar", null).text, "Updating… downloading (1 of 2)")
    compare(Model.activityRow(service.activity, "retail/Foo", null).text, "Updating…")
    compare(shownText(), "Updating 2 addons on retail: Bar, downloading (1 of 2)")
    // A change asked meanwhile is named as the next one.
    compare(service.run("update_target", { "target": "retail" }, "Update Retail"), "")
    compare(service.queuedText, "Next: Update Retail")
    launch.process.finish(0, applyEnvelope("update",
      { "targets": [{ "target": { "id": "retail" }, "items": [{ "name": "Foo", "status": "updated" }, { "name": "Bar", "status": "updated" }],
        "reload_required": "none" }], "summary": { "changed": 2 } },
      [{ "target": "retail", "key": "01M8Q2RRRRRRRRRRRRRRRRRRRR", "status": "committed", "tx": "01M8Q2TTTTTTTTTTTTTTTTTTTT", "error_code": null }]))
    compare(Model.activityRow(null, "retail/Bar", service.doneRows), { "pending": false, "text": "Updated" })
    tryCompare(service, "doneRows", ({}), 1000)
  }

  function test_the_last_result_shows_for_its_hold() {
    make({ "resultHoldMs": 80 })
    ready(0)
    compare(service.resultShown, false)
    compare(service.run("pin", { "target": "retail", "addon": "Foo" }, "Pin Foo"), "")
    answer(lastRun("pin"), envelope("pin", { "targets": [{ "target": { "id": "retail" }, "items": [{ "name": "Foo", "status": "pinned" }] }] }))
    verify(service.lastResult !== null)
    verify(service.resultShown)
    tryCompare(service, "resultShown", false, 1000)
    verify(service.lastResult !== null, "the result itself stays, for state()")
  }

  function test_a_failure_stays_until_it_is_dismissed() {
    ready(0)
    service.lastError = { "message": "no", "code": "network", "fixes": [] }
    service.dismissError()
    compare(service.lastError, null)
  }

  function test_user_mutations_go_before_background_ones() {
    service.templates = Object.assign({}, Model.COMMANDS, fixtureTemplates)
    ready(0)
    service.run("update_all", {}, "Update all")
    compare(service.run("pin_silent", { "target": "retail", "addon": "Baz" }, "Pin"), "")
    compare(service.run("update_target", { "target": "retail" }, "Update retail"), "")
    compare(runs("pin").length, 0)
    answer(lastRun("update"), noopPreview("update"))
    compare(words(lastRun("update").command), ["update", "-t", "retail"])
    compare(runs("pin").length, 0)
    answer(lastRun("update"), noopPreview("update"))
    compare(runs("pin").length, 1)
  }

  function test_a_third_user_mutation_replaces_the_queued_one() {
    ready(0)
    service.run("update_all", {}, "Update all")
    service.run("update_target", { "target": "retail" }, "Update retail")
    service.run("update_target", { "target": "era" }, "Update era")
    compare(service.queuedAction, "Update era")
    compare(service.queues.mutation.length, 1)
  }

  function test_all_searches_every_client_in_one_request() {
    ready(0)
    service.searchFor("damage meter", "")
    compare(words(lastRun("search").command), ["search", "--limit", "20", "--", "damage meter"])
  }

  function test_dismissals_go_one_client_at_a_time_and_a_stale_one_stops_the_rest() {
    ready(0)
    var plan = [{ "target": "era", "token": "pc1:" + "1".repeat(64) }, { "target": "retail", "token": "pc1:" + "2".repeat(64) }]
    compare(service.dismissErrors(plan), "")
    var first = lastRun("errors")
    compare(words(first.command), ["errors", "dismiss", "-t", "era"])
    var at = first.command.indexOf("--if-match")
    compare(first.command[at + 1], plan[0].token)
    compare(runs("errors").length, 1, "the second waits for the first")
    answer(first, envelope("errors dismiss", { "targets": [] }))
    var second = runs("errors").filter(function(l) { return words(l.command)[1] === "dismiss" })[1]
    verify(second !== undefined)
    compare(words(second.command), ["errors", "dismiss", "-t", "retail"])
    compare(second.command[second.command.indexOf("--if-match") + 1], plan[1].token)
    answer(second, envelope("errors dismiss", { "targets": [] }))
    compare(service.pendingDismissals.length, 0)
    // A stale observation stops what is left, and says why.
    compare(service.dismissErrors(plan), "")
    var stale = runs("errors").filter(function(l) { return words(l.command)[1] === "dismiss" })[2]
    answer(stale, failure("errors dismiss", "plan_stale", "the errors changed since they were shown", { "changed": ["era"] }))
    compare(runs("errors").filter(function(l) { return words(l.command)[1] === "dismiss" }).length, 3)
    compare(service.lastError.message, "New errors arrived; review them")
    compare(service.pendingDismissals.length, 0)
  }

  function test_the_backups_are_read_with_the_panel_and_their_detail_is_view_bound() {
    ready(0)
    service.setPanelOpen("bar@DP-1", true)
    answer(lastRun("status"), statusEnvelope(0))
    answer(lastRun("list"), envelope("list", { "targets": [] }))
    answer(lastRun("errors"), envelope("errors", { "targets": [] }))
    compare(words(lastRun("backup").command), ["backup", "list", "--limit", "50"])
    answer(lastRun("backup"), envelope("backup list", { "targets": [{ "target": { "id": "retail" }, "backups": [], "total": 0 }] }))
    compare(service.backups.targets[0].id, "retail")
    service.readBackupsDetail("retail")
    var detail = lastRun("backup")
    compare(words(detail.command), ["backup", "list", "-v", "-t", "retail", "--limit", "50"])
    service.clearBackupsDetail()
    answer(detail, envelope("backup list", { "targets": [] }))
    compare(service.backupsDetail, null)
  }

  // A restore refused for ambiguous ownership: each folder choice reruns that restore's
  // preview, through the template, never the CLI's own argv.
  function test_an_ambiguous_restore_reruns_for_the_folder_chosen() {
    ready(0)
    var id = "01M5JWP7MW633DZJ0TYDMBV8JN"
    compare(service.run("restore_addon", { "target": "retail", "addon": "WeakAuras", "backup": id }, "Restore WeakAuras's settings"), "")
    var preview = lastRun("backup")
    verify(preview.command.indexOf("--dry-run") !== -1)
    answer(preview, JSON.stringify({ "ok": false, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": "backup restore",
      "exit_code": 3, "warnings": [], "commands": [], "error": { "code": "ref_ambiguous", "message": "WeakAurasOptions is another addon's now",
        "fix": "omaddons backup list -t retail -v", "fix_argv": ["omaddons", "backup", "list", "-t", "retail", "-v"], "retryable": false, "details": {},
        "alternatives": [{ "display": "x", "argv": ["omaddons", "--actor", "widget", "--reason", "r", "--dry-run", "backup", "restore", "-t", "retail",
          "--addon", "folder:WeakAurasOptions", "--", id] }] } }))
    compare(service.confirm, null)
    compare(service.lastError.choices[0].addon, "folder:WeakAurasOptions")
    compare(service.restoreFolder("folder:WeakAurasOptions"), "")
    var again = lastRun("backup")
    verify(again !== preview)
    verify(again.command.indexOf("--dry-run") !== -1)
    compare(words(again.command), ["backup", "restore", "-t", "retail", "--addon=folder:WeakAurasOptions", "--", id])
  }

  function test_raw_details_are_dropped_once_their_row_closed() {
    ready(0)
    service.readRawErrors("retail", "Foo")
    var raw = lastRun("errors")
    compare(words(raw.command), ["errors", "--raw-details", "-t", "retail", "--addon=Foo"])
    service.clearRawErrors()
    answer(raw, envelope("errors", { "raw_details": true, "targets": [] }))
    compare(service.rawErrors, null)
  }

  function test_a_late_search_for_another_target_is_dropped() {
    ready(0)
    service.searchFor("dbm", "retail")
    var first = lastRun("search")
    service.searchFor("dbm", "era")
    answer(first, envelope("search", { "query": "dbm", "results": [{ "name": "DBM", "ref": "wowi:1", "source": "wowi", "compat": { "retail": "likely" } }], "sources_skipped": [] }))
    compare(service.search, null)
    var second = lastRun("search")
    verify(second !== first)
    compare(words(second.command), ["search", "--limit", "20", "-t", "era", "--", "dbm"])
    answer(second, envelope("search", { "query": "dbm", "results": [{ "name": "DBM", "ref": "wowi:1", "source": "wowi", "compat": { "era": "likely" } }], "sources_skipped": [] }))
    compare(service.search.selection.target, "era")
    compare(service.search.rows[0].compat, "likely")
  }

  function test_a_late_status_applies_when_nothing_newer_did() {
    var startup = lastRun("status")
    service.refresh()
    answer(startup, statusEnvelope(5))
    compare(service.bar.badge, 5)
    answer(lastRun("status"), statusEnvelope(6))
    compare(service.bar.badge, 6)
  }

  function test_status_jumps_the_local_queue() {
    ready(0)
    service.readList()
    var slow = lastRun("list")
    service.readList()
    service.refresh()
    answer(slow, envelope("list", { "targets": [] }))
    compare(words(ProcessRegistry.launches[ProcessRegistry.launches.length - 1].command)[0], "status")
  }

  function test_opening_a_panel_reads_full_status_then_list_locally() {
    ready(0)
    var statuses = runs("status").length
    service.setPanelOpen("bar@DP-1", true)
    var full = lastRun("status")
    compare(words(full.command), ["status", "-v"])
    answer(full, statusEnvelope(0))
    compare(words(lastRun("list").command), ["list", "-v"])
    // Then the Issues view's errors, still local.
    answer(lastRun("list"), envelope("list", { "targets": [] }))
    compare(words(lastRun("errors").command), ["errors", "--limit", "10"])
    // A second copy opening adds nothing.
    service.setPanelOpen("bar@HDMI-A-1", true)
    compare(runs("status").length, statuses + 1)
    verify(service.panelOpen)
    service.setPanelOpen("bar@DP-1", false)
    verify(service.panelOpen)
    service.setPanelOpen("bar@HDMI-A-1", false)
    verify(!service.panelOpen)
  }

  function test_a_view_reads_when_old_or_stale_and_a_doorbell_rereads_the_open_one() {
    ready(0)
    verify(service.readView("installed"))
    answer(lastRun("list"), envelope("list", { "targets": [] }))
    compare(service.readView("installed"), false)
    compare(service.readView("updates"), false)
    compare(runs("list").length, 1)
    // A doorbell with the panel closed reads nothing but status, and leaves the views stale.
    bell().process.emitStdout("doorbell.json")
    wait(50)
    compare(runs("list").length, 1)
    answer(lastRun("status"), statusEnvelope(0))
    verify(service.readView("installed"))
    answer(lastRun("list"), envelope("list", { "targets": [] }))
    // With the panel open on a view, the doorbell reads that view again.
    service.setPanelOpen("bar@DP-1", true)
    answer(lastRun("status"), statusEnvelope(0))
    answer(lastRun("list"), envelope("list", { "targets": [] }))
    answer(lastRun("errors"), envelope("errors", { "targets": [] }))
    answer(lastRun("backup"), envelope("backup list", { "targets": [] }))
    var lists = runs("list").length
    bell().process.emitStdout("doorbell.json")
    wait(50)
    answer(lastRun("status"), statusEnvelope(0))
    compare(runs("list").length, lists + 1)
  }

  function test_a_mutation_without_a_binary_says_so() {
    ready(0)
    ProcessRegistry.missingBinary = true
    service.run("update_all", {}, "Update all")
    wait(20)
    compare(service.phase, "missing")
    compare(service.lastError.code, "missing")
    compare(service.action, null)
  }

  // ---- deadlines and termination

  function test_a_network_read_with_progress_outlives_the_deadline_and_a_silent_one_does_not() {
    ready(0)
    service.check()
    var launch = lastRun("outdated")
    for (var i = 0; i < 8; i++) {
      wait(100)
      launch.process.emitStderr(JSON.stringify({ "event": "progress", "phase": "fetch", "message": "gh:o/a", "current": i, "total": 8 }))
    }
    compare(launch.process.signals, [])
    compare(service.outcomes.check, undefined)
    wait(500)
    compare(service.outcomes.check, "timeout")
  }

  function test_a_check_longer_than_the_local_deadline_is_not_killed() {
    make({ "localDeadlineMs": 100, "networkDeadlineMs": 1000 })
    ready(0)
    service.check()
    var launch = lastRun("outdated")
    wait(300)
    compare(launch.process.signals, [])
    launch.process.finish(0, envelope("outdated", { "targets": [] }))
    compare(service.outcomes.check, "ok")
  }

  function test_a_read_that_ignores_sigterm_is_killed_after_the_grace_and_holds_its_lane() {
    ready(0)
    service.readList()
    var launch = lastRun("list")
    launch.process.ignoreTerm = true
    service.refresh()
    var statuses = runs("status").length
    wait(450)
    compare(launch.process.signals, [15])
    // The lane stays busy: the queued status waits for the child to really exit.
    compare(runs("status").length, statuses)
    wait(150)
    compare(service.outcomes.list, "timeout")
    compare(runs("status").length, statuses + 1)
  }

  // ---- IPC, show and reinstall

  function test_state_has_the_documented_shape() {
    ready(4)
    var state = JSON.parse(service.stateJson())
    compare(Object.keys(state).sort(), ["action", "activity", "bar", "binary", "confirm", "contextGeneration", "lanes", "lastPreview",
      "lastRefreshMs", "lastResult", "panel", "panelOpen", "phase", "summary", "toolVersion", "widgetVersion"])
    compare([state.confirm, state.lastPreview, state.activity], [null, null, null])
    // What the activity bar says, in words (1n2p.18).
    service.check()
    var checking = JSON.parse(service.stateJson()).activity
    compare([checking.kind, checking.text, checking.cancellable], ["check", "Checking for updates…", true])
    service.cancelActivity()
    // The open panel's scope, view, page depth, sheet and zoom; null while none is open.
    compare(state.panel, null)
    service.panelState = { "scope": "retail", "view": "updates", "stackDepth": 0, "sheet": null, "zoomed": false, "textZoom": 1 }
    service.setPanelOpen("p", true)
    compare(JSON.parse(service.stateJson()).panel.scope, "retail")
    service.setPanelOpen("p", false)
    compare(JSON.parse(service.stateJson()).panel, null)
    // A check runs on the network lane: `checking` says so (the hero's spinner).
    verify(!service.checking)
    compare(state.phase, "ok")
    compare(state.bar.badge, 4)
    compare(state.binary, Build.BINARY)
    compare(state.toolVersion, Build.TOOL_VERSION)
    compare(Object.keys(state.lanes).sort(), ["local", "mutation", "network"])
    compare(state.lanes.mutation, { "running": null, "queued": 0 })
    verify(state.lastRefreshMs > 0)
  }

  function test_reinstall_is_detached_under_the_stamped_context() {
    compare(service.reinstallWidget(), "ok")
    compare(ProcessRegistry.detached.length, 1)
    compare(ProcessRegistry.detached[0].command, [Build.BINARY, "--actor", "widget", "--reason", "reinstall from the bar widget", "omarchy", "install"])
    compare(ProcessRegistry.detached[0].environment, Build.CONTEXT)
  }

  function test_show_summons_the_widget_by_its_build_id() {
    var calls = []
    service.shell = { "summon": function(id, payload) { calls.push([id, payload]); return true } }
    compare(service.show("updates"), "ok")
    compare(calls, [[Build.PLUGIN_ID, ""]])
    compare(service.requestedView, "updates")
    compare(Quickshell.detachedCommands, [])
    service.shell = null
    service.show("issues")
    compare(Quickshell.detachedCommands, [["omarchy-shell", "shell", "summon", Build.PLUGIN_ID, "{}"]])
  }

  function test_the_service_is_in_the_store_until_destroyed() {
    compare(Store.get(), service)
    service.destroy()
    service = null
    wait(0)
    compare(Store.get(), null)
  }
}
