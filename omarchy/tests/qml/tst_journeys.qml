import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import qs.Commons
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "Fixtures.js" as Fixtures

// Five player journeys through the rebuilt widget end to end (1n2p.41, design doc §1.1),
// on the real Service and frame over envelopes the binary wrote (pipeline step 1), by
// keys and by mouse, at 540 and at 400 px wide, and by keys again with the text zoomed.
// Each step asserts what the player sees, in the copy table's words, and what the
// service ran; no step shows an omaddons command line.
//
//   J1 before a raid: Update all, applied as the rows showed it, with its Undo.
//   J2 after a session: opened on Errors, the culprit's page, its report copied, the
//      errors marked as seen.
//   J3 find and install: Search, a result's verdict, Every client it works on, one plan
//      sheet naming the dependency it adds, each client's plan applied in turn.
//   J4 undo yesterday: History, the change's page, its undo previewed and applied.
//   J5 the first run: the welcome card, a client added by its folder, its addons taken
//      over, BugGrabber offered.
//
// By mouse (omad-y4f8, after the mouse pass .15), every step clicks the item where it
// shows (a tab, a row, its button, a page's button, a card's, the chooser's line, the
// sheet's confirm), never a key and never a signal; typing a name or a path stays typing.
TestCase {
  id: testCase
  name: "Journeys"
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
      height: 720
    }
  }

  // The text zoom the next journey runs at (1.5 for the zoomed passes).
  property real zoom: 1.0

  // A fresh service and frame for each journey, `width` wide at the text zoom `zoom`,
  // its first status answered with `answers`.
  function begin(width, answers) {
    ProcessRegistry.reset()
    Quickshell.resetDetachedCommands()
    service = serviceComponent.createObject(testCase, { "bellDebounceMs": 10, "localDeadlineMs": 600000,
      "networkDeadlineMs": 600000, "killGraceMs": 60, "streamGraceMs": 30 })
    verify(service !== null)
    frame = frameComponent.createObject(testCase, { "svc": service, "width": width, "uiScale": zoom })
    verify(frame !== null)
    service.setPanelOpen("journeys", true)
    frame.opened = true
    settle(answers)
    frame.setFilter("")
    frame.focusItem.forceActiveFocus()
    tryVerify(function() { return frame.focusItem.activeFocus })
  }

  function cleanup() {
    if (frame) frame.destroy()
    if (service) service.destroy()
    frame = null
    service = null
    zoom = 1.0
    ProcessRegistry.reset()
    wait(0)
  }

  // Each journey at 400 px with the text zoomed to 1.5 (design doc §5.6).
  function test_every_journey_at_400_with_the_text_zoomed() {
    var journeys = [function() { journeyBeforeRaid(400, false) }, function() { journeyAfterSession(400, false) },
      function() { journeyFindAndInstall(400, false) }, function() { journeyUndoYesterday(400, false) },
      function() { journeyFirstRun(400, false) }]
    for (var i = 0; i < journeys.length; i++) {
      cleanup()
      zoom = 1.5
      journeys[i]()
    }
  }

  // ---- the mock binary

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

  function launchLog() {
    return JSON.stringify(ProcessRegistry.launches.map(function(l) { return words(l.command) }))
  }

  // Every argv the service ran, for "no step shows an omaddons command line".
  function ran(word) {
    return ProcessRegistry.launches.filter(function(l) { return l.command[1] === "--json" && words(l.command)[0] === word })
  }

  function the(name, code) { return { "text": Fixtures.text(name), "code": code || 0 } }

  function envelope(command, data) {
    return { "text": JSON.stringify({ "ok": true, "schema_version": "1.1", "tool_version": Build.TOOL_VERSION, "command": command,
      "exit_code": 0, "partial": false, "data": data, "warnings": [], "commands": [] }) }
  }

  // The reads a journey does not look at, answered with nothing to say.
  function quiet(status) {
    return { "status": the(status), "status_full": the(status), "list": envelope("list", { "targets": [] }),
      "errors": envelope("errors", { "targets": [] }), "backup": envelope("backup list", { "targets": [] }),
      "catalog": envelope("search", { "results": [], "sources_skipped": [] }), "history": the("history-empty"),
      "auto": envelope("backup create", { "dry_run": false, "run": null, "targets": [] }) }
  }

  // Answers every running read from `answers` (status with or without its scan, the
  // after-play checkpoint, the catalog apart from a search) until none is left; a
  // change (a preview, an apply) is answered by the journey itself.
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

  // A change's launch (a preview or an apply), answered with the binary's fixture.
  function answerChange(word, name, code) {
    var launch = null
    tryVerify(function() { launch = runningOf(word); return launch !== null }, 2000, "a " + word + " runs: " + launchLog())
    launch.process.finish(code || 0, Fixtures.text(name), "streamFirst")
    wait(10)
    return launch
  }

  function child(name, from) {
    var found = findChild(from || frame, name)
    verify(found !== null, "no " + name)
    return found
  }

  // What the player sees never holds a command line.
  function noCommandLine(text) {
    verify(String(text).indexOf("omaddons ") === -1 && String(text).indexOf("--") === -1, "a command line shown: " + text)
  }

  function sheet() {
    tryVerify(function() { return !!service.confirm && !!service.confirm.sheet }, 2000, "the plan sheet opens: " + launchLog())
    return service.confirm.sheet
  }

  // ---- the mouse (omad-y4f8): every step clicks the item where it shows, never its signal

  // Every item laid out where it shows (no positioner's layout pending), so a pointer
  // reaches what the player would see; at once when nothing is pending.
  function laidOut() {
    verify(waitForPolish(testCase.Window.window), "the window's layout settles")
  }

  // A click on `item` at its centre, once the frame has laid it out.
  function click(item) {
    laidOut()
    verify(item !== null && item.visible, "visible to click: " + (item ? item.objectName : "nothing"))
    mouseClick(item, item.width / 2, item.height / 2)
  }

  // The pointer moved over `item` (twice: the first sample only primes the gate), so the
  // cursor follows it.
  function pointAt(item) {
    laidOut()
    mouseMove(item, item.width / 2 - 6, item.height / 2)
    mouseMove(item, item.width / 2, item.height / 2)
  }

  // Row `i`'s own target: the AddonRow in its delegate.
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

  function confirmSheet(mouse) {
    var planSheet = child("planSheet")
    if (mouse) click(child("sheetConfirm", planSheet))
    else {
      tryVerify(function() { return planSheet.keyItem.activeFocus })
      keyClick("y")
    }
  }

  // The result in the corner toast (omad-92d5).
  function resultLine() {
    tryVerify(function() { return !!frame.toastView && frame.toastView.kind === "result" }, 2000, "a result shows")
    verify(child("toast").showing)
    var line = child("toastLine").text
    noCommandLine(line)
    return line
  }

  // ---- J1 before a raid: Update all

  function journeyBeforeRaid(width, mouse) {
    var answers = quiet("status-updates")
    answers.list = the("plan-list-after")
    begin(width, answers)
    keyClick("1")
    settle(answers)
    var updates = frame.currentView
    compare(updates.rowCount, 3)
    // Two clients behind: the Updates tab counts them (from `status`), and Update all is
    // the one click.
    verify(child("tab_updates").modelData.count > 0)
    var button = child("updateAll", updates)
    compare(button.text, "Update all (3)")
    if (mouse) click(button)
    else keyClick("U")
    // The plan is exactly what the rows showed: applied with no sheet (M4-D28).
    var preview = answerChange("update", "plan-update-all")
    verify(preview.command.indexOf("--dry-run") !== -1, "a preview first")
    compare(service.confirm, null, "no sheet: every row matches")
    var apply = answerChange("apply", "plan-update-all-apply")
    compare(words(apply.command), ["apply", JSON.parse(Fixtures.text("plan-update-all")).data.plan_id])
    compare(resultLine(), "Updated 3 addons on 2 clients")
    settle(quiet("status-after-update"))
    // Its Undo: the change it just made, previewed when pressed.
    verify(child("toastUndo").visible, "the result offers its Undo")
  }

  function test_j1_before_a_raid_by_keys_540() { journeyBeforeRaid(540, false) }
  function test_j1_before_a_raid_by_keys_400() { journeyBeforeRaid(400, false) }
  function test_j1_before_a_raid_by_mouse_540() { journeyBeforeRaid(540, true) }
  function test_j1_before_a_raid_by_mouse_400() { journeyBeforeRaid(400, true) }

  // ---- J2 after a session: the errors

  function journeyAfterSession(width, mouse) {
    var answers = quiet("status-errors")
    answers.errors = the("errors")
    begin(width, answers)
    // A middle click opens on the most urgent view: Errors, with new Lua errors.
    compare(Model.urgentView(service.summary), "errors")
    frame.showView(Model.urgentView(service.summary))
    settle(answers)
    var errors = frame.currentView
    verify(errors.rowCount > 0, "the culprit's row")
    var row = errors.rows[0]
    compare(row.addon, "Foo")
    if (mouse) click(rowTarget(0))
    else {
      keyClick("j")
      keyClick("k")
      keyClick(Qt.Key_Return)
    }
    verify(frame.topPage !== null, "the error page opens")
    var page = frame.current
    noCommandLine(child("pageTitle", page).text)
    // `y` or Copy report: the report on the clipboard, one argument, never run.
    if (mouse) click(child("pageCopy", page))
    else keyClick("y")
    // Its own wl-copy process, whose exit says whether it was copied (omad-phcg).
    var copied = ProcessRegistry.launches.filter(function(l) { return l.command[0] === "wl-copy" })
    compare(copied.length, 1)
    compare(copied[0].command.slice(0, 2), ["wl-copy", "--"])
    verify(copied[0].command[2].indexOf("Foo") !== -1, "the report names the addon")
    copied[0].process.exit(0)
    // The flash is the corner toast's (omad-92d5).
    tryCompare(child("toastLine"), "text", "Copied the report on Foo", 1000)
    verify(child("toast").showing)
    if (mouse) click(child("backButton"))
    else keyClick(Qt.Key_Escape)
    compare(frame.topPage, null)
    // `d` or Dismiss: the new errors marked as seen, bound to the read the panel showed.
    if (mouse) click(child("dismiss", frame.currentView))
    else keyClick("d")
    var dismiss = answerChange("errors", "dismiss")
    compare(words(dismiss.command).slice(0, 2), ["errors", "dismiss"])
    verify(dismiss.command.indexOf("--if-match") !== -1, "bound to the read it showed")
    resultLine()
  }

  function test_j2_after_a_session_by_keys_540() { journeyAfterSession(540, false) }
  function test_j2_after_a_session_by_keys_400() { journeyAfterSession(400, false) }
  function test_j2_after_a_session_by_mouse_540() { journeyAfterSession(540, true) }
  function test_j2_after_a_session_by_mouse_400() { journeyAfterSession(400, true) }

  // ---- J3 find and install on every client it works on

  function journeyFindAndInstall(width, mouse) {
    var answers = quiet("status-updates")
    // The binary's catalog answer: its entries carry both clients' hints, so one works
    // on Retail and on Classic Era.
    answers.search = the("search-catalog")
    begin(width, answers)
    if (mouse) click(child("tab_search"))
    else keyClick("3")
    settle(answers)
    var field = child("searchField", frame.currentView)
    if (mouse) click(field)
    else keyClick("s")
    tryVerify(function() { return field.activeFocus })
    field.text = "advanced"
    // By keys, Enter searches now; by mouse, the search runs as the name is typed.
    if (!mouse) keyClick(Qt.Key_Return)
    tryVerify(function() { return ran("search").some(function(l) { return words(l.command).indexOf("advanced") !== -1 }) }, 2000,
      "the search runs: " + launchLog())
    settle(answers)
    var search = frame.currentView
    tryVerify(function() { return search.rowCount > 0 }, 2000, "results: " + launchLog())
    var first = search.rows[0]
    noCommandLine(first.meta)
    // Every client it works on: each client's preview in turn, then one sheet.
    var row = null
    var at = -1
    for (var i = 0; i < search.rows.length && at < 0; i++) {
      if (search.rows[i].targets.filter(function(t) { return t.eligible }).length >= 2) {
        row = search.rows[i]
        at = i
      }
    }
    verify(row !== null, "a result that works on two clients")
    if (mouse) {
      // The pointer on its row shows its buttons; Install opens the chooser.
      var target = rowTarget(at)
      pointAt(target)
      compare(frame.cursor, at)
      click(child("action_install", target))
    } else {
      frame.cursor = at
      frame.cursorActive = true
      keyClick("i")
    }
    verify(search.chooserOpen, "under All, the chooser")
    compare(search.chooserChoices[0].words, Model.copy("search.chooser.every", { "n": 2 }))
    if (mouse) {
      laidOut()
      click(child("chooserTargets", search).itemAt(0))
    } else {
      keyClick("j")
      keyClick(Qt.Key_Return)
    }
    answerChange("install", "plan-install-two-retail")
    answerChange("install", "plan-install-two-era")
    var s = sheet()
    verify(s.title.indexOf("Install " + row.name + " on ") === 0, s.title)
    compare(s.items.map(function(it) { return it.header }).filter(function(h) { return h !== "" }).length, 2)
    compare(s.effects.map(function(e) { return e.text }), ["Also installs LibFoo 1.0, which Qux needs."])
    compare(ran("apply").length, 0, "nothing runs before Yes")
    confirmSheet(mouse)
    answerChange("apply", "plan-install-two-retail-apply")
    answerChange("apply", "plan-install-two-era-apply")
    var line = resultLine()
    verify(line.indexOf("Installed " + row.name + " on ") === 0, line)
  }

  function test_j3_find_and_install_by_keys_540() { journeyFindAndInstall(540, false) }
  function test_j3_find_and_install_by_keys_400() { journeyFindAndInstall(400, false) }
  function test_j3_find_and_install_by_mouse_540() { journeyFindAndInstall(540, true) }
  function test_j3_find_and_install_by_mouse_400() { journeyFindAndInstall(400, true) }

  // ---- J3 from an addon page (omad-z5il): a dependency nothing is known for

  // An addon page's Install it of the name a TOC requires (tst_views proves its click runs
  // exactly this) previews `install`; the binary answers not_found, whose fix searches.
  // The failure card offers Search for it as the fix and its other fix as an alternative,
  // never "Fix: omaddons search …"; Search for it opens Search on that name, the box
  // holding it and the keys, and searches it at once, the card gone.
  function searchForAnUnknownName(width) {
    var answers = quiet("status-updates")
    begin(width, answers)
    compare(service.run("install", { "target": "retail", "ref": "LibNowhere" }, "Install LibNowhere", []), "")
    answerChange("install", "install-unknown-name", 3)
    tryVerify(function() { return !!service.lastError }, 2000, "the preview fails: " + launchLog())
    compare([service.lastError.code, service.lastError.search, service.lastError.fixText], ["not_found", "LibNowhere", ""])
    var offer = child("searchForIt")
    laidOut()
    verify(offer.visible)
    compare(offer.text, "Search for it")
    compare(child("fix_0").text, "omaddons help install", "an alternative, not the Fix")
    verify(findChild(frame, "fix_1") === null, "the search command is no button of its own")
    compare(ran("search").length, 0, "nothing searched before the click")
    click(offer)
    tryCompare(frame, "view", "search")
    var field = child("searchField", frame.currentView)
    compare(field.text, "LibNowhere")
    tryVerify(function() { return field.activeFocus })
    tryVerify(function() { return ran("search").some(function(l) { return words(l.command).indexOf("LibNowhere") !== -1 }) }, 2000,
      "the search runs: " + launchLog())
    compare(ran("search").filter(function(l) { return words(l.command).indexOf("LibNowhere") !== -1 }).length, 1, "searched once")
    compare(service.lastError, null, "the card goes")
    compare(ran("install").length, 1, "only the preview ran")
  }

  function test_j3_search_for_an_unknown_dependency_540() { searchForAnUnknownName(540) }
  function test_j3_search_for_an_unknown_dependency_400() { searchForAnUnknownName(400) }

  // ---- J4 undo yesterday

  function journeyUndoYesterday(width, mouse) {
    var answers = quiet("status-updates")
    answers.history = the("history")
    begin(width, answers)
    if (mouse) click(child("tab_history"))
    else keyClick("7")
    settle(answers)
    var history = frame.currentView
    verify(history.rowCount > 0)
    var at = -1
    for (var i = 0; i < history.rows.length; i++) if (history.rows[i].undo) { at = i; break }
    verify(at >= 0, "a change that can be undone")
    noCommandLine(history.rows[at].title)
    if (mouse) click(rowTarget(at))
    else {
      frame.cursor = at
      frame.cursorActive = true
      keyClick(Qt.Key_Return)
    }
    verify(frame.topPage !== null, "the change's page")
    // Ctrl+Z or the page's Undo: the undo, previewed in the plan sheet.
    if (mouse) click(child("pageUndo_0", frame.current))
    else keySequence("Ctrl+Z")
    var preview = answerChange("undo", "history-undo-preview")
    verify(preview.command.indexOf("--dry-run") !== -1)
    compare(words(preview.command).slice(0, 1), ["undo"])
    var s = sheet()
    noCommandLine(s.title)
    compare(ran("apply").length, 0, "nothing runs before Yes")
    confirmSheet(mouse)
    answerChange("apply", "history-undo-apply")
    resultLine()
  }

  function test_j4_undo_yesterday_by_keys_540() { journeyUndoYesterday(540, false) }
  function test_j4_undo_yesterday_by_keys_400() { journeyUndoYesterday(400, false) }
  function test_j4_undo_yesterday_by_mouse_540() { journeyUndoYesterday(540, true) }
  function test_j4_undo_yesterday_by_mouse_400() { journeyUndoYesterday(400, true) }

  // ---- J5 the first run

  function welcome() {
    var item = frame.currentView
    if (!item) return null
    var notices = findChild(item, "stateNotices")
    if (!notices || !notices.visible) return null
    var card = findChild(item, "stateNotice")
    return card && card.visible && card.title === Model.copy("welcome.title") ? card : null
  }

  // A card's button by its words: clicked where it shows by mouse, its signal by keys.
  function press(card, text, mouse) {
    var buttons = card.buttons
    for (var i = 0; i < buttons.length; i++) {
      if (buttons[i].text === text) {
        var button = findChild(card, "noticeButton_" + i)
        if (mouse) click(button)
        else button.clicked()
        return
      }
    }
    fail("no button " + text + " on the card")
  }

  function journeyFirstRun(width, mouse) {
    begin(width, quiet("status-none"))
    var card = welcome()
    verify(card !== null, "the welcome card")
    noCommandLine(card.detail)
    press(card, "Add a client folder…", mouse)
    var page = frame.current
    var field = child("pathField", page)
    tryVerify(function() { return field.activeFocus })
    field.text = "~/pserver"
    if (mouse) click(child("addClient", page))
    else keyClick(Qt.Key_Return)
    var added = answerChange("targets", "targets-add")
    compare(words(added.command), ["targets", "add", "--", Build.CONTEXT.HOME + "/pserver"])
    compare(resultLine(), "Added Wrath 3.3.5a private server")
    settle(quiet("status-added"))
    tryVerify(function() { return frame.topPage === null }, 1000, "the page goes back once the client is here")
    // Its folders taken over (the first-run card, over the binary's take-over scene).
    var adoptable = quiet("status-adoptable")
    adoptable.list = the("list-adoptable")
    service.refresh()
    settle(adoptable)
    var onboarding = child("onboardingCard")
    tryVerify(function() { return onboarding.visible }, 1000)
    compare(onboarding.title, "Take over the addons already on Retail?")
    press(onboarding, onboarding.buttons[0].text, mouse)
    answerChange("adopt", "adopt-preview")
    compare(sheet().title, "Take over 2 addons on Retail?")
    confirmSheet(mouse)
    answerChange("apply", "adopt-apply")
    resultLine()
    // A client that records no Lua errors yet: BugGrabber offered, previewed first.
    var errors = quiet("status-errors")
    errors.errors = the("errors")
    service.refresh()
    settle(errors)
    // Its Lua errors read again, as a doorbell would have it: Classic Era records none.
    service.readAgain("errors")
    settle(errors)
    tryVerify(function() { return onboarding.visible && onboarding.title === "Record Lua errors on Classic Era?" }, 2000,
      "the BugGrabber card: " + onboarding.title)
    press(onboarding, onboarding.buttons[0].text, mouse)
    var install = null
    tryVerify(function() { install = runningOf("install"); return install !== null }, 2000)
    compare(words(install.command), ["install", "buggrabber", "-t", "era"])
    verify(install.command.indexOf("--dry-run") !== -1, "a preview, never the install itself")
  }

  function test_j5_the_first_run_540() { journeyFirstRun(540, false) }
  function test_j5_the_first_run_400() { journeyFirstRun(400, false) }
  function test_j5_the_first_run_by_mouse_540() { journeyFirstRun(540, true) }
  function test_j5_the_first_run_by_mouse_400() { journeyFirstRun(400, true) }
}
