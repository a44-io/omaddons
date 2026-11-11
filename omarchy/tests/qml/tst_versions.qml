import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import qs.Commons
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "Fixtures.js" as Fixtures

// An addon page's Versions (1n2p.24) on the real Service and frame, over the binary's
// Versions scene (versions_fixtures): Foo v1, v2 and the beta v3 on Retail. `v` on the
// client line lists its releases (its own `info --releases` read, asked only then); an
// older release previews a downgrade that pins it and applies only through the plan
// sheet; a channel previews and applies the same way, and the page then follows it;
// `v` again closes them; the undo of its last update comes first.
TestCase {
  id: testCase
  name: "Versions"
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

  function cleanup() {
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

  function ran(word) {
    return ProcessRegistry.launches.filter(function(l) { return l.command[1] === "--json" && words(l.command)[0] === word })
  }

  function the(name) { return { "text": Fixtures.text(name), "code": 0 } }

  function envelope(command, data) {
    return { "text": JSON.stringify({ "ok": true, "schema_version": "1.1", "tool_version": Build.TOOL_VERSION, "command": command,
      "exit_code": 0, "partial": false, "data": data, "warnings": [], "commands": [] }) }
  }

  // Foo's scene on Retail, its list as `list` names it.
  function scene(list) {
    return { "status": the("versions-status"), "status_full": the("versions-status"), "list": the(list),
      "errors": envelope("errors", { "targets": [] }), "backup": envelope("backup list", { "targets": [] }),
      "catalog": envelope("search", { "results": [], "sources_skipped": [] }), "history": the("history-empty"),
      "auto": envelope("backup create", { "dry_run": false, "run": null, "targets": [] }),
      "info": the("changelog"), "releases": the("versions-releases") }
  }

  // Answers every running read from `answers` until none is left; a change (a preview,
  // an apply) is answered by the test itself.
  function settle(answers) {
    for (var round = 0; round < 40; round++) {
      var answered = 0
      var running = pending()
      for (var i = 0; i < running.length; i++) {
        var w = words(running[i].command)
        var key = w[0] === "status" ? (w.indexOf("--cached") !== -1 ? "status" : "status_full")
          : w[0] === "backup" && w[1] === "create" && w.indexOf("--auto") !== -1 ? "auto"
          : w[0] === "search" && w.indexOf("--catalog") !== -1 ? "catalog"
          : w[0] === "info" && w.indexOf("--releases") !== -1 ? "releases" : w[0]
        if (!answers.hasOwnProperty(key)) continue
        running[i].process.finish(answers[key].code || 0, answers[key].text, "streamFirst")
        answered++
      }
      wait(answered ? 5 : 15)
      if (!answered && round > 2) break
    }
  }

  function answerChange(word, name) {
    var launch = null
    tryVerify(function() { launch = runningOf(word); return launch !== null }, 2000, "a " + word + " runs")
    launch.process.finish(0, Fixtures.text(name), "streamFirst")
    wait(10)
    return launch
  }

  function child(name, from) {
    var found = findChild(from || frame, name)
    verify(found !== null, "no " + name)
    return found
  }

  function laidOut() {
    verify(waitForPolish(testCase.Window.window), "the window's layout settles")
  }

  // Foo's page, opened from the list, with the keyboard on its Retail line.
  function openFoo(answers) {
    ProcessRegistry.reset()
    service = serviceComponent.createObject(testCase, { "bellDebounceMs": 10, "localDeadlineMs": 600000,
      "networkDeadlineMs": 600000, "killGraceMs": 60, "streamGraceMs": 30 })
    frame = frameComponent.createObject(testCase, { "svc": service })
    service.setPanelOpen("versions", true)
    frame.opened = true
    settle(answers)
    frame.openAddon("Foo", "gh:o/foo")
    settle(answers)
    frame.focusItem.forceActiveFocus()
    tryVerify(function() { return frame.focusItem.activeFocus })
    frame.cursor = 0
    frame.cursorActive = true
    var page = frame.current
    compare(page.page.clients[0].target, "retail")
    return page
  }

  // `v`: its releases read once, the Versions lines after the client line.
  function openVersions(page, answers) {
    var before = ran("info").length
    keyClick("v")
    tryVerify(function() { return ran("info").length > before }, 1000, "the releases read runs")
    var read = ran("info")[ran("info").length - 1]
    compare(words(read.command), ["info", "--releases", "--", "gh:o/foo"])
    settle(answers)
    compare(page.page.versions.state, "")
    verify(child("versionsHeader", page).visible)
    compare(child("versionsHeader", page).title, "Versions on Retail")
    // The cursor on their first line, which shows: below the fold, they would seem not
    // to have opened.
    compare(frame.cursor, 1)
    laidOut()
    verify(onScreen(page.rowItem(1)), "the first Versions line is in view")
  }

  // Whether `item` shows whole inside the frame's scrolled content.
  function onScreen(item) {
    var flick = child("contentFlick")
    var top = item.mapToItem(flick, 0, 0).y
    return top >= 0 && top + item.height <= flick.height
  }

  // The wheel over the content, a tick at a time, until `item` shows (a player reaching
  // for a line below the fold).
  function scrollTo(item) {
    var flick = child("contentFlick")
    for (var tick = 0; tick < 10 && !onScreen(item); tick++) {
      mouseWheel(flick, 40, flick.height / 2, 0, -120)
      wait(30)
    }
    tryVerify(function() { return !flick.moving && onScreen(item) }, 2000, "scrolled into view")
    laidOut()
  }

  function sheet() {
    tryVerify(function() { return !!service.confirm && !!service.confirm.sheet }, 2000, "the plan sheet opens")
    return service.confirm.sheet
  }

  // ---- an older release

  function test_an_older_release_previews_a_pinned_downgrade_and_applies_through_the_sheet() {
    var answers = scene("versions-list-beta")
    var page = openFoo(answers)
    openVersions(page, answers)
    // The release notes' lines come last (omad-z3ur).
    compare(page.rows.filter(function(r) { return r.kind !== "note" }).map(function(r) { return r.title || r.client }),
      ["Retail", "Follow stable releases", "Follow betas", "Follow alphas", "v3", "v2", "v1"])
    compare(page.rows.slice(7).map(function(r) { return r.kind }), page.page.notes.map(function() { return "note" }))
    for (var i = 0; i < 5; i++) keyClick("j")
    compare(frame.cursor, 6)
    compare(page.rows[6].title, "v1")
    keyClick(Qt.Key_Return)
    var preview = answerChange("install", "versions-install-preview")
    verify(preview.command.indexOf("--dry-run") !== -1, "a preview first")
    compare(words(preview.command), ["install", "-t", "retail", "--version=v1", "--", "gh:o/foo"])
    var s = sheet()
    compare(s.effects.map(function(e) { return e.text }), ["Goes back to an older version (v3 → v1).",
      "Pins it: it stays at this version until you unpin it."])
    compare(ran("apply").length, 0, "nothing runs before Yes")
    tryVerify(function() { return child("planSheet").keyItem.activeFocus })
    keyClick("y")
    var apply = answerChange("apply", "versions-install-apply")
    compare(words(apply.command), ["apply", JSON.parse(Fixtures.text("versions-install-preview")).data.plan_id])
    // The page reads its addon again: v1 installed, pinned.
    var after = scene("versions-list-fixed")
    after.history = the("versions-history")
    settle(after)
    tryVerify(function() {
      var v1 = page.page.versions.lines.filter(function(l) { return l.title === "v1" })[0]
      return !!v1 && v1.chips.length === 2 && v1.chips[0].text === "installed"
    }, 2000, "v1 shows installed and pinned")
  }

  // ---- a channel, by mouse

  function test_a_channel_previews_and_applies_through_the_sheet_and_the_page_follows_it() {
    var answers = scene("versions-list-stable")
    var page = openFoo(answers)
    openVersions(page, answers)
    var beta = page.page.versions.lines.filter(function(l) { return l.channel === "beta" })[0]
    compare([beta.title, beta.meta, beta.chips.length], ["Follow betas", "newest listed: v3", 0])
    var line = child("versionLine_ch/beta", page)
    scrollTo(line)
    mouseClick(line, 40, line.height / 2)
    var preview = answerChange("update", "versions-channel-preview")
    compare(words(preview.command), ["update", "-t", "retail", "--channel=beta", "--", "Foo"])
    compare(sheet().effects.map(function(e) { return e.text }), ["Foo switches to beta releases."])
    compare(ran("apply").length, 0, "nothing runs before Yes")
    laidOut()
    var confirm = child("sheetConfirm", child("planSheet"))
    mouseClick(confirm, confirm.width / 2, confirm.height / 2)
    answerChange("apply", "versions-channel-apply")
    settle(scene("versions-list-beta"))
    tryVerify(function() { return page.page.clients[0].meta.indexOf("beta releases") !== -1 }, 2000,
      "the client line says it follows beta: " + page.page.clients[0].meta)
    var following = page.page.versions.lines.filter(function(l) { return l.channel === "beta" })[0]
    compare(following.chips.map(function(c) { return c.text }), ["following"])
  }

  // ---- `v` again, and the undo first

  function test_v_again_closes_them_and_the_cursor_goes_back_to_the_client() {
    var answers = scene("versions-list-beta")
    var page = openFoo(answers)
    openVersions(page, answers)
    keyClick("j")
    compare(frame.cursor, 2)
    var reads = ran("info").length
    keyClick("v")
    compare(page.page.versions.open, false)
    // The client line, then the release notes' lines (omad-z3ur).
    compare(page.rows.length, 1 + page.page.notes.length)
    compare(page.rows[0].client, "Retail")
    compare(frame.cursor, 0)
    // Open again: the releases are not read twice.
    keyClick("v")
    compare(page.page.versions.lines.length, 6)
    compare(frame.cursor, 1)
    compare(ran("info").length, reads, "read once while the page is open")
  }

  function test_the_undo_of_its_last_update_comes_first_and_previews() {
    var answers = scene("versions-list-fixed")
    answers.history = the("versions-history")
    var page = openFoo(answers)
    openVersions(page, answers)
    var first = page.page.versions.lines[0]
    compare(first.kind, "undo")
    verify(first.title.indexOf("Undo the update from ") === 0, first.title)
    var v1 = page.page.versions.lines.filter(function(l) { return l.title === "v1" })[0]
    compare(v1.chips.map(function(c) { return c.text }), ["installed", "installed as a fixed version"])
    compare(frame.cursor, 1)
    keyClick(Qt.Key_Return)
    var undo = null
    tryVerify(function() { undo = runningOf("undo"); return undo !== null }, 2000, "its undo previews")
    verify(undo.command.indexOf("--dry-run") !== -1)
    var w = words(undo.command)
    compare(w.slice(0, 1), ["undo"])
    verify(w.indexOf("-t") !== -1 && w[w.indexOf("-t") + 1] === "retail", JSON.stringify(w))
  }
}
