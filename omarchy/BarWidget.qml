import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model
import "Store.js" as Store
import "Build.js" as Build
import "ui"

// The bar button: one per monitor, owning nothing. It binds to the shared service
// (M3-D5), pushes its settings into it (services get none), tells it whether this
// copy's panel is open, and draws the service's `bar` model (Model.barState, design doc
// §2.5): the glyph in the urgent colour or dimmed, the spinner, the badge beside the
// glyph, the broken, stale or running mark under it, and the tooltip. Its keyboard
// panel holds the frame (PanelView.qml). Every action is a service call.
Panel {
  id: root
  moduleName: Build.PLUGIN_ID
  ipcTarget: Build.PLUGIN_ID
  // The service owns the IPC target: only one handler per target works.
  manageIpc: false

  property var svc: null
  // Until a service answers, the bar shows the model of no answer yet (dimmed).
  readonly property var model: svc && svc.bar ? svc.bar : Model.barState(null, { "phase": "starting" })
  readonly property bool spinning: model.spinning === true
  // The sync glyph turns while work runs; with Hyprland's animations off
  // (`Style.reduceMotion`) it stands still (design doc §5.7).
  readonly property bool turning: spinning && Style.reduceMotion !== true
  property real spinAngle: 0
  // This copy's key in the service's set of open panels.
  readonly property string panelKey: Build.PLUGIN_ID + "@" + String(root)

  // U+F0297 nf-md-gamepad_variant and U+F04E6 nf-md-sync (JetBrainsMono Nerd Font),
  // written as code points so the source survives editors that mangle private-use
  // characters.
  readonly property string glyph: String.fromCodePoint(0xF0297)
  readonly property string syncGlyph: String.fromCodePoint(0xF04E6)
  // The mark's glyph ("" for none): broken, stale, attention or a client running.
  readonly property string markGlyph: Model.BAR_MARK_GLYPHS.hasOwnProperty(model.mark)
    ? String.fromCodePoint(Model.BAR_MARK_GLYPHS[model.mark]) : ""

  // Where the kit draws the glyph: its canvas, centred in the button (BarIconButton).
  readonly property real glyphSize: button.canvasSize
  readonly property real glyphLeft: button.x + (button.width - glyphSize) / 2
  readonly property real glyphTop: button.y + (button.height - glyphSize) / 2

  // The service: `serviceFor` once the shell has handed this copy its `bar`, else the
  // store the service set when it completed (a copy completes before `bar` is set, F0).
  function bindService() {
    var host = root.bar && root.bar.shell ? root.bar.shell : null
    var found = host && typeof host.serviceFor === "function" ? host.serviceFor(Build.PLUGIN_ID) : null
    if (!found) found = Store.get()
    if (!found || found === svc) return
    svc = found
    pushSettings()
    pushOpen()
  }

  function pushSettings() {
    if (svc) svc.applySettings(root.settings)
  }

  function pushOpen() {
    if (svc) svc.setPanelOpen(root.panelKey, root.opened)
  }

  // Left toggles the panel, right checks for updates now (vision §5.11), middle opens
  // the most urgent view: Errors with new Lua errors, else Issues with a P0 or P1
  // problem, else Updates (design doc §2.5). The wheel does nothing: a scroll over the
  // bar while the game has focus nearby must never change anything.
  function press(button) {
    if (button === Qt.RightButton) {
      if (svc) svc.check()
      return
    }
    if (button === Qt.MiddleButton) {
      if (svc) svc.requestedView = Model.urgentView(svc.summary)
      if (svc) svc.openedByMouse = true
      root.open()
      return
    }
    // Opened by mouse: the first run's keybinding tip may follow (1n2p.32).
    if (!root.opened && svc) svc.openedByMouse = true
    root.toggle()
  }

  // A setting the panel changes (the keybinding tip seen, 1n2p.32): applied here first,
  // then saved through Omarchy's shell (`updateEntryInline`, as the shell's own widgets
  // save theirs), which writes shell.json; the widget writes no file.
  function saveSetting(key, value) {
    var entry = { "id": root.moduleName }
    // Saved under the names this widget reads (a renamed setting moves, 1n2p.35).
    var current = Model.migrateSettings(root.settings)
    for (var name in current) if (name !== "id") entry[name] = current[name]
    entry[key] = value
    root.settings = entry
    if (root.bar && root.bar.shell && typeof root.bar.shell.updateEntryInline === "function")
      root.bar.shell.updateEntryInline(root.moduleName, entry)
  }

  Component.onCompleted: bindService()
  Component.onDestruction: if (svc) svc.setPanelOpen(root.panelKey, false)
  onBarChanged: bindService()
  onSettingsChanged: pushSettings()
  onOpenedChanged: pushOpen()

  // Until bound (M3-D5).
  Timer {
    interval: 200
    repeat: true
    running: root.svc === null
    onTriggered: root.bindService()
  }

  visible: true
  implicitWidth: button.implicitWidth
  implicitHeight: button.implicitHeight

  // The glyph, optically centred in the kit's icon slot like the shell's own icons;
  // while work runs, the sync glyph in its place, turning (one turn per 1.2 s).
  BarIconButton {
    id: button
    objectName: "button"
    anchors.fill: parent
    bar: root.bar
    text: root.spinning ? root.syncGlyph : root.glyph
    textRotation: root.turning ? root.spinAngle : 0
    active: root.model.urgent === true
    dimmed: root.model.dimmed === true
    tooltipText: Array.isArray(root.model.tooltip) ? root.model.tooltip.join("\n") : ""
    onPressed: function(which) { root.press(which) }
  }

  NumberAnimation on spinAngle {
    running: root.turning
    from: 0
    to: 360
    duration: 1200
    loops: Animation.Infinite
  }

  // The count (design doc §2.5, §5.5): beside the glyph, overlapping it by a quarter of
  // its own width, in the urgent colour for what needs attention, else the accent for
  // updates. It is drawn past the button's edge, never laid out, so the bar keeps its
  // width in every state.
  Badge {
    id: badge
    objectName: "badge"
    count: Math.max(0, Math.floor(Number(root.model.badge) || 0))
    urgent: root.model.badgeUrgent === true
    urgentColor: button.activeColor
    fontFamily: button.fontFamily
    x: root.glyphLeft + root.glyphSize - width / 4
    y: Math.max(0, root.glyphTop - height / 4)
    opacity: button.opacity
  }

  // The mark (design doc §2.5): broken (no omaddons the panel can use), stale (no
  // answer for a while), attention (new Lua errors or a problem) or a client running, a
  // small disc under the badge. It stays at full strength over a dimmed glyph, so the
  // reason for the dimming shows.
  Rectangle {
    id: mark
    objectName: "mark"
    visible: root.markGlyph !== "" && button.opacity > 0
    width: Style.space(10)
    height: width
    radius: width / 2
    x: root.glyphLeft + root.glyphSize - width / 4
    // At the glyph's foot, below the badge when one shows, inside the bar.
    y: Math.max(0, Math.min(root.height - height,
      Math.max(badge.visible ? badge.y + badge.height : 0, root.glyphTop + root.glyphSize - height * 3 / 4)))
    color: root.model.markUrgent === true ? button.activeColor : button.foreground

    Text {
      objectName: "markGlyph"
      anchors.centerIn: parent
      textFormat: Text.PlainText
      text: root.markGlyph
      color: Commons.Color.background
      font.family: button.fontFamily
      font.pixelSize: Style.space(8)
    }
  }

  // The keyboard panel (the house pattern): `panelWidth` wide, at most 720 tall, the
  // frame inside. Zoomed (`z`, design doc §5.6), the fitted helpers get a size no
  // screen reaches and land on the screen less the bar and margins; it is the frame's
  // state, in memory only.
  KeyboardPanel {
    id: panel
    objectName: "panel"
    anchorItem: button
    owner: root
    bar: root.bar
    open: root.opened
    focusTarget: view.focusItem
    contentWidth: view.zoomed ? panel.fittedContentWidth(999999)
      : panel.fittedContentWidth(Style.space(Model.intSetting(Model.migrateSettings(root.settings), "panelWidth", 540, 400, 800)))
    contentHeight: view.zoomed ? panel.fittedContentHeight(999999)
      : panel.fittedContentHeight(view.contentImplicitHeight, Style.space(720))

    PanelView {
      id: view
      objectName: "view"
      anchors.fill: parent
      svc: root.svc
      opened: root.opened
      // The most the panel may take, as contentHeight's cap: a long list fills it.
      heightCap: view.zoomed ? panel.fittedContentHeight(999999) : panel.fittedContentHeight(999999, Style.space(720))
      foreground: root.bar ? root.bar.foreground : Commons.Color.foreground
      urgent: root.bar ? root.bar.urgent : Commons.Color.urgent
      fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }
      onSettingRequested: function(key, value) { root.saveSetting(key, value) }
    }
  }
}
