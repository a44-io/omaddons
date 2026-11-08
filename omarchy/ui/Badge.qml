import QtQuick
import qs.Commons
import qs.Commons as Commons
import ".."

// The bar button's count pill (design doc §2.5, §5.5): the reason for the button's
// colour (urgent: new Lua errors and P0/P1 problems; else pending updates). The
// urgent fill or the accent fill, the count in the background colour. Nothing to count,
// no badge. The bar-button bead places it beside the glyph.
Rectangle {
  id: root

  property int count: 0
  property bool urgent: false

  property color background: Commons.Color.background
  property color urgentColor: Commons.Color.urgent
  property color accent: Commons.Color.accent
  property string fontFamily: Style.font.family

  readonly property string label: count > 99 ? "99+" : String(count)

  visible: count > 0
  implicitWidth: Math.max(implicitHeight, countLabel.implicitWidth + Style.space(6))
  implicitHeight: Math.max(Style.space(12), countLabel.implicitHeight)
  radius: height / 2
  color: urgent ? urgentColor : accent

  Accessible.role: Accessible.StaticText
  Accessible.name: label + (urgent ? " needing attention" : " updates")

  PlainLabel {
    id: countLabel
    objectName: "badgeCount"
    anchors.centerIn: parent
    elide: Text.ElideNone
    text: root.label
    color: root.background
    font.family: root.fontFamily
    font.pixelSize: Math.max(8, Style.font.caption - Style.space(2))
    font.bold: true
  }
}
