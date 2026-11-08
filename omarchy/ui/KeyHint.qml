import QtQuick
import qs.Commons
import qs.Commons as Commons
import ".."
import "../Model.js" as Model

// A key cap drawn inline ("u", "Ctrl+Z"), as the help overlay and empty states show
// keys (design doc §3, §5.5).
Rectangle {
  id: root

  property string key: ""

  property color foreground: Commons.Color.foreground
  property color dim: Model.dimColor(foreground, Commons.Color.popups.background)
  property color muted: Commons.Color.muted
  property string fontFamily: Style.font.family

  implicitWidth: Math.max(implicitHeight, label.implicitWidth + Style.spacing.sm * 2)
  implicitHeight: label.implicitHeight + Style.spacing.xs * 2
  radius: Style.cornerRadius
  color: "transparent"
  border.width: 1
  border.color: muted

  Accessible.role: Accessible.StaticText
  Accessible.name: "key " + key

  PlainLabel {
    id: label
    objectName: "keyText"
    anchors.centerIn: parent
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.bold: true
    elide: Text.ElideNone
    text: root.key
  }
}
