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
import "BenchConfig.js" as BenchConfig

// scripts/widget-bench.sh, item 2 of omad-widget-user-first-1n2p.2: how long the panel
// takes to open, against the real binary on the bench home. The real Service and
// PanelView run under qmltestrunner; the bench's Process stand-in has the runner run each
// child (runner.js). Each open is timed from setPanelOpen(true) to the panel's first
// render, and to the end of each read the open queues (status -v, list -v, errors,
// backup list), in the order they finish. The runner's log gives the binary's own wall
// time per read, so what remains is the widget's: queueing, the hop to the runner,
// parsing and reducing. Prints one `WIDGET_BENCH {json}` line.
TestCase {
  id: testCase
  name: "BenchOpen"
  when: windowShown
  visible: true
  width: 620
  height: 800

  property var service: null
  property var card: null
  property var finished: []

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

  function onFinished(resource, outcome) {
    finished = finished.concat([{ "resource": resource, "outcome": outcome, "at": Date.now() }])
  }

  function idle() {
    var state = JSON.parse(service.stateJson())
    for (var name in state.lanes) {
      var lane = state.lanes[name]
      if (lane.running !== null || lane.queued !== 0) return false
    }
    return true
  }

  function waitIdle(timeoutMs) {
    var start = Date.now()
    while (!idle()) {
      if (Date.now() - start > timeoutMs) fail("the lanes did not settle in " + timeoutMs + " ms: " + service.stateJson())
      wait(5)
    }
  }

  // GET on the runner, answered as JSON.
  function fetch(path) {
    var xhr = new XMLHttpRequest()
    var reply = null
    xhr.onreadystatechange = function() {
      if (xhr.readyState === XMLHttpRequest.DONE) reply = xhr.responseText
    }
    xhr.open("GET", BenchConfig.RUNNER_URL + path)
    xhr.send()
    var start = Date.now()
    while (reply === null) {
      if (Date.now() - start > 10000) fail("the runner did not answer " + path)
      wait(1)
    }
    return JSON.parse(reply)
  }

  function median(values) {
    var sorted = values.slice().sort(function(a, b) { return a - b })
    return sorted.length ? sorted[Math.floor((sorted.length - 1) / 2)] : null
  }

  function test_panel_open() {
    service = serviceComponent.createObject(testCase, {
      "settings": { "autoCheck": "Off", "notifyErrors": "Off", "notifyUpdates": "Off" },
      "localDeadlineMs": 600000, "networkDeadlineMs": 600000
    })
    verify(service !== null)
    service.requestFinished.connect(onFinished)
    // The service's start: its first status and what follows it (checkpoints, reads).
    waitIdle(120000)
    compare(service.phase, "ok", "the service reached the binary: " + service.stateJson())

    // The hop to the runner and back, which every read below pays once.
    var hops = []
    for (var h = 0; h < 20; h++) {
      var t = Date.now()
      fetch("/now")
      hops.push(Date.now() - t)
    }
    fetch("/clear")

    var opens = []
    for (var i = 0; i < BenchConfig.OPENS; i++) {
      finished = []
      card = cardComponent.createObject(testCase, {})
      verify(card !== null)
      var frame = card.view
      frame.svc = service
      var t0 = Date.now()
      service.setPanelOpen("bench", true)
      frame.opened = true
      waitForRendering(card)
      var rendered = Date.now() - t0
      // Every read the open made, done (1n2p.38: the open asks only what it needs: the
      // cached status, the shown view's read when its data is old, and the first open's
      // quiet reads and scan), each by its first answer.
      waitIdle(120000)
      var reads = []
      var firsts = {}
      for (var f = 0; f < finished.length; f++) {
        var r = finished[f]
        if (firsts[r.resource]) continue
        firsts[r.resource] = true
        reads.push({ "resource": r.resource, "outcome": r.outcome, "done_ms": r.at - t0 })
      }
      var dataMs = reads.length ? Math.max.apply(null, reads.map(function(x) { return x.done_ms })) : 0
      opens.push({ "open": i + 1, "rendered_ms": rendered, "data_ms": dataMs, "reads": reads,
        "rows": frame.rowCount, "view": frame.view })
      service.setPanelOpen("bench", false)
      card.destroy()
      card = null
      wait(20)
      waitIdle(120000)
    }
    var runs = fetch("/log")
    var warm = opens.slice(1)
    console.log("WIDGET_BENCH " + JSON.stringify({
      "kind": "open",
      "opens": opens,
      "runner": runs,
      "hop_ms_median": median(hops),
      "summary": {
        "first_open_data_ms": opens[0].data_ms,
        "first_open_rendered_ms": opens[0].rendered_ms,
        "data_ms_median": median(warm.map(function(o) { return o.data_ms })),
        "data_ms_max": Math.max.apply(null, warm.map(function(o) { return o.data_ms })),
        "rendered_ms_median": median(warm.map(function(o) { return o.rendered_ms })),
        "rendered_ms_max": Math.max.apply(null, warm.map(function(o) { return o.rendered_ms }))
      }
    }))
    service.destroy()
    service = null
  }
}
