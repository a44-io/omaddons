import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import qs.Commons
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "Fixtures.js" as Fixtures

// The first run (1n2p.32) on the real Service and frame, over envelopes the binary
// wrote for a home where discovery finds no client (pipeline step 1's first-run scene):
// the welcome card in every view, Scan again, and a client added by its folder from the
// path box, with no terminal; the panel moves on once the scan lists it.
TestCase {
  id: testCase
  name: "Onboarding"
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
    service.setPanelOpen("onboarding", true)
    frame.opened = true
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

  function words(command) {
    var i = 6
    while (i < command.length) {
      if (command[i] === "--progress" || command[i] === "--if-match") i += 2
      else if (command[i] === "--dry-run") i += 1
      else break
    }
    return command.slice(i)
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

  function runningOf(word) {
    var all = pending()
    for (var i = all.length - 1; i >= 0; i--) if (words(all[i].command)[0] === word) return all[i]
    return null
  }

  // Answers every running read with `answers[word]` (an envelope's text), until none is
  // left to answer.
  function settle(answers) {
    for (var round = 0; round < 40; round++) {
      var answered = 0
      var running = pending()
      for (var i = 0; i < running.length; i++) {
        var w = words(running[i].command)
        var key = w[0] === "status" ? (w.indexOf("--cached") !== -1 ? "status" : "status_full") : w[0]
        if (!answers.hasOwnProperty(key)) continue
        running[i].process.finish(answers[key].code || 0, answers[key].text, "streamFirst")
        answered++
      }
      wait(answered ? 5 : 15)
      if (!answered && round > 2) break
    }
  }

  function the(name, code) { return { "text": Fixtures.text(name), "code": code || 0 } }

  // What the reads with nothing to say answer.
  function quiet() {
    var empty = function(command) {
      return { "text": JSON.stringify({ "ok": true, "schema_version": "1.1", "tool_version": Build.TOOL_VERSION, "command": command,
        "exit_code": 0, "partial": false, "data": { "targets": [] }, "warnings": [], "commands": [] }) }
    }
    var catalog = { "text": JSON.stringify({ "ok": true, "schema_version": "1.1", "tool_version": Build.TOOL_VERSION, "command": "search",
      "exit_code": 0, "partial": false, "data": { "results": [], "sources_skipped": [] }, "warnings": [], "commands": [] }) }
    return { "list": empty("list"), "errors": empty("errors"), "backup": empty("backup list"), "search": catalog }
  }

  // `status` with or without its scan (the panel opening asks for the scan) answer
  // `statusName`, unless `fullName` names the scan's own answer.
  function scene(statusName, fullName) {
    var answers = quiet()
    answers.status = the(statusName)
    answers.status_full = the(fullName || statusName)
    return answers
  }

  function child(name, from) {
    var found = findChild(from || frame, name)
    verify(found !== null, "no " + name)
    return found
  }

  // The view's welcome notice, or null.
  function welcome() {
    var item = frame.currentView
    if (!item) return null
    var notices = findChild(item, "stateNotices")
    if (!notices || !notices.visible) return null
    var card = findChild(item, "stateNotice")
    return card && card.visible && card.title === Model.copy("welcome.title") ? card : null
  }

  function press(card, text) {
    var buttons = card.buttons
    for (var i = 0; i < buttons.length; i++) {
      if (buttons[i].text === text) {
        findChild(card, "noticeButton_" + i).clicked()
        return
      }
    }
    fail("no button " + text + " on the card")
  }

  // ---- the tests

  function test_with_no_client_every_view_welcomes_and_scan_again_scans() {
    settle(scene("status-none"))
    compare(service.phase, "ok")
    for (var v = 0; v < Model.VIEWS.length; v++) {
      frame.showView(Model.VIEWS[v])
      wait(10)
      verify(welcome() !== null, Model.VIEWS[v] + " shows the welcome card")
    }
    frame.showView("updates")
    // The reads each view asked for, answered, so the lane is free for the scan.
    settle(scene("status-none"))
    press(welcome(), "Scan again")
    var scan = runningOf("status")
    verify(scan !== null && words(scan.command).indexOf("--cached") === -1, "Scan again runs status with its scan")
    // The scan finds the client: the panel moves on.
    settle(scene("status-added", "status-added"))
    compare(welcome(), null)
    compare(Model.liveTargets(service.targets).length, 1)
  }

  function test_a_client_is_added_by_its_folder_from_the_path_box_and_the_panel_moves_on() {
    settle(scene("status-none"))
    frame.showView("updates")
    wait(10)
    press(welcome(), "Add a client folder…")
    verify(frame.topPage !== null, "the page opens")
    var page = frame.current
    var field = child("pathField", page)
    tryVerify(function() { return field.activeFocus }, 1000, "the path box takes the keys")
    verify(frame.typing, "the frame leaves the keys to the box")
    // Not a full path: said, nothing runs.
    field.text = "Games/wow"
    keyClick(Qt.Key_Return)
    compare(child("addClientNote", page).text, "Type the whole path, from / or ~/.")
    compare(runningOf("targets"), null)
    // A folder that holds no client: the CLI says so in the failure card.
    field.text = "/srv/empty"
    keyClick(Qt.Key_Return)
    var refused = runningOf("targets")
    verify(refused !== null, "targets add runs")
    compare(words(refused.command), ["targets", "add", "--", "/srv/empty"])
    refused.process.finish(3, Fixtures.text("targets-add-none"), "streamFirst")
    wait(10)
    compare(service.lastError.code, "target_not_found")
    compare(child("addClientNote", page).text, "", "no longer adding")
    verify(frame.topPage !== null, "still on the page")
    // "~/" is the home; the client is added, the scan lists it, the page goes back.
    field.text = "~/pserver"
    child("addClient", page).clicked()
    var added = runningOf("targets")
    compare(words(added.command), ["targets", "add", "--", Build.CONTEXT.HOME + "/pserver"])
    var before = ProcessRegistry.launches.length
    added.process.finish(0, Fixtures.text("targets-add"), "streamFirst")
    wait(10)
    compare(service.lastResult.line, "Added Wrath 3.3.5a private server")
    settle(scene("status-added", "status-added"))
    var scanned = ProcessRegistry.launches.slice(before).some(function(l) {
      var w = words(l.command)
      return w[0] === "status" && w.indexOf("--cached") === -1
    })
    verify(scanned, "the add is followed by a scan")
    tryVerify(function() { return frame.topPage === null }, 1000, "the page goes back once a client is here")
    compare(welcome(), null)
  }

  // Take over the folders already on a client (1n2p.32), over the binary's envelopes:
  // Installed's Take over… previews `adopt --apply`, the plan sheet lists what it would
  // adopt and says nothing moves, and Yes applies exactly that plan.
  function test_take_over_previews_lists_and_applies_through_the_plan_sheet() {
    var answers = scene("status-adoptable")
    answers.list = the("list-adoptable")
    settle(answers)
    frame.showView("installed")
    settle(answers)
    var button = child("takeOver", frame.currentView)
    verify(button.visible, "Installed offers the take-over")
    button.clicked()
    var preview = runningOf("adopt")
    verify(preview !== null, "the take-over runs as its preview")
    compare(words(preview.command), ["adopt", "--apply", "-t", "retail"])
    verify(preview.command.indexOf("--dry-run") !== -1)
    preview.process.finish(0, Fixtures.text("adopt-preview"), "streamFirst")
    tryVerify(function() { return !!service.confirm && !!service.confirm.sheet }, 1000, "the plan sheet opens")
    var sheet = service.confirm.sheet
    compare(sheet.title, "Take over 2 addons on Retail?")
    compare(sheet.items.map(function(i) { return i.addon }), ["Bar", "Foo"])
    compare(sheet.effects[0].text, Model.copy("sheet.effect.adopt_in_place"))
    // Yes: exactly the stored plan.
    service.confirmYes()
    var apply = runningOf("apply")
    verify(apply !== null, "Yes applies the plan")
    compare(words(apply.command), ["apply", JSON.parse(Fixtures.text("adopt-preview")).data.plan_id])
    apply.process.finish(0, Fixtures.text("adopt-apply"), "streamFirst")
    var after = scene("status-adopted")
    after.list = the("list-adoptable")
    settle(after)
    compare(service.targets[0].addons, 2, "Foo and Bar are managed now")
  }

  // ---- the cards above the views (1n2p.32): one at a time, each with Not now

  function onboardingCard() { return child("onboardingCard") }

  function pressCard(index) {
    findChild(onboardingCard(), "noticeButton_" + index).clicked()
  }

  function test_take_over_comes_first_and_not_now_puts_it_off() {
    var answers = scene("status-adoptable")
    answers.list = the("list-adoptable")
    settle(answers)
    verify(onboardingCard().visible)
    compare(onboardingCard().title, "Take over the addons already on Retail?")
    // Its action is the take-over's preview.
    pressCard(0)
    var preview = runningOf("adopt")
    verify(preview !== null, "Take over… previews")
    compare(words(preview.command), ["adopt", "--apply", "-t", "retail"])
    preview.process.finish(0, Fixtures.text("adopt-preview"), "streamFirst")
    tryVerify(function() { return !!service.confirm }, 1000)
    service.confirmNo()
    // Not now: gone for the shell's lifetime, and nothing ran.
    pressCard(1)
    compare(JSON.stringify(service.onboardingDismissed), JSON.stringify({ "adopt": ["retail"] }))
    verify(!onboardingCard().visible, "no other card is due")
    compare(runningOf("apply"), null)
  }

  function test_a_client_without_buggrabber_is_offered_its_own_build_as_a_preview() {
    var answers = scene("status-errors")
    answers.errors = the("errors")
    settle(answers)
    // The `errors` read the card needed ran by itself, and said Classic Era has a build.
    verify(service.errors !== null)
    compare(onboardingCard().title, "Record Lua errors on Classic Era?")
    pressCard(0)
    var install = runningOf("install")
    verify(install !== null, "Install BugGrabber previews")
    compare(words(install.command), ["install", "buggrabber", "-t", "era"])
    verify(install.command.indexOf("--dry-run") !== -1, "a preview first, never the install itself")
    pressCard(1)
    verify(!onboardingCard().visible)
  }

  function test_the_keybinding_tip_shows_once_after_a_mouse_open() {
    settle(scene("status-idle"))
    verify(!onboardingCard().visible, "not before the panel was opened by mouse")
    service.openedByMouse = true
    verify(onboardingCard().visible)
    compare(onboardingCard().title, "Open this panel with a key")
    var line = 'o.bind("SUPER + ALT + W", "WoW addons", { panel = "' + Build.PLUGIN_ID + '" })'
    verify(onboardingCard().body.indexOf(line) !== -1, onboardingCard().body)
    var spy = Qt.createQmlObject('import QtTest; SignalSpy { signalName: "settingRequested" }', testCase)
    spy.target = frame
    Quickshell.resetDetachedCommands()
    pressCard(0)
    compare(JSON.stringify(Quickshell.detachedCommands), JSON.stringify([["wl-copy", "--", line]]))
    compare(child("onboardingCopied").text, "Copied")
    compare(spy.count, 1)
    compare([spy.signalArguments[0][0], spy.signalArguments[0][1]], ["keyHint", "Off"])
    // The bar copy saves it and pushes it back: the tip is gone for good.
    service.applySettings({ "keyHint": "Off" })
    verify(!onboardingCard().visible)
    spy.destroy()
  }

  function test_esc_leaves_the_path_box_then_goes_back() {
    settle(scene("status-none"))
    frame.showView("updates")
    wait(10)
    press(welcome(), "Add a client folder…")
    var field = child("pathField", frame.current)
    tryVerify(function() { return field.activeFocus }, 1000)
    field.text = "/some/where"
    keyClick(Qt.Key_Escape)
    verify(!field.activeFocus, "Esc leaves the box")
    compare(field.text, "/some/where", "its text stays")
    keyClick(Qt.Key_Escape)
    compare(frame.topPage, null, "Esc again goes back")
  }
}
