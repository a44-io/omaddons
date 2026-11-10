import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "Fixtures.js" as Fixtures
import "BenchConfig.js" as BenchConfig

// scripts/widget-bench.sh, items 3 and 5 of omad-widget-user-first-1n2p.2, on the test
// stubs' mock Process, answered with the envelopes the real binary wrote in pipeline step
// 1 (Fixtures.js) and scaled to 20, 150 and 500 rows as tst_snapshots.qml scales them.
//
// Item 3: for Installed, Updates, Backups, Search's catalog browse and History, the time
// from showView to the panel's next render on the first visit (the view is built, its
// data already read), on a second visit (the view is kept), and after its data is read
// again with nothing changed (every delegate rebuilt), with the delegates the view built.
// The widget reads at most 100 changes for History; its 500 is the view's stress only.
// Item 5: the runner's RSS
// before and after opening and closing the panel 50 times, every view visited each time.
// Prints `WIDGET_BENCH {json}` lines.
TestCase {
  id: testCase
  name: "BenchLists"
  when: windowShown
  visible: true
  width: 620
  height: 800

  property var service: null
  property var card: null
  property var frame: null

  readonly property var rowPrefix: ({
    "installed": "installedRow_", "updates": "updateRow_", "backups": "backupRow_", "search": "searchRow_",
    "history": "historyRow_"
  })
  readonly property var listViews: ["installed", "updates", "backups", "search", "history"]

  Component {
    id: serviceComponent
    Service {}
  }

  Component {
    id: cardComponent
    BorderSurface {
      property alias view: inner
      x: 10
      y: 10
      width: 540 + 24
      height: Math.min(inner.contentImplicitHeight, 720) + 24
      color: Commons.Color.popups.background
      borderSpec: Border.flat(Commons.Color.popups.border, 2)
      radius: Style.cornerRadius

      PanelView {
        id: inner
        x: 12
        y: 12
        width: 540
        height: Math.min(contentImplicitHeight, 720)
      }
    }
  }

  // ---------------------------------------------------------------- the mock binary
  // (tst_snapshots.qml's, trimmed to the reads these views make)

  // The command's own words, after the widget's flags (by name: a read such as `history`
  // carries no `--actor`).
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
      if (launch.command[1] !== "--json") continue
      var p = launch.process
      if (!p || p.running !== true) continue
      out.push(launch)
    }
    return out
  }

  function fixture(name) { return JSON.parse(Fixtures.text(name)) }
  function text(env) { return JSON.stringify(env) }
  function the(name) { return { "text": Fixtures.text(name), "code": 0 } }

  function answerOf(scene, w) {
    if (w[0] === "status") return scene.status
    if (w[0] === "list") return scene.list
    if (w[0] === "errors") return scene.errors
    if (w[0] === "backup" && w[1] === "list" && w.indexOf("-v") !== -1) return scene.backupsDetail
    if (w[0] === "backup" && w[1] === "list") return scene.backups
    if (w[0] === "backup" && w[1] === "create")
      return { "text": text({ "ok": true, "schema_version": "1.1", "tool_version": Build.TOOL_VERSION, "command": "backup create",
        "exit_code": 0, "partial": false, "data": { "dry_run": false, "run": null, "targets": [] }, "warnings": [], "commands": [] }) }
    if (w[0] === "search" && w.indexOf("--catalog") !== -1) return scene.catalog
    if (w[0] === "history") return scene.history
    return null
  }

  // Answers every running request the scene has an answer for, until none is left.
  function settle(scene) {
    for (var round = 0; round < 60; round++) {
      var answered = 0
      var running = pending()
      for (var i = 0; i < running.length; i++) {
        var answer = answerOf(scene, words(running[i].command))
        if (!answer) continue
        running[i].process.finish(answer.code || 0, answer.text, "streamFirst")
        answered++
      }
      wait(answered ? 1 : 5)
      if (!answered && round > 2) break
    }
  }

  // ---------------------------------------------------------------- envelopes

  // `count` addons over the fixture's clients from its first addon: all behind for
  // Updates (every row an update), else one in three behind.
  function listWith(count, allBehind) {
    var env = fixture("list")
    var template = env.data.targets[1].addons[0]
    var lists = []
    for (var t = 0; t < env.data.targets.length; t++) lists.push([])
    for (var i = 0; i < count; i++) {
      var a = JSON.parse(JSON.stringify(template))
      a.name = "Addon" + ("000" + (i + 1)).slice(-4)
      a.folders = [a.name]
      a.version = "v1." + i
      var behind = allBehind || i % 3 === 0
      a.latest = { "version": behind ? "v1." + (i + 1) : a.version, "status": behind ? "update_available" : "current",
        "source": "github", "release_id": "gh:o/" + a.name.toLowerCase() + "@v1." + (i + 1), "asset": a.name + ".zip",
        "published_at": null }
      lists[i % lists.length].push(a)
    }
    for (var k = 0; k < lists.length; k++) env.data.targets[k].addons = lists[k]
    return { "text": text(env) }
  }

  function backupsWith(count) {
    var env = fixture("backups")
    var template = env.data.targets[0].backups[0]
    var list = []
    for (var i = 0; i < count; i++) {
      var b = JSON.parse(JSON.stringify(template))
      b.id = template.id.slice(0, 20) + ("00000" + i).slice(-6)
      b.kind = i % 4 === 0 ? "manual" : "auto"
      b.trigger = b.kind === "auto" ? (i % 2 ? "post_session" : "pre_change") : null
      list.push(b)
    }
    env.data.targets[0].backups = list
    env.data.targets[0].shown = count
    env.data.targets[0].total = count
    return { "text": text(env) }
  }

  function catalogWith(count) {
    var env = fixture("search-catalog")
    var results = env.data.results
    var list = []
    for (var i = 0; i < count; i++) {
      var r = JSON.parse(JSON.stringify(results[i % results.length]))
      if (i >= results.length) r.name = r.name + " " + (i + 1)
      list.push(r)
    }
    env.data.results = list
    return { "text": text(env) }
  }

  // `count` changes from the fixture's own (each kind it recorded, in turn), each its own
  // run with its own ids, an hour apart: a day's heading every 24 rows.
  function historyWith(count) {
    var env = fixture("history")
    var templates = env.data.entries
    var newest = Model.instantMs(templates[0].at)
    var list = []
    for (var i = 0; i < count; i++) {
      var e = JSON.parse(JSON.stringify(templates[i % templates.length]))
      e.tx = e.tx.slice(0, 20) + ("00000" + i).slice(-6)
      e.run = e.run.slice(0, 20) + ("00000" + i).slice(-6)
      e.at = new Date(newest - i * 3600000).toISOString()
      list.push(e)
    }
    env.data.entries = list
    return { "text": text(env) }
  }

  function scene(count, allBehind) {
    return { "status": the("status-updates"), "list": listWith(count, allBehind), "errors": the("errors"),
      "backups": backupsWith(count), "backupsDetail": the("backups-detail"), "catalog": catalogWith(count),
      "history": historyWith(count) }
  }

  // ---------------------------------------------------------------- the panel

  function start(sceneNow) {
    if (service) service.destroy()
    service = null
    wait(0)
    ProcessRegistry.reset()
    Quickshell.resetDetachedCommands()
    service = serviceComponent.createObject(testCase, {
      "settings": { "autoCheck": "Off", "notifyErrors": "Off", "notifyUpdates": "Off" },
      "bellDebounceMs": 10, "localDeadlineMs": 600000, "networkDeadlineMs": 600000, "killGraceMs": 60, "streamGraceMs": 30
    })
    verify(service !== null)
    settle(sceneNow)
  }

  // The panel opened on Issues, which builds no list, with every read answered. The
  // frame's view is set before it has a service, so Updates (its default) is never
  // visited, and its first visit below builds it.
  function openPanel(sceneNow) {
    card = cardComponent.createObject(testCase, {})
    verify(card !== null)
    frame = card.view
    frame.view = "issues"
    frame.svc = service
    service.setPanelOpen("bench", true)
    frame.opened = true
    frame.showView("issues")
    settle(sceneNow)
    service.readCatalog("")
    // Backups and History are read on their view's first visit since 1n2p.38 (the open
    // reads the list and the errors): read here, so each first visit below times the
    // view built with its data, as the others are, never an empty one.
    service.readBackups()
    service.readHistory()
    settle(sceneNow)
    wait(20)
  }

  function closePanel() {
    if (service) service.setPanelOpen("bench", false)
    if (card) card.destroy()
    card = null
    frame = null
    wait(0)
  }

  function cleanup() {
    closePanel()
    if (service) service.destroy()
    service = null
    ProcessRegistry.reset()
    wait(0)
  }

  // The items under `item` (visual children), and those whose objectName starts with
  // `prefix`: the view's row delegates.
  function census(item, prefix) {
    var stack = [item]
    var items = 0
    var rows = 0
    while (stack.length) {
      var at = stack.pop()
      if (!at) continue
      items++
      if (prefix && typeof at.objectName === "string" && at.objectName.indexOf(prefix) === 0) rows++
      var kids = at.children || []
      for (var i = 0; i < kids.length; i++) stack.push(kids[i])
    }
    return { "items": items, "rows": rows }
  }

  // The time from `t0` to the next frame, or null when none came within 3 s (then
  // nothing changed on screen, and there is nothing to time).
  function rendered(t0) {
    return waitForRendering(card, 3000) ? Date.now() - t0 : null
  }

  function timedShow(view) {
    var t0 = Date.now()
    frame.showView(view)
    return rendered(t0)
  }

  // The view's data read again with the same answer: the time from the answer to the
  // next render, every delegate rebuilt from the new arrays.
  function timedReread(view, sceneNow) {
    service.readAgain(view)
    var running = []
    var start = Date.now()
    while (running.length === 0) {
      running = pending().filter(function(l) { return answerOf(sceneNow, words(l.command)) !== null })
      if (Date.now() - start > 5000) return null
      wait(1)
    }
    var answer = answerOf(sceneNow, words(running[0].command))
    var t0 = Date.now()
    running[0].process.finish(0, answer.text, "streamFirst")
    var took = rendered(t0)
    settle(sceneNow)
    return took
  }

  function median(values) {
    var sorted = values.filter(function(v) { return v !== null }).sort(function(a, b) { return a - b })
    return sorted.length ? sorted[Math.floor((sorted.length - 1) / 2)] : null
  }

  function measure(view, count) {
    var s = scene(count, view === "updates")
    start(s)
    openPanel(s)
    var first = timedShow(view)
    var counted = census(frame.current, rowPrefix[view])
    timedShow("issues")
    var again = timedShow(view)
    var reread = view === "search" ? null : timedReread(view, s)
    closePanel()
    return { "first_ms": first, "again_ms": again, "reread_ms": reread, "rows": counted.rows, "items": counted.items }
  }

  // The floor under every number here: a frame of the offscreen software renderer for a
  // change that builds nothing (the card's opacity), on the panel open on Issues.
  function frameFloor() {
    var s = scene(20, false)
    start(s)
    openPanel(s)
    var frames = []
    for (var i = 0; i < 10; i++) {
      var t0 = Date.now()
      card.opacity = i % 2 ? 1 : 0.99
      frames.push(rendered(t0))
    }
    card.opacity = 1
    closePanel()
    return median(frames)
  }

  function test_1_list_costs() {
    var floor = frameFloor()
    // Every view built once first, so component compilation is not counted below.
    for (var w of listViews) measure(w, 20)
    var results = []
    for (var view of listViews) {
      for (var count of BenchConfig.COUNTS) {
        var runs = []
        for (var r = 0; r < BenchConfig.REPEATS; r++) runs.push(measure(view, count))
        results.push({
          "view": view, "count": count, "rows": runs[0].rows, "items": runs[0].items,
          "first_ms": median(runs.map(function(x) { return x.first_ms })),
          "first_ms_max": Math.max.apply(null, runs.map(function(x) { return x.first_ms })),
          "again_ms": median(runs.map(function(x) { return x.again_ms })),
          "reread_ms": median(runs.map(function(x) { return x.reread_ms }))
        })
      }
    }
    console.log("WIDGET_BENCH " + JSON.stringify({ "kind": "lists", "repeats": BenchConfig.REPEATS, "frame_ms": floor, "results": results }))
  }

  // ---------------------------------------------------------------- item 5

  function rss() {
    var xhr = new XMLHttpRequest()
    xhr.open("GET", "file:///proc/self/status", false)
    xhr.send()
    var body = String(xhr.responseText || "")
    var current = /VmRSS:\s+(\d+) kB/.exec(body)
    return current ? Number(current[1]) : -1
  }

  function settledRss() {
    for (var i = 0; i < 3; i++) {
      gc()
      wait(50)
    }
    return rss()
  }

  // One open and close; the mock's own launch records are cleared first (the lanes are
  // idle between cycles), so what grows is the widget's, not the harness's.
  function cycle(s) {
    ProcessRegistry.reset()
    openPanel(s)
    for (var view of Model.VIEWS) {
      frame.showView(view)
      settle(s)
    }
    closePanel()
  }

  function test_2_open_close_leak() {
    var s = scene(150, false)
    start(s)
    for (var w = 0; w < 3; w++) cycle(s)
    var before = settledRss()
    verify(before > 0, "VmRSS read from /proc/self/status")
    var marks = []
    for (var i = 1; i <= BenchConfig.CYCLES; i++) {
      cycle(s)
      if (i % 10 === 0) marks.push({ "cycles": i, "rss_kb": settledRss() })
    }
    var after = settledRss()
    console.log("WIDGET_BENCH " + JSON.stringify({ "kind": "leak", "cycles": BenchConfig.CYCLES, "rows": 150,
      "rss_kb_before": before, "rss_kb_after": after, "growth_kb": after - before,
      "growth_kb_per_cycle": Math.round((after - before) / BenchConfig.CYCLES * 10) / 10, "marks": marks }))
  }
}
