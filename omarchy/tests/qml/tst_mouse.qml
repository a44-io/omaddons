import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import qs.Commons
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "Fixtures.js" as Fixtures

// The mouse (1n2p.15, design doc §4) on the real Service and frame, over envelopes the
// binary wrote: the pointer moving over a row puts the cursor on it, a row moving under a
// still pointer (a key, a scroll) never takes it, one click opens a row as Enter does in
// every list and on a page, and the cursored row's action buttons run their verb with
// one click, never the row's.
TestCase {
  id: testCase
  name: "Mouse"
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
    PanelView {}
  }

  function begin(answers, height) {
    ProcessRegistry.reset()
    service = serviceComponent.createObject(testCase, { "bellDebounceMs": 10, "localDeadlineMs": 600000,
      "networkDeadlineMs": 600000, "killGraceMs": 60, "streamGraceMs": 30 })
    verify(service !== null)
    frame = frameComponent.createObject(testCase, { "svc": service, "width": 540, "height": height || 720 })
    verify(frame !== null)
    service.setPanelOpen("mouse", true)
    frame.opened = true
    settle(answers)
    frame.setFilter("")
    frame.focusItem.forceActiveFocus()
    tryVerify(function() { return frame.focusItem.activeFocus })
  }

  function cleanup() {
    // A launch's `started` arrives a turn later (the Process stub): let it, first.
    wait(0)
    if (frame) frame.destroy()
    if (service) service.destroy()
    frame = null
    service = null
    ProcessRegistry.reset()
    wait(0)
  }

  // ---- the mock binary (tst_journeys' harness)

  function words(command) {
    var i = 1
    while (i < command.length) {
      if (command[i] === "--json" || command[i] === "--dry-run") i += 1
      else if (["--actor", "--reason", "--progress", "--if-match"].indexOf(command[i]) !== -1) i += 2
      else break
    }
    return command.slice(i)
  }

  function pending() {
    var out = []
    for (var i = 0; i < ProcessRegistry.launches.length; i++) {
      var launch = ProcessRegistry.launches[i]
      if (launch.command[1] !== "--json" || !launch.process || launch.process.running !== true) continue
      out.push(launch)
    }
    return out
  }

  function runningOf(word) {
    var all = pending()
    for (var i = all.length - 1; i >= 0; i--) if (words(all[i].command)[0] === word) return all[i]
    return null
  }

  function the(name) { return { "text": Fixtures.text(name), "code": 0 } }

  function envelope(command, data) {
    return { "text": JSON.stringify({ "ok": true, "schema_version": "1.1", "tool_version": Build.TOOL_VERSION, "command": command,
      "exit_code": 0, "partial": false, "data": data, "warnings": [], "commands": [] }) }
  }

  function quiet(status) {
    return { "status": the(status), "status_full": the(status), "list": envelope("list", { "targets": [] }),
      "errors": envelope("errors", { "targets": [] }), "backup": envelope("backup list", { "targets": [] }),
      "catalog": envelope("search", { "results": [], "sources_skipped": [] }), "history": the("history-empty"),
      "auto": envelope("backup create", { "dry_run": false, "run": null, "targets": [] }) }
  }

  function settle(answers) {
    for (var round = 0; round < 40; round++) {
      var answered = 0
      var running = pending()
      for (var i = 0; i < running.length; i++) {
        var w = words(running[i].command)
        var key = w[0] === "status" ? (w.indexOf("--cached") !== -1 ? "status" : "status_full")
          : w[0] === "backup" && w[1] === "create" && w.indexOf("--auto") !== -1 ? "auto"
          : w[0] === "search" && w.indexOf("--catalog") !== -1 ? "catalog" : w[0]
        if (!answers.hasOwnProperty(key)) continue
        running[i].process.finish(answers[key].code || 0, answers[key].text, "streamFirst")
        answered++
      }
      wait(answered ? 5 : 15)
      if (!answered && round > 2) break
    }
  }

  function child(name, from) {
    var found = findChild(from || frame, name)
    verify(found !== null, "no " + name)
    return found
  }

  // Every item laid out where it shows (no positioner's layout pending), so a pointer
  // reaches what the player would see; at once when nothing is pending.
  function laidOut() {
    verify(waitForPolish(testCase.Window.window), "the window's layout settles")
  }

  // A row's own target: the AddonRow in a view's delegate (under its day or section
  // heading), or the delegate itself when it is its own row (Issues).
  function addonRowIn(item) {
    if (!item) return null
    if (item.shownActions !== undefined && item.activated !== undefined) return item
    for (var i = 0; i < item.children.length; i++) {
      var found = addonRowIn(item.children[i])
      if (found) return found
    }
    return null
  }

  function rowTarget(i) {
    var item = frame.currentView.rowItem(i)
    verify(item !== null, "row " + i + " is built")
    return addonRowIn(item) || item
  }

  // `view` shown and read, over `answers`.
  function show(view, answers) {
    frame.showView(view)
    if (view === "errors" || view === "backups") service.readAgain(view)
    settle(answers)
    tryVerify(function() { return frame.currentView && frame.currentView.rowCount > 0 }, 2000, view + " has rows")
    return frame.currentView
  }

  // ---- one click opens

  function test_one_click_opens_a_row_in_every_list_data() {
    var updates = quiet("status-updates")
    updates.list = the("plan-list-after")
    var errors = quiet("status-errors")
    errors.errors = the("errors")
    var history = quiet("status-updates")
    history.history = the("history")
    var backups = quiet("status-updates")
    backups.backup = the("backups")
    return [
      { "tag": "updates", "view": "updates", "answers": updates },
      { "tag": "installed", "view": "installed", "answers": updates },
      { "tag": "errors", "view": "errors", "answers": errors },
      { "tag": "history", "view": "history", "answers": history },
      { "tag": "backups", "view": "backups", "answers": backups },
    ]
  }

  function test_one_click_opens_a_row_in_every_list(data) {
    begin(data.answers)
    show(data.view, data.answers)
    compare(frame.topPage, null)
    var row = rowTarget(0)
    mouseClick(row, Math.min(40, row.width / 4), Math.min(12, row.height / 2))
    verify(frame.topPage !== null, "one click opened the row's page in " + data.view)
    // Back, on the row the click opened (the frame keeps it under the page, §2.4).
    keyClick(Qt.Key_Escape)
    compare(frame.topPage, null)
    compare(frame.cursor, 0)
    verify(frame.cursorActive)
  }

  // Issues: a row's click is its Enter, the fix previewed first, else its explanation.
  function test_one_click_on_a_problem_runs_its_enter() {
    var answers = quiet("status-attention")
    begin(answers)
    frame.showView("issues")
    settle(answers)
    var view = frame.currentView
    tryVerify(function() { return view.rowCount > 0 })
    var row = view.rows[0]
    var before = ProcessRegistry.launches.length
    mouseClick(rowTarget(0), 4, 6)
    if (row.primary === "explain") verify(frame.topPage !== null, "its explanation")
    else {
      compare(row.primary, "fix")
      tryVerify(function() { return ProcessRegistry.launches.length > before }, 1000, "its fix runs")
      verify(ProcessRegistry.launches[ProcessRegistry.launches.length - 1].command.indexOf("--dry-run") !== -1, "previewed first")
    }
  }

  // A page's lines open as a list's rows do: the change's addon, by one click.
  function test_one_click_opens_a_line_on_a_page() {
    var answers = quiet("status-updates")
    answers.history = the("history")
    begin(answers)
    var history = show("history", answers)
    // The first change whose page has an addon's line (a restore's has none).
    var page = null
    var at = -1
    for (var r = 0; r < history.rows.length && at < 0; r++) {
      frame.cursor = r
      frame.cursorActive = true
      keyClick(Qt.Key_Return)
      verify(frame.topPage !== null, "the change's page")
      page = frame.current
      for (var i = 0; i < page.rows.length && at < 0; i++) if (page.rows[i].addon) at = i
      if (at < 0) keyClick(Qt.Key_Escape)
    }
    verify(at >= 0, "a change with an addon's line")
    compare(frame.pages.length, 1)
    laidOut()
    var line = child("itemLine_" + at, page)
    mouseClick(line, 30, line.height / 2)
    compare(frame.pages.length, 2, "one click opened the addon's page")
    compare(frame.topPage.title, page.rows[at].addon)
  }

  // ---- hover

  function test_hover_moves_the_cursor_only_when_the_pointer_moves() {
    var answers = quiet("status-updates")
    answers.list = the("plan-list-after")
    begin(answers)
    show("updates", answers)
    var second = rowTarget(1)
    mouseMove(second, 40, 10)
    mouseMove(second, 50, 10)
    compare(frame.cursor, 1, "the pointer moved over the second row")
    verify(frame.cursorActive)
    keyClick("j")
    compare(frame.cursor, 2)
    // The same pointer, still: the keys keep the cursor.
    mouseMove(second, 50, 10)
    compare(frame.cursor, 2, "a still pointer does not take the cursor back")
    var first = rowTarget(0)
    mouseMove(first, 40, 10)
    mouseMove(first, 50, 10)
    compare(frame.cursor, 0, "a moving pointer does")
    compare(frame.topPage, null, "hovering opens nothing")
  }

  function test_a_list_scrolled_under_a_still_pointer_keeps_the_cursor() {
    var answers = quiet("status-updates")
    answers.list = the("plan-list-after")
    // Short enough that the rows scroll.
    begin(answers, 330)
    show("updates", answers)
    var flick = child("contentFlick")
    tryVerify(function() { return flick.contentHeight > flick.height }, 1000, "the content overflows")
    var first = rowTarget(0)
    var at = first.mapToItem(testCase, 40, 6)
    mouseMove(testCase, at.x - 6, at.y)
    mouseMove(testCase, at.x, at.y)
    compare(frame.cursor, 0)
    var top = flick.contentY
    // Two ticks: the Flickable scrolls by the time between them.
    mouseWheel(testCase, at.x, at.y, 0, -120)
    wait(50)
    mouseWheel(testCase, at.x, at.y, 0, -120)
    tryVerify(function() { return flick.contentY > top && !flick.moving }, 2000, "the wheel scrolled the rows")
    // Another row is under the pointer now, which has not moved.
    mouseMove(testCase, at.x, at.y)
    compare(frame.cursor, 0, "rows moving under a still pointer do not take the cursor")
  }

  // ---- the cursored row's actions

  function test_the_cursored_rows_action_runs_its_verb_on_one_click() {
    var answers = quiet("status-updates")
    answers.list = the("plan-list-after")
    begin(answers)
    show("updates", answers)
    var row = rowTarget(0)
    // The pointer puts the cursor on the row; its buttons show.
    mouseMove(row, 30, 10)
    mouseMove(row, 40, 10)
    compare(frame.cursor, 0)
    tryVerify(function() { return row.shownActions.length > 0 }, 1000, "the cursored row shows its actions")
    laidOut()
    var name = row.shownActions[0].name
    compare(name, "update")
    var button = child("action_update", row)
    verify(String(button.tooltipText).indexOf("(u)") !== -1, "its key in the tooltip: " + button.tooltipText)
    mouseClick(button, button.width / 2, button.height / 2)
    var update = null
    tryVerify(function() { update = runningOf("update"); return update !== null }, 1000)
    verify(update.command.indexOf("--dry-run") !== -1, "previewed first")
    compare(frame.topPage, null, "the button's click is not the row's")
  }

  // ---- Search's chooser

  function test_searchs_chooser_follows_the_pointer_and_installs_on_one_click() {
    var answers = quiet("status-updates")
    answers.search = the("search-catalog")
    begin(answers)
    frame.showView("search")
    settle(answers)
    keyClick("s")
    var search = frame.currentView
    var field = child("searchField", search)
    tryVerify(function() { return field.activeFocus })
    field.text = "advanced"
    keyClick(Qt.Key_Return)
    settle(answers)
    tryVerify(function() { return search.rowCount > 0 })
    var at = -1
    for (var i = 0; i < search.rows.length && at < 0; i++)
      if (search.rows[i].targets.filter(function(t) { return t.eligible }).length >= 2) at = i
    verify(at >= 0)
    frame.cursor = at
    frame.cursorActive = true
    keyClick("i")
    verify(search.chooserOpen)
    // Its lines laid out where they show, before the pointer reaches for them.
    laidOut()
    var choices = child("chooserTargets", search)
    var line = choices.itemAt(2)
    mouseMove(line, 30, line.height / 2)
    mouseMove(line, 40, line.height / 2)
    compare(search.chooserCursor, 2, "the pointer moved over the second client")
    keyClick("k")
    compare(search.chooserCursor, 1)
    mouseMove(line, 40, line.height / 2)
    compare(search.chooserCursor, 1, "a still pointer does not take it back")
    var target = search.chooserChoices[2].id
    mouseClick(line, 40, line.height / 2)
    var install = null
    tryVerify(function() { install = runningOf("install"); return install !== null }, 1000)
    var argv = words(install.command)
    compare(argv[argv.indexOf("-t") + 1], target, "into the client clicked: " + JSON.stringify(argv))
    verify(install.command.indexOf("--dry-run") !== -1, "previewed first")
    verify(!search.chooserOpen)
  }
}
