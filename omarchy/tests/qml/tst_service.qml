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
    "remove_confirm": { "argv": ["remove", "-t", "{target}", "--", "{addon}"], "mutates": true, "confirm": true }
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
    answer(lastRun("remove"), envelope("remove", { "dry_run": true, "target": { "id": "retail" },
      "items": [{ "name": "Baz", "version": "v1", "folders": ["Baz"] }], "reload_required": "none", "precondition": "pc1:" + "a".repeat(64) }))
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
    answer(lastRun("remove"), envelope("remove", { "dry_run": true, "target": { "id": "retail" },
      "items": [{ "name": "Baz", "folders": ["Baz"] }], "reload_required": "none", "precondition": "pc1:" + "b".repeat(64) }))
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
    verify(service.lastError !== null && service.lastError.code === "incompatible")
  }

  // ---- confirmations (M3-D9, M3-D46)

  function test_a_confirmed_run_carries_its_previews_token_and_a_stale_one_previews_again() {
    ready(0)
    var token = "pc1:" + "c".repeat(64)
    compare(service.run("remove", { "target": "retail", "addon": "Baz" }, "Remove Baz"), "")
    var preview = lastRun("remove")
    verify(preview.command.indexOf("--dry-run") !== -1)
    verify(preview.command.indexOf("--if-match") === -1)
    answer(preview, envelope("remove", { "dry_run": true, "target": { "id": "retail" },
      "items": [{ "name": "Baz", "version": "v1", "folders": ["Baz"] }], "reload_required": "none", "precondition": token }))
    compare(service.confirm.text, "Remove Baz v1 from retail (1 folder)")
    compare(service.confirmYes(), "")
    var real = lastRun("remove")
    verify(real !== preview)
    compare(real.command.slice(6, 10), ["--progress", "json", "--if-match", token])
    verify(real.command.indexOf("--dry-run") === -1)
    compare(words(real.command), ["remove", "-t", "retail", "--", "Baz"])
    answer(real, failure("remove", "plan_stale", "this remove is no longer what its preview showed", { "changed": ["selection"] }))
    var again = lastRun("remove")
    verify(again !== real)
    verify(again.command.indexOf("--dry-run") !== -1)
    answer(again, envelope("remove", { "dry_run": true, "target": { "id": "retail" },
      "items": [{ "name": "Baz", "version": "v1", "folders": ["Baz"] }], "reload_required": "none", "precondition": "pc1:" + "d".repeat(64) }))
    verify(service.confirm.stale.indexOf("Changed since you looked: selection") === 0)
    compare(runs("remove").length, 3)
  }

  function test_a_destructive_cli_argv_previews_first() {
    ready(0)
    compare(service.runCliArgv(["omaddons", "undo", "--run", "01M5JHFF8X0FXBT1K068DQKCHG"], "Undo"), "")
    var preview = lastRun("undo")
    verify(preview.command.indexOf("--dry-run") !== -1)
    verify(service.runCliArgv(["omaddons", "--actor", "agent", "update"], "x").indexOf("widget_flag") !== -1)
  }

  // ---- actions and lanes (M3-D11)

  function test_an_action_reports_progress_then_its_result() {
    ready(2)
    compare(service.run("update_all", {}, "Update all"), "")
    var launch = lastRun("update")
    verify(service.bar.spinning)
    compare(service.action.label, "Update all")
    launch.process.emitStderr(JSON.stringify({ "event": "progress", "phase": "download", "message": "gh:o/bar@v2 on era", "current": 1, "total": 4 }))
    compare(service.action.message, "download 1/4 · gh:o/bar@v2 on era")
    compare(service.action.current, 1)
    launch.process.finish(0, envelope("update", { "targets": [{ "target": { "id": "retail" }, "items": [{ "status": "updated" }, { "status": "updated" }], "reload_required": "reload" }], "summary": { "changed": 2 } },
      { "commands": [{ "action": "undo_run", "command": "omaddons undo --run 01M", "argv": ["omaddons", "undo", "--run", "01M"], "destructive": false }] }))
    compare(service.action, null)
    verify(!service.bar.spinning)
    compare(service.lastResult.line, "Updated 2 addons · /reload in game")
    compare(service.lastResult.commands[0].runnable, true)
    // The result refreshes the bar.
    compare(words(lastRun("status").command)[0], "status")
  }

  function test_user_mutations_go_before_background_ones() {
    service.templates = Object.assign({}, Model.COMMANDS, fixtureTemplates)
    ready(0)
    service.run("update_all", {}, "Update all")
    compare(service.run("pin_silent", { "target": "retail", "addon": "Baz" }, "Pin"), "")
    compare(service.run("update_target", { "target": "retail" }, "Update retail"), "")
    compare(runs("pin").length, 0)
    lastRun("update").process.finish(0, envelope("update", { "targets": [], "summary": {} }))
    compare(words(lastRun("update").command), ["update", "-t", "retail"])
    compare(runs("pin").length, 0)
    lastRun("update").process.finish(0, envelope("update", { "targets": [], "summary": {} }))
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
    compare(Object.keys(state).sort(), ["action", "bar", "binary", "contextGeneration", "lanes", "lastRefreshMs",
      "lastResult", "panelOpen", "phase", "summary", "toolVersion", "widgetVersion"])
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
