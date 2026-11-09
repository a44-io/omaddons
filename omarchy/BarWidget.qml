import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model
import "Store.js" as Store
import "Build.js" as Build

// The bar button: one per monitor, owning nothing. It binds to the shared service
// (M3-D5), pushes its settings into it (services get none), tells it whether this
// copy's panel is open, and draws the service's `bar` model: the badge, the urgent
// color, dimmed, the spinner and the tooltip. Its keyboard panel holds the frame
// (PanelView.qml). Every action is a service call.
Panel {
  id: root
  moduleName: Build.PLUGIN_ID
  ipcTarget: Build.PLUGIN_ID
  // The service owns the IPC target: only one handler per target works.
  manageIpc: false

  property var svc: null
  // Until a service answers, the bar shows the model of no answer (dimmed).
  readonly property var model: svc && svc.bar ? svc.bar : Model.reduceStatus(null).bar
  readonly property bool spinning: model.spinning === true
  // This copy's key in the service's set of open panels.
  readonly property string panelKey: Build.PLUGIN_ID + "@" + String(root)

  // U+F0297 nf-md-gamepad_variant and U+F04E6 nf-md-sync (JetBrainsMono Nerd Font),
  // written as code points so the source survives editors that mangle private-use
  // characters.
  readonly property string glyph: String.fromCodePoint(0xF0297)
  readonly property string syncGlyph: String.fromCodePoint(0xF04E6)

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
  // the Issues view.
  function press(button) {
    if (button === Qt.RightButton) {
      if (svc) svc.check()
      return
    }
    if (button === Qt.MiddleButton) {
      if (svc) svc.requestedView = "issues"
      root.open()
      return
    }
    root.toggle()
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

  WidgetButton {
    id: button
    objectName: "button"
    anchors.fill: parent
    bar: root.bar
    text: root.glyph
    // While a change runs, the spinner below takes the glyph's place.
    labelVisible: !root.spinning
    hasVisualContent: true
    active: root.model.urgent === true
    dimmed: root.model.dimmed === true
    tooltipText: Array.isArray(root.model.tooltip) ? root.model.tooltip.join("\n") : ""
    onPressed: function(which) { root.press(which) }
  }

  // WidgetButton has no spinner (Button's iconSpinning is not its), so the turning
  // sync glyph is drawn here, in the button's own color and size.
  Text {
    id: spinner
    objectName: "spinner"
    visible: root.spinning
    anchors.centerIn: button
    textFormat: Text.PlainText
    text: root.syncGlyph
    color: button.active ? button.activeColor : button.foreground
    font.family: button.fontFamily
    font.pixelSize: button.fontSize
    opacity: button.opacity

    RotationAnimation on rotation {
      running: spinner.visible
      from: 0
      to: 360
      duration: 1200
      loops: Animation.Infinite
    }
  }

  // The count, drawn over the icon so the bar does not change width (the
  // notification-center pattern).
  Rectangle {
    id: badge
    objectName: "badge"
    visible: badgeLabel.text !== ""
    anchors.right: button.right
    anchors.rightMargin: Style.space(1)
    anchors.top: button.top
    anchors.topMargin: Style.space(3)
    width: Math.max(badgeLabel.implicitWidth + Style.space(6), Style.space(12))
    height: Style.space(12)
    radius: height / 2
    color: Commons.Color.accent

    Text {
      id: badgeLabel
      objectName: "badgeLabel"
      anchors.centerIn: parent
      textFormat: Text.PlainText
      text: Model.badgeText(root.model.badge)
      font.family: button.fontFamily
      font.pixelSize: Style.font.caption - Style.space(2)
      font.bold: true
      color: Commons.Color.background
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
      : panel.fittedContentWidth(Style.space(Model.intSetting(root.settings, "panelWidth", 540, 400, 800)))
    contentHeight: view.zoomed ? panel.fittedContentHeight(999999)
      : panel.fittedContentHeight(view.contentImplicitHeight, Style.space(720))

    PanelView {
      id: view
      objectName: "view"
      anchors.fill: parent
      svc: root.svc
      opened: root.opened
      foreground: root.bar ? root.bar.foreground : Commons.Color.foreground
      urgent: root.bar ? root.bar.urgent : Commons.Color.urgent
      fontFamily: root.bar ? root.bar.fontFamily : Style.font.family
      onCloseRequested: root.close()
      onTabRequested: function(direction) { root.switchPanel(direction) }
    }
  }
}
