import QtQuick
import qs.Commons
import qs.Commons as Commons
import ".."
import "../Model.js" as Model

// A tab's count as a pill (design doc §2.1, §5.5): caption, dim on a muted border; an
// urgent count has an urgent border and bold foreground figures, so it never says
// "urgent" by colour alone (§5.1). Nothing at zero.
Rectangle {
  id: root

  property int count: 0
  property bool urgent: false

  property color foreground: Commons.Color.foreground
  property color dim: Model.dimColor(foreground, Commons.Color.popups.background)
  property color muted: Commons.Color.muted
  property color urgentColor: Commons.Color.urgent
  property string fontFamily: Style.font.family

  visible: count > 0
  implicitWidth: Math.max(implicitHeight, label.implicitWidth + Style.spacing.sm * 2)
  implicitHeight: label.implicitHeight + Style.spacing.xs * 2
  radius: implicitHeight / 2
  color: "transparent"
  border.width: 1
  border.color: urgent ? urgentColor : muted

  Accessible.role: Accessible.StaticText
  Accessible.name: urgent ? Model.copy("tab.count.urgent", { "count": count }) : String(count)

  PlainLabel {
    id: label
    objectName: "countText"
    anchors.centerIn: parent
    elide: Text.ElideNone
    color: root.urgent ? root.foreground : root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.bold: root.urgent
    text: String(root.count)
  }
}
