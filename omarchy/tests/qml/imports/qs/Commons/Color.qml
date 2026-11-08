pragma Singleton
import QtQuick

QtObject {
  property color foreground: "#cacccc"
  property color background: "#101315"
  property color accent: "#7aa2f7"
  property color urgent: "#e06c75"
  property color muted: "#707880"

  readonly property QtObject popups: QtObject {
    property color background: "#101315"
    property color text: "#cacccc"
    property color border: "#7aa2f7"
  }
}
