import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import qs.Commons
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "Fixtures.js" as Fixtures

// History (1n2p.30) on the real Service and frame, over envelopes the binary wrote
// (pipeline step 1's history scene, tests/omarchy_widget.rs history_fixtures): `7`
// reads every change locally, with no actor (an explicit `--actor` filters `history`);
// the rows, `/` and Esc; Ctrl+Z on a cursored change previews `undo --run` in the plan
// sheet and Yes applies exactly that plan, for an update, a removal and a restore; a
// change already undone says why and runs nothing; Enter opens a change's page, whose
// buttons undo all of it or one client's part.
TestCase {
  id: testCase
  name: "History"
  when: windowShown
  visible: true
  width: 600
  height: 800

  property var service: null
  property var frame: null

  Component {
    id: serviceComponent
    Service {}
  }

  Component {
    id: frameComponent
    PanelView {
      width: 540
      height: 720
    }
  }

  function init() {
    ProcessRegistry.reset()
    Quickshell.resetDetachedCommands()
    service = serviceComponent.createObject(testCase, { "bellDebounceMs": 10, "localDeadlineMs": 600000,
      "networkDeadlineMs": 600000, "killGraceMs": 60, "streamGraceMs": 30 })
    verify(service !== null)
    frame = frameComponent.createObject(testCase, { "svc": service })
    verify(frame !== null)
    service.setPanelOpen("history", true)
    frame.opened = true
    settle(answers())
    compare(service.phase, "ok")
    frame.setFilter("")
    frame.focusItem.forceActiveFocus()
    tryVerify(function() { return frame.focusItem.activeFocus })
  }

  function cleanup() {
    if (frame) frame.destroy()
    if (service) service.destroy()
    frame = null
    service = null
    ProcessRegistry.reset()
    wait(0)
  }

  // ---- helpers

  // The command words after the standard flags, by name (History's read names no actor).
  function words(command) {
    var i = 1
    while (i < command.length) {
      if (command[i] === "--json" || command[i] === "--dry-run") i += 1
      else if (["--actor", "--reason", "--progress", "--if-match"].indexOf(command[i]) !== -1) i += 2
      else break
    }
    return command.slice(i)
  }

  function the(name) { return { "text": Fixtures.text(name), "code": 0 } }

  // What the reads answer: the everyday scene, and History's.
  function answers() {
    return { "status": the("status-updates"), "status_full": the("status-updates"), "list": the("list"), "errors": the("errors"),
      "backup": the("backups"), "history": the("history") }
  }

  // The running omaddons launches, oldest first.
  function pending() {
    var out = []
    for (var i = 0; i < ProcessRegistry.launches.length; i++) {
      var launch = ProcessRegistry.launches[i]
      if (launch.command[1] !== "--json" || !launch.process || launch.process.running !== true) continue
      out.push(launch)
    }
    return out
  }

  // Every launch whose words start with `word`, oldest first.
  function launched(word) {
    return ProcessRegistry.launches.filter(function(l) { return l.command[1] === "--json" && words(l.command)[0] === word })
  }

  function last(word) {
    var all = launched(word)
    return all.length ? all[all.length - 1] : null
  }

  function launchLog() {
    return JSON.stringify(ProcessRegistry.launches.map(function(l) { return words(l.command) }))
  }

  // Answers every running read from `answers` (by its first word; `status` with its
  // scan apart), until none is left to answer; a change (undo, apply) is answered by
  // the test itself.
  function settle(answers) {
    for (var round = 0; round < 40; round++) {
      var answered = 0
      var running = pending()
      for (var i = 0; i < running.length; i++) {
        var w = words(running[i].command)
        var key = w[0] === "status" ? (w.indexOf("--cached") !== -1 ? "status" : "status_full")
          : (w[0] === "backup" && w[1] === "create" ? "auto" : w[0])
        if (key === "auto") {
          running[i].process.finish(0, JSON.stringify({ "ok": true, "schema_version": "1.1", "tool_version": Build.TOOL_VERSION,
            "command": "backup create", "exit_code": 0, "partial": false, "data": { "dry_run": false, "run": null, "targets": [] },
            "warnings": [], "commands": [] }), "streamFirst")
          answered++
          continue
        }
        if (!answers.hasOwnProperty(key)) continue
        running[i].process.finish(answers[key].code || 0, answers[key].text, "streamFirst")
        answered++
      }
      wait(answered ? 5 : 15)
      if (!answered && round > 2) break
    }
  }

  // Half an hour after the scene's newest change, as model.test.js's `soon()`. The
  // fixture's times are moved to a fixed midday (history_fixtures), so the panel's own
  // clock would read its day as a date weeks ago.
  function soon() {
    return Date.parse(JSON.parse(Fixtures.text("history")).data.entries[0].at) + 30 * 60000
  }

  // History open, its rows read, the panel's clock half an hour after the scene.
  function openHistory() {
    keyClick("7")
    compare(frame.view, "history")
    settle(answers())
    frame.nowMs = soon()
    tryVerify(function() { return frame.currentView && frame.currentView.rowCount > 0 }, 2000, "History lists the scene's changes")
    return frame.currentView
  }

  function rowIndex(view, title) {
    for (var i = 0; i < view.rows.length; i++) if (view.rows[i].title === title) return i
    fail("no row " + title + " in " + JSON.stringify(view.rows.map(function(r) { return r.title })))
    return -1
  }

  // Ctrl+Z on the row titled `title`: its undo previewed (`undo --run=<run>`), the plan
  // sheet, then Yes applies exactly that plan; the result says what was undone.
  function undoesThrough(title, previewFixture, applyFixture) {
    var history = openHistory()
    var at = rowIndex(history, title)
    history.select(at)
    var run = history.rows[at].run
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    var preview = last("undo")
    verify(preview !== null, title + ": Ctrl+Z previews its undo: " + launchLog())
    verify(preview.command.indexOf("--dry-run") !== -1, title + ": a preview")
    compare(words(preview.command), ["undo", "--run=" + run], title)
    preview.process.finish(0, Fixtures.text(previewFixture), "streamFirst")
    tryVerify(function() { return frame.confirming }, 2000, title + ": the plan sheet opens")
    var sheet = findChild(frame, "planSheet")
    verify(Model.dialogText(sheet.plan).indexOf("Undo the ") === 0, Model.dialogText(sheet.plan))
    compare(launched("apply").length, 0, title + ": nothing runs before Yes")
    tryVerify(function() { return sheet.keyItem.activeFocus })
    keyClick("y")
    var apply = last("apply")
    verify(apply !== null, title + ": Yes applies the plan: " + launchLog())
    compare(words(apply.command), ["apply", JSON.parse(Fixtures.text(previewFixture)).data.plan_id], title)
    compare(launched("undo").length, 1, title + ": the undo runs only through its plan")
    apply.process.finish(0, Fixtures.text(applyFixture), "streamFirst")
    tryVerify(function() { return service.lastResult !== null }, 2000, title + ": a result")
    verify(service.lastResult.line.indexOf("Undid ") === 0, service.lastResult.line)
  }

  // ---- the tests

  function test_seven_reads_every_change_with_no_actor() {
    var history = openHistory()
    var read = last("history")
    compare(words(read.command), ["history", "-v", "--limit", String(Model.HISTORY_LIMIT)])
    compare(read.command.indexOf("--actor"), -1, "an explicit --actor would filter history to the panel's own changes")
    compare(history.rows[0].title, "Restored the settings")
    compare(history.rows[0].group, "Today")
    verify(history.rows.some(function(r) { return r.actorWords === "an agent" }), "an agent's changes are listed")
    verify(history.rows.some(function(r) { return r.actorWords === "you, in a terminal" }), "a terminal's changes are listed")
    compare(findChild(history, "historyRow_0") !== null, true)
  }

  function test_slash_filters_and_esc_clears() {
    var history = openHistory()
    var all = history.rowCount
    keyClick("/")
    tryVerify(function() { return history.typing })
    keyClick("e")
    keyClick("l")
    keyClick("v")
    keyClick("u")
    keyClick("i")
    tryCompare(history, "rowCount", 1)
    compare(history.rows[0].title, "Removed Bar")
    compare(findChild(history, "filterCount").text, "1 of " + all)
    keyClick(Qt.Key_Escape)
    verify(!history.typing, "Esc leaves the box, the filter kept")
    compare(history.rowCount, 1)
    keyClick(Qt.Key_Escape)
    compare(history.rowCount, all, "Esc again clears the filter")
  }

  function test_ctrl_z_undoes_an_update_through_its_plan() {
    undoesThrough("Updated 2 addons", "history-undo-preview", "history-undo-apply")
  }

  function test_ctrl_z_undoes_a_removal_through_its_plan() {
    undoesThrough("Removed Foo", "history-undo-remove-preview", "history-undo-remove-apply")
  }

  function test_ctrl_z_undoes_a_restore_through_its_plan() {
    undoesThrough("Restored the settings", "history-undo-restore-preview", "history-undo-restore-apply")
  }

  // A change already undone, and one undo never reverts: why, in words; nothing runs.
  function test_a_change_that_cannot_be_undone_says_why() {
    var history = openHistory()
    history.select(rowIndex(history, "Removed Bar"))
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    compare(frame.notice, "It was already undone.")
    history.select(rowIndex(history, "Changed a setting"))
    keyClick(Qt.Key_Z, Qt.ControlModifier)
    compare(frame.notice, "A settings change: undo doesn't revert it.")
    compare(launched("undo").length, 0, launchLog())
  }

  // Enter: the change's page, each client's addons; its buttons undo all of it or one
  // client's part; an addon line opens that addon's page; back returns to the row.
  function test_enter_opens_the_change_page() {
    var history = openHistory()
    var at = rowIndex(history, "Updated 2 addons")
    history.select(at)
    var run = history.rows[at].run
    keyClick(Qt.Key_Return)
    compare(frame.pages.length, 1)
    tryVerify(function() { return !!frame.current && frame.current !== history })
    var page = frame.current
    compare(page.changeKey, history.rows[at].key)
    compare(findChild(page, "pageTitle").text.indexOf("Updated 2 addons · "), 0)
    compare(page.page.undos.map(function(u) { return u.text }), ["Undo all of it", "Undo on Classic Era only", "Undo on Retail only"])
    // One client's part: `undo -t <client> --run=<run>`, previewed.
    findChild(page, "pageUndo_1").clicked()
    var preview = last("undo")
    verify(preview !== null && preview.command.indexOf("--dry-run") !== -1, launchLog())
    compare(words(preview.command), ["undo", "-t", "era", "--run=" + run])
    preview.process.finish(0, Fixtures.text("history-undo-target-preview"), "streamFirst")
    tryVerify(function() { return frame.confirming })
    var sheet = findChild(frame, "planSheet")
    verify(Model.dialogText(sheet.plan).indexOf("Undo the update on Classic Era from ") === 0)
    tryVerify(function() { return sheet.keyItem.activeFocus })
    keyClick(Qt.Key_Escape)
    tryVerify(function() { return !frame.confirming })
    compare(launched("apply").length, 0, "cancelled: nothing applied")
    frame.focusItem.forceActiveFocus()
    // An addon line: its page on top; back, and back again to the row.
    page.select(0)
    keyClick(Qt.Key_Return)
    compare(frame.pages.length, 2)
    compare(frame.pages[1].title, "Bar")
    keyClick("h")
    keyClick("h")
    compare(frame.pages.length, 0)
    compare(frame.cursor, at)
  }
}
