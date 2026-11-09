import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "Fixtures.js" as Fixtures

// The doctor's repairs (G2) over envelopes the real binary wrote (pipeline step 1's
// repair scenes, through the Fixtures.js step 4 generates): the real Service on the mock
// Process, and a PanelView over it where a step presses keys. The fixture Build.js
// stamps its own context, so a path is matched here by its tail; bun asserts how it is
// shortened. The binary's reads the service makes on its own (a checkpoint, an update
// check, Lua errors, the views') are answered with empty envelopes by `settle`.
TestCase {
  id: testCase
  name: "Repairs"
  when: windowShown
  visible: true
  width: 600
  height: 720

  property var service: null
  property var frame: null

  // ubs:ignore — failure-mode ids, not secrets
  readonly property string credentialFm: "fm-secrets-credential-file-mode"
  readonly property string setAsideFm: "fm-concurrency_primitives-journal-too-new"

  Component {
    id: serviceComponent
    Service {}
  }

  Component {
    id: frameComponent
    PanelView {
      width: 540
      height: 700
    }
  }

  function init() {
    ProcessRegistry.reset()
    Quickshell.resetDetachedCommands()
  }

  function cleanup() {
    if (frame) frame.destroy()
    frame = null
    if (service) service.destroy()
    service = null
    ProcessRegistry.reset()
    wait(0)
  }

  // ---- helpers (tst_service.qml's and tst_plans.qml's)

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

  // The `doctor fix` launches holding `flag` (`--dry-run`, `--if-match`).
  function repairs(flag) {
    return runs("doctor").filter(function(l) { return words(l.command)[1] === "fix" && l.command.indexOf(flag) !== -1 })
  }

  function launchLog() {
    return JSON.stringify(runs().map(function(l) { return words(l.command) }))
  }

  function fixture(name) {
    return JSON.parse(Fixtures.text(name))
  }

  function envelope(command, data) {
    return JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": command,
      "exit_code": 0, "partial": false, "data": data, "warnings": [], "commands": [] })
  }

  // The binary's fixture `name` as the answer of `launch`, with the fixture's own exit.
  function answer(launch, name) {
    verify(launch !== null, "a launch to answer with " + name + ": " + launchLog())
    launch.process.finish(fixture(name).exit_code, Fixtures.text(name))
  }

  // Answers every omaddons read the service started on its own, until none runs: a status
  // with fixture `statusName`, and empty envelopes for the rest.
  function settle(statusName) {
    for (var round = 0; round < 30; round++) {
      var live = ProcessRegistry.processes.filter(function(p) {
        return p.running && p.command[1] === "--json" && p.command[2] === "--actor"
          && ["status", "errors", "outdated", "list", "backup"].indexOf(words(p.command)[0]) !== -1
      })
      if (live.length === 0) return
      var p = live[0]
      var w = words(p.command)
      if (w[0] === "status") p.finish(0, Fixtures.text(statusName))
      else p.finish(0, envelope(w[0] === "backup" ? "backup " + w[1] : w[0], { "targets": [] }))
      wait(0)
    }
    fail("the service's reads never settled: " + launchLog())
  }

  // The service at rest after its startup status answered with fixture `statusName`.
  function start(statusName) {
    service = serviceComponent.createObject(testCase, { "bellDebounceMs": 10, "localDeadlineMs": 4000, "networkDeadlineMs": 4000,
      "killGraceMs": 60, "streamGraceMs": 60 })
    verify(service !== null)
    answer(lastRun("status"), statusName)
    tryCompare(service, "phase", "ok")
    settle(statusName)
  }

  // A panel over the service, opened and focused.
  function openFrame() {
    frame = frameComponent.createObject(testCase, { "svc": service })
    verify(frame !== null)
    frame.opened = true
    frame.focusItem.forceActiveFocus()
    tryVerify(function() { return frame.focusItem.activeFocus })
  }

  function content() {
    var item = findChild(frame, "content").item
    verify(item !== null, "a loaded view")
    return item
  }

  // The Issues view, its reads answered.
  function showIssues(statusName) {
    keyClick("5")
    compare(frame.view, "issues")
    settle(statusName)
  }

  // The index of the Issues row whose fix repairs `fm`, or -1.
  function repairRow(fm) {
    var rows = content().rows
    for (var i = 0; i < rows.length; i++) if (rows[i].fix && rows[i].fix.slots.fm === fm) return i
    return -1
  }

  // The plan sheet, once it is open with the keyboard; its plan as text, a line each.
  function planSheet() {
    tryVerify(function() { return frame.confirming }, 3000, "the plan sheet opens: " + launchLog())
    var sheet = findChild(frame, "planSheet")
    tryVerify(function() { return sheet.keyItem.activeFocus })
    return sheet
  }

  function sheetLines(sheet) {
    return Model.dialogText(sheet.plan).split("\n")
  }

  // The credential repair asked for as the Issues row asks: its preview's launch.
  function previewCredential() {
    var rows = Model.issueRows(Model.issueSections(service.targets, [], "", service.machineIssues))
    var row = rows.filter(function(r) { return r.fix && r.fix.slots.fm === credentialFm })[0]
    verify(!!row, "the credential row: " + JSON.stringify(rows))
    compare(service.run(row.fix.template, row.fix.slots, row.fix.label), "")
    var preview = lastRun("doctor")
    verify(preview.command.indexOf("--dry-run") !== -1, "the repair's preview: " + launchLog())
    return preview
  }

  // The credential repair previewed and confirmed: the bound run's launch.
  function confirmCredential() {
    answer(previewCredential(), "repair-preview")
    verify(service.confirm !== null, "the repair asks first")
    compare(service.confirmYes(), "")
    var bound = lastRun("doctor")
    verify(bound.command.indexOf("--if-match") !== -1, "Yes runs bound: " + launchLog())
    return bound
  }

  // ---- the scenarios

  function test_a_credential_fix_from_issues_is_undone_with_undo_last_through_a_stored_plan() {
    start("repair-status")
    openFrame()
    showIssues("repair-status")
    var at = repairRow(credentialFm)
    verify(at >= 0, "the credential row: " + JSON.stringify(content().rows))
    frame.cursor = at
    frame.cursorActive = true
    compare(findChild(content(), "issueRow_" + at) && findChild(findChild(content(), "issueRow_" + at), "enterHint").text,
      "Enter previews the repair")
    keyClick(Qt.Key_Return)
    var preview = lastRun("doctor")
    compare(words(preview.command), ["doctor", "fix", "--only=" + credentialFm], "Enter previews the repair")
    verify(preview.command.indexOf("--dry-run") !== -1)
    answer(preview, "repair-preview")
    var sheet = planSheet()
    var lines = sheetLines(sheet)
    // Titled by the problem, an item per change, and the verb Repair, never urgent.
    verify(lines[0].indexOf("Repair: ") === 0 && lines[0].slice(-1) === "?", lines[0])
    verify(lines[1].indexOf("chmod 0600 ") === 0, lines[1])
    verify(lines[1].indexOf("credentials/github (was 0644)") !== -1, lines[1])
    compare(findChild(sheet, "sheetConfirm").text, "Repair")
    verify(!sheet.destructive)
    compare(sheet.selectedIndex, 0, "the sheet starts on Cancel")
    compare(repairs("--if-match").length, 0, "nothing ran before Yes")
    keyClick("y")
    var bound = lastRun("doctor")
    verify(bound !== preview, "Yes runs the repair: " + launchLog())
    compare(bound.command[bound.command.indexOf("--if-match") + 1], fixture("repair-preview").precondition)
    verify(bound.command.indexOf("--dry-run") === -1)
    answer(bound, "repair-run")
    verify(findChild(frame, "resultLine").text.indexOf("Repaired: chmod 0600 ") === 0, findChild(frame, "resultLine").text)
    verify(!findChild(frame, "resultUndo").visible, "no Undo before status names the repair")
    // The status the repair's change rang for names it as the last change.
    settle("repair-status-after")
    tryVerify(function() { return findChild(frame, "resultUndo").visible }, 3000, "the result's Undo once status names the repair")
    compare(repairRow(credentialFm), -1, "the credential row is gone")
    findChild(frame, "resultUndo").clicked()
    var undo = lastRun("undo")
    compare(words(undo.command), fixture("repair-status-after").data.last_change.undo.argv.slice(1))
    verify(undo.command.indexOf("--dry-run") !== -1, "the undo previews first")
    answer(undo, "repair-undo-preview")
    sheet = planSheet()
    lines = sheetLines(sheet)
    verify(lines[0].indexOf("Undo the repair from ") === 0, lines[0])
    verify(lines[1].indexOf("(readable by other users again)") !== -1, lines[1])
    verify(sheet.destructive)
    keyClick("y")
    var apply = lastRun("apply")
    compare(words(apply.command), ["apply", fixture("repair-undo-preview").data.plan_id], "Yes applies the undo's plan")
    answer(apply, "repair-undo-apply")
    compare(findChild(frame, "resultLine").text, "Undid repair (1 change)")
    // Across the whole scenario: one of each, and never the doctor's own undo.
    compare([repairs("--dry-run").length, repairs("--if-match").length, runs("undo").length, runs("apply").length], [1, 1, 1, 1], launchLog())
    compare(runs("doctor").filter(function(l) { return words(l.command)[1] === "undo" }).length, 0, launchLog())
  }

  function test_a_set_aside_row_offers_no_fix_and_shows_its_explanation_and_command() {
    start("setaside-status")
    openFrame()
    showIssues("setaside-status")
    var row = findChild(content(), "issueRow_0")
    var issue = fixture("setaside-status").data.targets.filter(function(t) { return t.id === "retail" })[0].issues[0]
    compare(issue.id, setAsideFm)
    verify(findChild(row, "consentNote").visible)
    compare(findChild(row, "commandText").text, issue.remediation.command, "the exact command, --force --yes included")
    compare(findChild(row, "enterHint").text, "Enter explains it")
    keyClick(Qt.Key_Return)
    var explain = lastRun("doctor")
    compare(words(explain.command), ["doctor", "explain", setAsideFm, "-t", "retail"])
    verify(explain.command.indexOf("--progress") === -1, "a read")
    answer(explain, "setaside-explain")
    var lines = findChild(row, "explanationLines")
    tryVerify(function() { return lines.count > 0 }, 3000, "the explanation's lines")
    compare(lines.itemAt(0).text, fixture("setaside-explain").failure_mode.title)
    var terminal = false
    for (var i = 0; i < lines.count; i++) if (lines.itemAt(i).text.indexOf("  In a terminal: ") === 0) terminal = true
    verify(terminal, "a line with the command for a terminal")
    keyClick(Qt.Key_Return)
    verify(!findChild(row, "explanation").visible, "Enter again closes it")
    compare(runs("doctor").filter(function(l) { return words(l.command)[1] === "fix" }).length, 0, "no repair from a forced row: " + launchLog())
  }

  function test_undo_last_reverts_a_repair_through_its_stored_plan() {
    start("repair-status-after")
    openFrame()
    compare(findChild(frame, "undoButton").tooltipText, "Undo repair (Ctrl+Z)")
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    var undo = lastRun("undo")
    verify(undo !== null && undo.command.indexOf("--dry-run") !== -1, "Undo last previews first: " + launchLog())
    compare(words(undo.command), ["undo", "--run", fixture("repair-status-after").data.last_change.run])
    answer(undo, "repair-undo-preview")
    var sheet = planSheet()
    verify(sheetLines(sheet)[0].indexOf("Undo the repair from ") === 0, sheetLines(sheet)[0])
    compare(findChild(sheet, "sheetConfirm").text, String.fromCodePoint(0xF054C) + " Undo")
    keyClick("y")
    var apply = lastRun("apply")
    compare(words(apply.command), ["apply", fixture("repair-undo-preview").data.plan_id])
    answer(apply, "repair-undo-apply")
    compare(findChild(frame, "resultLine").text, "Undid repair (1 change)")
    compare(runs("undo").length, 1, "the undo runs only through its plan")
  }

  function test_a_credential_fix_previews_then_runs_bound_to_its_token() {
    start("repair-status")
    var preview = previewCredential()
    compare(words(preview.command), ["doctor", "fix", "--only=" + credentialFm])
    answer(preview, "repair-preview")
    compare(JSON.parse(service.stateJson()).lastPreview, { "resource": "doctor_fix", "decision": "dialog", "planId": null })
    compare(service.lastError, null, "a dry run that plans is no failure")
    compare(service.confirmYes(), "")
    var bound = lastRun("doctor")
    compare(bound.command[bound.command.indexOf("--if-match") + 1], fixture("repair-preview").precondition, "the preview's token, byte for byte")
    compare(words(bound.command), words(preview.command), "the same repair")
  }

  function test_a_stale_credential_fix_previews_again_and_runs_nothing_twice() {
    start("repair-status")
    var bound = confirmCredential()
    answer(bound, "repair-stale")
    var again = lastRun("doctor")
    verify(again !== bound && again.command.indexOf("--dry-run") !== -1, "previewed again: " + launchLog())
    answer(again, "repair-preview")
    verify(service.confirm !== null, "the dialog is back")
    verify(service.confirm.stale.indexOf("Changed since you looked: the files this repair checks") === 0, service.confirm.stale)
    compare(repairs("--if-match").length, 1, "the bound run is never launched twice: " + launchLog())
  }

  function test_a_credential_fix_finished_after_the_panel_closed_notifies_once() {
    start("repair-status")
    service.setPanelOpen("p", true)
    settle("repair-status")
    var toasts = function() {
      return Quickshell.detachedCommands.filter(function(c) { return c[0] === "omarchy-notification-send" })
    }
    var bound = confirmCredential()
    compare(toasts().length, 0, "the preview raises none")
    service.setPanelOpen("p", false)
    answer(bound, "repair-run")
    compare(toasts().length, 1, JSON.stringify(Quickshell.detachedCommands))
    verify(toasts()[0].some(function(arg) { return String(arg).indexOf("Repaired: chmod 0600 ") === 0 }), JSON.stringify(toasts()[0]))
  }
}
