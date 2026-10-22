import QtQuick

// Omarchy's qs.Ui WidgetButton: the properties the bar widget binds and `pressed`.
Item {
  property var bar: null
  property string text: ""
  property string fontFamily: "monospace"
  property real fontSize: 12
  property color foreground: "#cacccc"
  property color activeColor: "#e06c75"
  property bool active: false
  property bool dimmed: false
  property bool labelVisible: true
  property bool hasVisualContent: text !== ""
  property string tooltipText: ""

  signal pressed(int button)

  implicitWidth: 27
  implicitHeight: 26
}
