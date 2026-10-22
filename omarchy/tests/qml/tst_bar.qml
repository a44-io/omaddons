import QtQuick
import QtTest
import Quickshell.Io
import "../.."
import "../../Build.js" as Build
import "../../Store.js" as Store

// BarWidget.qml against a stub service (pipeline step 4): it binds (serviceFor, the
// store, the retry), pushes settings and its open state, draws the service's bar model
// and turns clicks into service calls. Two copies stand for two monitors.
TestCase {
  id: testCase
  name: "Bar"
  // Visibility is only real in a shown window, and a TestCase hides its children
  // unless it is visible itself.
  when: windowShown
  visible: true
  width: 400
  height: 100

  property var svc: null
  property var widgets: []

  Component {
    id: barComponent
    BarWidget {}
  }

  // The shared service as the bar sees it.
  Component {
    id: serviceComponent
    QtObject {
      property var bar: ({ "badge": 0, "urgent": false, "dimmed": false, "spinning": false, "tooltip": ["Everything is up to date"] })
      property var pushed: []
      property var panels: ({})
      property int checks: 0
      property string requestedView: ""

      function applySettings(settings) { pushed = pushed.concat([settings]) }
      function setPanelOpen(key, open) {
        var next = {}
        for (var k in panels) if (k !== key) next[k] = panels[k]
        if (open) next[key] = true
        panels = next
      }
      function check() { checks++ }
      function readView(view) {}
    }
  }

  // The PluginBarApi facade the shell injects: `shell.serviceFor`.
  Component {
    id: barApiComponent
    QtObject {
      id: api
      property var service: null
      property var asked: []
      property var shell: null
      property color foreground: "#cacccc"
      property color urgent: "#e06c75"
      property string fontFamily: "monospace"
      Component.onCompleted: shell = {
        "serviceFor": function(id) {
          api.asked = api.asked.concat([id])
          return api.service
        }
      }
    }
  }

  function make(props) {
    var widget = barComponent.createObject(testCase, props || {})
    verify(widget !== null)
    widgets = widgets.concat([widget])
    return widget
  }

  function init() {
    ProcessRegistry.reset()
    svc = serviceComponent.createObject(testCase)
    Store.set(svc)
  }

  function cleanup() {
    for (var i = 0; i < widgets.length; i++) widgets[i].destroy()
    widgets = []
    svc.destroy()
    svc = null
    Store.set(null)
    wait(0)
  }

  function part(widget, name) {
    var found = findChild(widget, name)
    verify(found !== null, name)
    return found
  }

  function test_the_bar_draws_the_service_model() {
    var widget = make()
    compare(widget.svc, svc)
    compare(widget.moduleName, Build.PLUGIN_ID)
    compare(widget.ipcTarget, Build.PLUGIN_ID)
    compare(widget.manageIpc, false)
    // Idle: no badge, plain, still.
    verify(!part(widget, "badge").visible)
    verify(!part(widget, "button").active)
    verify(!part(widget, "button").dimmed)
    verify(!part(widget, "spinner").visible)
    verify(part(widget, "button").labelVisible)
    compare(part(widget, "button").text, String.fromCodePoint(0xF0297))
    compare(part(widget, "button").tooltipText, "Everything is up to date")
    // Updates.
    svc.bar = { "badge": 1, "urgent": false, "dimmed": false, "spinning": false, "tooltip": ["1 update on 1 client (retail 1)", "checked 3 min ago"] }
    verify(part(widget, "badge").visible)
    compare(part(widget, "badgeLabel").text, "1")
    compare(part(widget, "button").tooltipText, "1 update on 1 client (retail 1)\nchecked 3 min ago")
    svc.bar = { "badge": 120, "urgent": false, "dimmed": false, "spinning": false, "tooltip": [] }
    compare(part(widget, "badgeLabel").text, "99+")
    // Attention or Lua errors: the urgent color.
    svc.bar = { "badge": 0, "urgent": true, "dimmed": false, "spinning": false, "tooltip": ["retail: Baz's folder is gone"] }
    verify(part(widget, "button").active)
    verify(!part(widget, "badge").visible)
    // Missing, unreachable or incompatible: dimmed.
    svc.bar = { "badge": 3, "urgent": false, "dimmed": true, "spinning": false, "tooltip": [], "stale": true }
    verify(part(widget, "button").dimmed)
    // A change running: the sync glyph turns in place of the gamepad.
    svc.bar = { "badge": 3, "urgent": false, "dimmed": false, "spinning": true, "tooltip": [] }
    verify(part(widget, "spinner").visible)
    verify(!part(widget, "button").labelVisible)
    compare(part(widget, "spinner").text, String.fromCodePoint(0xF04E6))
  }

  function test_with_no_service_yet_the_bar_is_dimmed() {
    Store.set(null)
    var widget = make()
    compare(widget.svc, null)
    verify(part(widget, "button").dimmed)
  }

  function test_serviceFor_binds_once_the_bar_arrives() {
    Store.set(null)
    var widget = make()
    compare(widget.svc, null)
    var api = barApiComponent.createObject(testCase)
    api.service = svc
    widget.bar = api
    compare(widget.svc, svc)
    compare(api.asked, [Build.PLUGIN_ID])
    api.destroy()
  }

  function test_the_bar_retries_until_a_service_appears() {
    Store.set(null)
    var widget = make()
    wait(50)
    compare(widget.svc, null)
    Store.set(svc)
    wait(300)
    compare(widget.svc, svc)
  }

  function test_two_copies_bind_one_service_and_start_nothing() {
    var left = make({ "settings": { "panelWidth": 600 } })
    var right = make({ "settings": { "panelWidth": 640 } })
    compare(left.svc, svc)
    compare(right.svc, svc)
    compare(svc.pushed, [{ "panelWidth": 600 }, { "panelWidth": 640 }])
    right.settings = { "panelWidth": 700 }
    compare(svc.pushed[svc.pushed.length - 1], { "panelWidth": 700 })
    compare(ProcessRegistry.launches.length, 0)
    compare(ProcessRegistry.processes.length, 0)
    verify(!left.manageIpc && !right.manageIpc)
  }

  function test_each_copy_reports_its_own_panel() {
    var left = make()
    var right = make()
    verify(left.panelKey !== right.panelKey)
    left.open()
    right.open()
    compare(Object.keys(svc.panels).length, 2)
    left.close()
    compare(Object.keys(svc.panels), [right.panelKey])
    right.destroy()
    widgets = [left]
    wait(0)
    compare(Object.keys(svc.panels).length, 0)
  }

  function test_clicks_toggle_check_and_open_issues() {
    var widget = make()
    var button = part(widget, "button")
    button.pressed(Qt.LeftButton)
    verify(widget.opened)
    compare(svc.panels[widget.panelKey], true)
    button.pressed(Qt.LeftButton)
    verify(!widget.opened)
    button.pressed(Qt.RightButton)
    compare(svc.checks, 1)
    verify(!widget.opened)
    button.pressed(Qt.MiddleButton)
    verify(widget.opened)
    // The panel's frame took the requested view.
    compare(part(widget, "view").view, "issues")
    compare(svc.requestedView, "")
  }
}
