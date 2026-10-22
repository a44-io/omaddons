import QtQuick
import QtTest
import Quickshell
import Quickshell.Hyprland
import Quickshell.Io
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build

// The service's desktop triggers (F8) against the mock Process and a mock Hyprland:
// window events as a doorbell with follow-ups after a close, the post-session
// checkpoint then errors read, the catch-up at start behind any user change, errors
// read again when stale or unchecked, the automatic check, and the toasts (rises only,
// never the first status, never while incompatible or missing, never error text).
TestCase {
  id: testCase
  name: "Signals"

  property var service: null
  // What `status` answers with: client id → {running, updates, errors: {state, new, top}}.
  property var world: ({})
  property bool checkDue: false
  // Indexes into ProcessRegistry.launches already answered (a finished process is gone).
  property var answered: ({})

  Component {
    id: serviceComponent
    Service {}
  }

  function make(extra) {
    if (service) service.destroy()
    service = null
    wait(0)
    ProcessRegistry.reset()
    Quickshell.resetDetachedCommands()
    answered = {}
    world = {}
    checkDue = false
    var props = { "bellDebounceMs": 10, "localDeadlineMs": 5000, "networkDeadlineMs": 5000, "killGraceMs": 60, "streamGraceMs": 60,
      "windowDebounceMs": 30, "followUpDelaysMs": [150, 400, 650], "runningPollMs": 60000 }
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

  function isOmaddons(launch) {
    return launch.command[1] === "--json" && launch.command[2] === "--actor"
  }

  // omaddons launches whose command words start with `word` (all when undefined).
  function runs(word) {
    return ProcessRegistry.launches.filter(function(l) { return isOmaddons(l) && (word === undefined || words(l.command)[0] === word) })
  }

  // Indexes of the omaddons launches not answered yet.
  function pending(word) {
    var out = []
    var all = ProcessRegistry.launches
    for (var i = 0; i < all.length; i++) {
      if (answered[i] || !isOmaddons(all[i])) continue
      if (word === undefined || words(all[i].command)[0] === word) out.push(i)
    }
    return out
  }

  function wordsAt(i) {
    return words(ProcessRegistry.launches[i].command)
  }

  function answerAt(i, text) {
    var next = {}
    for (var key in answered) next[key] = true
    next[i] = true
    answered = next
    ProcessRegistry.launches[i].process.finish(0, text)
  }

  function envelope(command, data, extra) {
    var e = { "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": command,
      "exit_code": 0, "partial": false, "data": data, "warnings": [], "commands": [] }
    if (extra) for (var key in extra) e[key] = extra[key]
    return JSON.stringify(e)
  }

  // `status --cached` over `clients` (as `world`).
  function status(clients, due, extra) {
    var targets = []
    var running = []
    var errorsNew = 0
    var errorTargets = []
    var available = 0
    var updateTargets = []
    for (var id in clients) {
      var c = clients[id]
      var errors = c.errors || {}
      var fresh = errors["new"] || 0
      targets.push({ "id": id, "label": id, "running": c.running === undefined ? false : c.running,
        "updates": { "available": c.updates || 0 }, "issues": [],
        "errors": { "state": errors.state || "ok", "coverage": "complete", "new": fresh, "total": fresh,
          "addons": (errors.top || []).length, "top": errors.top || [], "last_session_at": null } })
      if (c.running === true) running.push(id)
      errorsNew += fresh
      if (fresh > 0) errorTargets.push(id)
      available += c.updates || 0
      if (c.updates) updateTargets.push(id)
    }
    return envelope("status", { "cached": true, "targets": targets,
      "summary": { "targets": targets.length, "running": running, "updates": { "available": available, "targets": updateTargets },
        "issues": { "p0": 0, "p1": 0, "p2": 0, "p3": 0 }, "errors": { "new": errorsNew, "targets": errorTargets },
        "check_due": due === true, "widget_outdated": false, "state": "idle" },
      "last_change": null, "last_change_state": "ok" }, extra)
  }

  function replyFor(w) {
    if (w[0] === "status") return status(world, checkDue)
    if (w[0] === "backup") return envelope("backup create", { "targets": [] })
    if (w[0] === "outdated") return envelope("outdated", { "targets": [] })
    if (w[0] === "pin") return envelope("pin", { "targets": [{ "target": { "id": "retail" }, "items": [{ "name": w[w.length - 1] }] }] })
    return envelope(w[0], { "targets": [] })
  }

  // Answers every launch still waiting, and whatever those answers start, until none is.
  function settle() {
    for (var round = 0; round < 30; round++) {
      var open = pending()
      if (open.length === 0) {
        wait(0)
        open = pending()
        if (open.length === 0) return
      }
      for (var i = 0; i < open.length; i++) answerAt(open[i], replyFor(wordsAt(open[i])))
    }
    fail("still running after 30 rounds")
  }

  // The startup status, answered from `clients`.
  function start(clients, due) {
    world = clients
    checkDue = due === true
    var first = pending("status")
    compare(first.length, 1)
    answerAt(first[0], status(world, checkDue))
    compare(service.phase, "ok")
  }

  // A refresh, answered from the world as it is now (and nothing else).
  function refreshWith(clients) {
    world = clients
    service.refresh()
    var open = pending("status")
    verify(open.length > 0, "a status to answer")
    answerAt(open[0], status(world, checkDue))
  }

  function toasts() {
    return Quickshell.detachedCommands.filter(function(c) { return c[0] === "omarchy-notification-send" })
  }

  function lanes() {
    return JSON.parse(service.stateJson()).lanes
  }

  // ---- window events (M3-D13)

  function test_a_window_opening_or_closing_refreshes_once_after_the_debounce() {
    start({ "retail": {} })
    settle()
    var before = runs("status").length
    Hyprland.emitRaw("activewindow", "kitty,kitty")
    wait(100)
    compare(runs("status").length, before, "focus changes ring nothing")
    Hyprland.emitRaw("openwindow", "5612abc,1,steam_app_wow335,World of Warcraft")
    Hyprland.emitRaw("closewindow", "5612abc")
    Hyprland.emitRaw("openwindow", "5612abd,1,kitty,kitty")
    compare(runs("status").length, before, "nothing before the debounce")
    tryVerify(function() { return runs("status").length === before + 1 }, 1000)
    compare(wordsAt(pending("status")[0]), ["status", "--cached", "-v"])
    settle()
    wait(800)
    compare(runs("status").length, before + 1, "one refresh for the burst, and no follow-ups with nothing running")
  }

  function test_a_window_closing_while_a_client_runs_is_followed_up() {
    start({ "retail": { "running": true } })
    settle()
    var before = runs("status").length
    Hyprland.emitRaw("closewindow", "5612abc")
    for (var n = 1; n <= 4; n++) {
      tryVerify(function() { return runs("status").length === before + n }, 1500)
      settle()
    }
    wait(800)
    compare(runs("status").length, before + 4, "the debounced refresh and three follow-ups")
  }

  function test_while_a_client_runs_the_poll_is_short() {
    make({ "runningPollMs": 150 })
    start({ "retail": {} })
    settle()
    var before = runs("status").length
    wait(450)
    compare(runs("status").length, before)
    refreshWith({ "retail": { "running": true } })
    settle()
    before = runs("status").length
    tryVerify(function() { return runs("status").length > before }, 1000)
  }

  // ---- post-session work and the catch-up (M3-D31, M3-D13)

  function test_a_client_that_stopped_gets_its_checkpoint_then_its_errors_read() {
    start({ "retail": { "running": true }, "era": {} })
    settle()
    compare(runs("backup").map(function(l) { return words(l.command) }), [["backup", "create", "--auto", "-t", "era"]])
    var from = ProcessRegistry.launches.length
    refreshWith({ "retail": {}, "era": {} })
    var started = ProcessRegistry.launches.slice(from).filter(isOmaddons).map(function(l) { return words(l.command) })
    compare(started.slice(1), [["backup", "create", "--auto", "-t", "retail"], ["errors", "--limit", "10", "-t", "retail"]])
    compare(lanes().mutation.running, "backup_auto")
    compare(lanes().local.running, "errors_target")
    verify(ProcessRegistry.launches[pending("backup")[0]].command.indexOf("Settings checkpoint after play (bar widget)") !== -1)
    settle()
    compare(runs("backup").length, 2, "era did not stop: no second checkpoint")
    compare(toasts().length, 0, "a silent checkpoint says nothing")
    compare(service.lastResult, null)
  }

  function test_at_start_every_stopped_client_gets_a_checkpoint_behind_any_user_change() {
    start({ "retail": {}, "era": {}, "classic": { "running": true } })
    var autos = pending("backup")
    compare(autos.length, 1, "one lane: the second waits")
    compare(wordsAt(autos[0]), ["backup", "create", "--auto", "-t", "retail"])
    verify(ProcessRegistry.launches[autos[0]].command.indexOf("Settings checkpoint at widget start (bar widget)") !== -1)
    // Silent: no progress, and the panel's changes stay on.
    compare(service.action, null)
    verify(!service.bar.spinning)
    compare(service.run("pin", { "target": "retail", "addon": "Baz" }, "Pin Baz"), "")
    compare(service.queuedAction, "Pin Baz")
    answerAt(autos[0], replyFor(["backup"]))
    compare(pending("pin").length, 1, "the user's change runs next")
    compare(pending("backup").length, 0)
    answerAt(pending("pin")[0], replyFor(["pin", "Baz"]))
    var second = pending("backup")
    compare(second.length, 1)
    compare(wordsAt(second[0]), ["backup", "create", "--auto", "-t", "era"])
    settle()
    compare(runs("backup").length, 2, "never one for the running client")
  }

  function test_a_silent_checkpoint_shows_nothing_unless_it_fails() {
    start({ "retail": {} })
    var auto = pending("backup")[0]
    ProcessRegistry.launches[auto].process.finish(1, JSON.stringify({ "ok": false, "schema_version": "1.0",
      "tool_version": Build.TOOL_VERSION, "command": "backup create", "exit_code": 1, "warnings": [], "commands": [],
      "error": { "code": "space_low", "message": "not enough free space for a backup", "fix": null, "fix_argv": null,
        "alternatives": [], "retryable": false, "details": {} } }))
    answered = { [auto]: true }
    verify(service.lastResult !== null && !service.lastResult.ok)
    compare(service.lastError.code, "space_low")
  }

  // ---- errors read again (M3-D20)

  function test_errors_are_read_again_when_stale_or_unchecked_never_when_partial() {
    start({ "retail": { "errors": { "state": "partial" } } })
    settle()
    compare(runs("errors").length, 0)
    refreshWith({ "retail": { "errors": { "state": "stale" } }, "era": { "errors": { "state": "unchecked" } } })
    // One read per client, one at a time on the local lane.
    compare(pending("errors").map(wordsAt), [["errors", "--limit", "10", "-t", "retail"]])
    settle()
    compare(runs("errors").map(function(l) { return words(l.command) }),
      [["errors", "--limit", "10", "-t", "retail"], ["errors", "--limit", "10", "-t", "era"]])
    compare(runs("backup").length, 1, "a client seen for the first time is no transition")
    refreshWith({ "retail": { "errors": { "state": "partial" } }, "era": {} })
    settle()
    compare(runs("errors").length, 2)
  }

  // ---- the automatic check (M3-D12)

  function test_an_update_check_runs_when_due_at_most_once_per_interval() {
    make({ "autoCheckIntervalMs": 400 })
    start({ "retail": { "running": true } }, true)
    compare(runs("outdated").length, 1)
    compare(lanes().network.running, "check")
    verify(ProcessRegistry.launches[pending("outdated")[0]].command.indexOf("Automatic update check (bar widget)") !== -1)
    settle()
    compare(runs("outdated").length, 1, "the status after the check is within the interval")
    tryVerify(function() { return runs("outdated").length === 2 }, 2000)
    settle()
    service.applySettings({ "autoCheck": "Off" })
    var n = runs("outdated").length
    wait(900)
    settle()
    compare(runs("outdated").length, n, "off: never")
    service.applySettings({ "autoCheck": "On" })
    checkDue = false
    settle()
    service.refresh()
    settle()
    n = runs("outdated").length
    wait(900)
    compare(runs("outdated").length, n, "not due: never")
  }

  // ---- toasts (M3-D14, M3-D21)

  function test_toasts_fire_on_rises_only_and_never_on_the_first_status() {
    service.applySettings({ "notifyUpdates": "On" })
    start({ "retail": { "updates": 2, "errors": { "new": 3, "top": [{ "addon": "DBM", "folder": "DBM-Core", "new": 3, "count": 9 }] } } })
    settle()
    compare(toasts().length, 0, "nothing on the first status")
    refreshWith({ "retail": { "updates": 3, "errors": { "new": 4, "top": [
      { "addon": "DBM", "folder": "DBM-Core", "new": 3, "count": 9 }, { "addon": "WeakAuras", "folder": "WeakAuras", "new": 1, "count": 1 }] } } })
    var t = toasts()
    compare(t.length, 2)
    compare(t[0].slice(0, 7), ["omarchy-notification-send", "--app-name", "omaddons", "-g", t[0][4], "-u", "normal"])
    compare(t[0].slice(7, 9), ["Lua errors in your last session on retail", "DBM 3 · WeakAuras 1"])
    compare(t[0].slice(-6), ["--exec", "omarchy-shell", "shell", "summon", Build.PLUGIN_ID, "{}"])
    compare(t[1].slice(6, 9), ["low", "Addon updates", "3 updates on retail"])
    settle()
    refreshWith(world)
    compare(toasts().length, 2, "the same counts again: nothing")
    service.applySettings({ "notifyUpdates": "Off", "notifyErrors": "Off" })
    refreshWith({ "retail": { "updates": 5, "errors": { "new": 6, "top": [{ "addon": "DBM", "folder": "DBM-Core", "new": 6, "count": 12 }] } } })
    compare(toasts().length, 2, "both off: nothing")
  }

  function test_no_toast_while_incompatible_or_missing() {
    start({ "retail": {} })
    settle()
    // Another build answers with more errors: read-only, and silent.
    service.refresh()
    answerAt(pending("status")[0], status({ "retail": { "errors": { "new": 2, "top": [{ "addon": "DBM", "folder": "DBM-Core", "new": 2, "count": 2 }] } } },
      false, { "tool_version": "9.9.9" }))
    compare(service.phase, "incompatible")
    settle()
    compare(toasts().length, 0)
    compare(runs("errors").length + runs("outdated").length, 0, "no automatic work while incompatible")
    // No binary at all.
    ProcessRegistry.missingBinary = true
    service.refresh()
    tryCompare(service, "phase", "missing")
    compare(toasts().length, 0)
  }

  // Whatever the panel's errors read held, the toast names addons, clients and counts.
  function test_the_error_toast_carries_no_error_text() {
    start({ "retail": {} })
    settle()
    var secrets = ["Interface/AddOns/DBM-Core/DBM-Core.lua:412", "attempt to index a nil value (field 'boss')",
      "FakeChar-Realm pulled too early", "local timer = <table>", "in function 'StartTimer'", "Interface/AddOns/DBM-Core"]
    service.readErrors()
    answerAt(pending("errors")[0], envelope("errors", { "targets": [{ "target": { "id": "retail" }, "capture": "buggrabber",
      "coverage": "complete", "files": { "partial": 0 }, "databases": [], "summary": { "new": 1, "received": 0 }, "dismiss": null,
      "addons": [{ "addon": "DBM", "managed": true, "kind": "addon", "version": "1", "latest": null, "count": 1, "new": 1,
        "errors": [{ "message": secrets[1] + " " + secrets[2], "location": secrets[0], "stack": [secrets[4]], "locals": secrets[3],
          "count": 1, "new": true, "received": false, "first_at": null, "last_at": null }] }] }] }))
    verify(service.errors !== null)
    refreshWith({ "retail": { "errors": { "new": 1, "top": [{ "addon": "DBM", "folder": "DBM-Core", "new": 1, "count": 1 }] } } })
    var t = toasts()
    compare(t.length, 1)
    for (var i = 0; i < t[0].length; i++) {
      for (var j = 0; j < secrets.length; j++) verify(t[0][i].indexOf(secrets[j]) === -1, t[0][i] + " holds " + secrets[j])
      verify(t[0][i].indexOf("DBM-Core") === -1, "not even the folder")
    }
  }

  function test_a_change_that_finished_with_every_panel_closed_says_so_in_a_toast() {
    start({ "retail": { "running": true } })
    settle()
    compare(service.run("pin", { "target": "retail", "addon": "Baz" }, "Pin Baz"), "")
    answerAt(pending("pin")[0], replyFor(["pin", "Baz"]))
    var t = toasts()
    compare(t.length, 1)
    verify(service.lastResult.line !== "")
    compare(t[0].slice(5, 9), ["-u", "low", "omaddons", service.lastResult.line])
    settle()
    service.setPanelOpen("bar@DP-1", true)
    settle()
    compare(service.run("pin", { "target": "retail", "addon": "Baz" }, "Pin Baz"), "")
    answerAt(pending("pin")[0], replyFor(["pin", "Baz"]))
    compare(toasts().length, 1, "an open panel shows the result itself")
  }
}
