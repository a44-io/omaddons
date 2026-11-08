import QtQuick

// Omarchy's qs.Ui PanelSeparator: a 1 px line in the foreground at `strength`.
Rectangle {
  property color foreground: "#cacccc"
  property real strength: 0.12

  width: parent ? parent.width : implicitWidth
  implicitWidth: 100
  implicitHeight: 1
  height: 1
  color: Qt.rgba(foreground.r, foreground.g, foreground.b, strength)
}
