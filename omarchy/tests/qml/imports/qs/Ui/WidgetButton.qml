import QtQuick

// Omarchy's qs.Ui WidgetButton: the properties the bar widget binds, `pressed` and
// `wheelMoved`.
Item {
  property var bar: null
  property string text: ""
  property string fontFamily: "monospace"
  property real fontSize: 12
  // As the kit binds them: the bar's colors, so a theme switch recolors the button.
  property color foreground: bar && bar.barForeground !== undefined ? bar.barForeground : "#cacccc"
  property color activeColor: bar && bar.urgent !== undefined ? bar.urgent : "#e06c75"
  property bool active: false
  property bool dimmed: false
  property bool labelVisible: true
  property bool hasVisualContent: text !== ""
  property string tooltipText: ""
  property real textRotation: 0

  signal pressed(int button)
  signal wheelMoved(int delta)

  implicitWidth: 27
  implicitHeight: 26
}
