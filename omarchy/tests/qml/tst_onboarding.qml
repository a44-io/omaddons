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
    return { "list": empty("list"), "errors": empty("errors"), "backup": empty("backup list") }
  }

  function scene(statusName, fullName) {
    var answers = quiet()
    answers.status = the(statusName)
    if (fullName) answers.status_full = the(fullName)
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
    wait(10)
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
