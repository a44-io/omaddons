import QtQuick
import QtTest
import Quickshell.Io
import qs.Commons
import "../.."
import "../../Build.js" as Build
import "../../Model.js" as Model
import "../../Store.js" as Store

// BarWidget.qml against a stub service (pipeline step 4): it binds (serviceFor, the
// store, the retry), pushes settings and its open state, draws the service's bar model
// (Model.barState: the glyph, the spinner, the badge beside the glyph, the mark) and
// turns clicks into service calls. Two copies stand for two monitors.
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

  readonly property string gamepad: String.fromCodePoint(0xF0297)
  readonly property string sync: String.fromCodePoint(0xF04E6)

  // A bar model as Model.barState makes it: an idle one, with `extra` over it.
  function barModel(extra) {
    var b = { "state": "idle", "badge": 0, "badgeUrgent": false, "urgent": false, "dimmed": false, "spinning": false,
      "playing": false, "mark": "", "markUrgent": false, "tooltip": ["All good"] }
    for (var key in extra) b[key] = extra[key]
    return b
  }

  Component {
    id: barComponent
    BarWidget {}
  }

  // The shared service as the bar sees it.
  Component {
    id: serviceComponent
    QtObject {
      property var bar: testCase.barModel({})
      property var pushed: []
      property var panels: ({})
      property int checks: 0
      property string requestedView: ""
      // A panel opened by mouse (the first run's keybinding tip follows it, 1n2p.32).
      property bool openedByMouse: false
      // What the middle click reads to pick the most urgent view (design doc §2.5).
      property var summary: ({ "errors": { "new": 0 }, "issuesUrgent": 1 })

      // The settings applied, as the real service keeps them (the settings sheet reads them).
      property var settings: ({})
      function applySettings(next) {
        pushed = pushed.concat([next])
        settings = next
      }
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

  // What the bar shows for `model`: [glyph, turning, active, dimmed, badge shown, its
  // count, urgent badge, mark glyph ("" for none), urgent mark].
  function look(widget) {
    var badge = part(widget, "badge")
    var mark = part(widget, "mark")
    var button = part(widget, "button")
    return [button.text === sync ? "sync" : button.text === gamepad ? "gamepad" : button.text, widget.turning, button.active,
      button.dimmed, badge.visible, badge.visible ? part(widget, "badgeCount").text : "", badge.urgent,
      mark.visible ? part(widget, "markGlyph").text.codePointAt(0) : 0, mark.visible && Qt.colorEqual(mark.color, button.activeColor)]
  }

  function test_the_bar_draws_the_service_model() {
    var widget = make()
    compare(widget.svc, svc)
    compare(widget.moduleName, Build.PLUGIN_ID)
    compare(widget.ipcTarget, Build.PLUGIN_ID)
    compare(widget.manageIpc, false)
    // Idle: the gamepad, plain and still, no badge, no mark.
    compare(look(widget), ["gamepad", false, false, false, false, "", false, 0, false])
    compare(part(widget, "button").tooltipText, "All good")
    // Updates: the accent badge counts them.
    svc.bar = barModel({ "state": "updates", "badge": 4, "tooltip": ["4 updates", "Checked 3 min ago"] })
    compare(look(widget), ["gamepad", false, false, false, true, "4", false, 0, false])
    compare(part(widget, "button").tooltipText, "4 updates\nChecked 3 min ago")
    svc.bar = barModel({ "state": "updates", "badge": 120 })
    compare(part(widget, "badgeCount").text, "99+")
    // Attention, with or without updates: the urgent glyph, the urgent badge counting
    // what needs attention, and the attention mark.
    svc.bar = barModel({ "state": "attention", "badge": 2, "badgeUrgent": true, "urgent": true, "mark": "attention", "markUrgent": true })
    compare(look(widget), ["gamepad", false, true, false, true, "2", true, 0xF0026, true])
    // Playing: the running mark, in the bar's colour.
    svc.bar = barModel({ "state": "updates", "badge": 4, "playing": true, "mark": "running" })
    compare(look(widget), ["gamepad", false, false, false, true, "4", false, 0xF040A, false])
    // Working: the sync glyph turns in place of the gamepad; the badge stays.
    svc.bar = barModel({ "state": "updates", "badge": 4, "spinning": true })
    compare(look(widget), ["sync", true, false, false, true, "4", false, 0, false])
    // Stale: dimmed, the last numbers kept, the clock mark at full strength.
    svc.bar = barModel({ "state": "stale", "badge": 4, "dimmed": true, "mark": "stale" })
    compare(look(widget), ["gamepad", false, false, true, true, "4", false, 0xF0150, false])
    // Broken: dimmed, the urgent warning mark, no badge.
    svc.bar = barModel({ "state": "broken", "dimmed": true, "mark": "broken", "markUrgent": true })
    compare(look(widget), ["gamepad", false, false, true, false, "", false, 0xF0028, true])
    // Starting and empty: dimmed, nothing else.
    svc.bar = barModel({ "state": "empty", "dimmed": true })
    compare(look(widget), ["gamepad", false, false, true, false, "", false, 0, false])
  }

  function test_the_badge_and_the_mark_sit_beside_the_glyph_and_the_bar_keeps_its_width() {
    var widget = make()
    var width = widget.implicitWidth
    var badge = part(widget, "badge")
    var mark = part(widget, "mark")
    var states = [
      barModel({ "state": "updates", "badge": 4 }),
      barModel({ "state": "updates", "badge": 120, "playing": true, "mark": "running" }),
      barModel({ "state": "attention", "badge": 12, "badgeUrgent": true, "urgent": true, "mark": "attention", "markUrgent": true }),
      barModel({ "state": "stale", "badge": 3, "dimmed": true, "mark": "stale" }),
      barModel({ "state": "broken", "dimmed": true, "mark": "broken", "markUrgent": true }),
      barModel({ "state": "updates", "badge": 4, "spinning": true })
    ]
    for (var i = 0; i < states.length; i++) {
      svc.bar = states[i]
      compare(widget.implicitWidth, width, states[i].state + ": the bar keeps its width")
      var glyphRight = widget.glyphLeft + widget.glyphSize
      if (badge.visible) {
        // Beside the glyph: it overlaps the glyph by a quarter of its own width, no more.
        compare(glyphRight - badge.x, badge.width / 4, states[i].state + ": the badge's overlap")
        verify(!mark.visible || badge.y + badge.height <= mark.y, states[i].state + ": the badge sits above the mark")
      }
      if (mark.visible) {
        compare(glyphRight - mark.x, mark.width / 4, states[i].state + ": the mark's overlap")
        verify(mark.y + mark.height <= widget.height, "the mark stays inside the bar")
      }
    }
  }

  function test_the_spinner_turns_and_stands_still_with_reduced_motion() {
    var widget = make()
    var button = part(widget, "button")
    svc.bar = barModel({ "state": "idle", "spinning": true })
    compare(button.text, sync)
    verify(widget.turning)
    tryVerify(function() { return button.textRotation > 0 }, 1000, "the sync glyph turns")
    svc.bar = barModel({})
    compare(button.text, gamepad)
    compare(button.textRotation, 0)
    // Hyprland's animations off: the sync glyph, standing still (design doc §5.7).
    Style.reduceMotion = true
    try {
      svc.bar = barModel({ "state": "idle", "spinning": true })
      compare(button.text, sync)
      verify(!widget.turning)
      wait(100)
      compare(button.textRotation, 0)
    } finally {
      Style.reduceMotion = false
    }
  }

  function test_with_no_service_yet_the_bar_is_dimmed() {
    Store.set(null)
    var widget = make()
    compare(widget.svc, null)
    verify(part(widget, "button").dimmed)
    compare(widget.model.state, "starting")
    compare(part(widget, "button").tooltipText, Model.barState(null, { "phase": "starting" }).tooltip.join("\n"))
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
    verify(!svc.openedByMouse)
    button.pressed(Qt.LeftButton)
    verify(widget.opened)
    verify(svc.openedByMouse, "a mouse open is recorded for the keybinding tip")
    compare(svc.panels[widget.panelKey], true)
    button.pressed(Qt.LeftButton)
    verify(!widget.opened)
    button.pressed(Qt.RightButton)
    compare(svc.checks, 1)
    verify(!widget.opened)
    button.pressed(Qt.MiddleButton)
    verify(widget.opened)
    // The panel's frame took the most urgent view: a P1 problem, no new error.
    compare(part(widget, "view").view, "issues")
    compare(svc.requestedView, "")
    // New Lua errors come first.
    button.pressed(Qt.LeftButton)
    verify(!widget.opened)
    svc.summary = { "errors": { "new": 2 }, "issuesUrgent": 1 }
    button.pressed(Qt.MiddleButton)
    compare(part(widget, "view").view, "errors")
    // Nothing urgent: Updates.
    button.pressed(Qt.LeftButton)
    verify(!widget.opened)
    svc.summary = { "errors": { "new": 0 }, "issuesUrgent": 0 }
    button.pressed(Qt.MiddleButton)
    verify(widget.opened)
    compare(part(widget, "view").view, "updates")
  }

  // The keybinding tip seen (1n2p.32): the setting is applied, pushed to the service and
  // saved through the shell's own facade, never written by the widget.
  function test_a_setting_the_panel_changes_is_saved_through_the_shell() {
    var api = barApiComponent.createObject(testCase)
    var saved = []
    api.shell = { "serviceFor": function(id) { return svc }, "updateEntryInline": function(id, entry) { saved.push([id, entry]) } }
    var widget = make({ "settings": { "panelWidth": 600 } })
    widget.bar = api
    part(widget, "view").settingRequested("keyHint", "Off")
    compare(widget.settings.keyHint, "Off")
    compare(widget.settings.panelWidth, 600, "the other settings stay")
    compare(svc.pushed[svc.pushed.length - 1].keyHint, "Off")
    compare(saved.length, 1)
    compare(saved[0][0], Build.PLUGIN_ID)
    compare([saved[0][1].id, saved[0][1].keyHint, saved[0][1].panelWidth], [Build.PLUGIN_ID, "Off", 600])
    api.destroy()
  }

  // 1n2p.31: the settings sheet changes the panel's own settings without a terminal. Each
  // change goes through the shell's facade (`updateEntryInline`, which writes shell.json),
  // reaches the service, and the sheet shows what was applied; the widget writes nothing.
  function test_the_settings_sheet_saves_each_change_through_the_shell() {
    var api = barApiComponent.createObject(testCase)
    var saved = []
    api.shell = { "serviceFor": function(id) { return svc }, "updateEntryInline": function(id, entry) { saved.push([id, entry]) } }
    var widget = make({ "settings": { "panelWidth": 540, "autoCheck": "On", "keyHint": "Off" } })
    widget.bar = api
    part(widget, "button").pressed(Qt.LeftButton)
    var view = part(widget, "view")
    view.settingsOpen = true
    var sheet = part(widget, "settings")
    verify(sheet.opened)
    compare(JSON.stringify(sheet.rows.map(function(r) { return r.key })), JSON.stringify(Model.PANEL_SETTING_KEYS))
    // Width: Enter steps it on; l on again; h back.
    sheet.choose(0)
    compare([saved[saved.length - 1][1].panelWidth, widget.settings.panelWidth, sheet.rows[0].text], [560, 560, "560 px"])
    sheet.stepped(0, 1)
    sheet.stepped(0, -1)
    compare(JSON.stringify(saved.map(function(s) { return s[1].panelWidth })), JSON.stringify([560, 580, 560]))
    // A switch flips, and the other settings stay as they were.
    sheet.choose(2)
    compare([saved[saved.length - 1][1].autoCheck, saved[saved.length - 1][1].keyHint, svc.settings.autoCheck], ["Off", "Off", "Off"])
    compare(sheet.rows[2].text, "Off")
    // The binary's path is typed: a full path or nothing; anything else says what a path is.
    var count = saved.length
    sheet.choose(6)
    compare(sheet.editing, "binary")
    var field = part(widget, "settingEdit")
    field.text = "omaddons"
    sheet.commit()
    compare([saved.length, sheet.editing, sheet.editError], [count, "binary", Model.copy("settings.panel.binary.invalid")])
    field.text = " /opt/omaddons-dev/omaddons "
    sheet.commit()
    compare([saved.length, saved[saved.length - 1][1].binary, sheet.editing], [count + 1, "/opt/omaddons-dev/omaddons", ""])
    compare(saved.every(function(s) { return s[0] === Build.PLUGIN_ID }), true)
    api.destroy()
  }

  function test_the_wheel_does_nothing() {
    var widget = make()
    var button = part(widget, "button")
    button.wheelMoved(120)
    button.wheelMoved(-120)
    verify(!widget.opened)
    compare(svc.checks, 0)
    compare(svc.requestedView, "")
    compare(Object.keys(svc.panels).length, 0)
  }
}
