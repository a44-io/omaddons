import QtQuick

// Omarchy's qs.Ui CursorSurface: a row's cursor highlight, from `hasCursor`.
BorderSurface {
  property bool hasCursor: false
  property bool current: false
  property bool outline: false
  property bool bordered: false
  property color foreground: "#cacccc"
  property color accent: "#7aa2f7"

  color: hasCursor ? Qt.rgba(foreground.r, foreground.g, foreground.b, 0.1) : "transparent"
}
