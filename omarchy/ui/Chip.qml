import QtQuick
import qs.Commons
import qs.Commons as Commons
import ".."
import "../Model.js" as Model

// A chip (design doc §5.5): a client, a source, a channel or a state word, in a 1 px
// muted border. An urgent chip carries the alert glyph, so it never says "urgent" by
// colour alone (§5.1).
Rectangle {
  id: root

  property string text: ""
  property bool urgent: false

  property color foreground: Commons.Color.foreground
  property color dim: Model.dimColor(foreground, Commons.Color.popups.background)
  property color muted: Commons.Color.muted
  property color urgentColor: Commons.Color.urgent
  property string fontFamily: Style.font.family

  implicitWidth: row.implicitWidth + Style.spacing.sm * 2
  implicitHeight: row.implicitHeight + Style.spacing.xs * 2
  radius: Style.cornerRadius
  color: "transparent"
  border.width: 1
  border.color: urgent ? urgentColor : muted

  Accessible.role: Accessible.StaticText
  Accessible.name: text

  Row {
    id: row
    anchors.centerIn: parent
    spacing: Style.spacing.xs

    Text {
      objectName: "chipGlyph"
      visible: root.urgent
      textFormat: Text.PlainText
      // U+F0028 nf-md-alert_circle.
      text: String.fromCodePoint(0xF0028)
      color: root.urgentColor
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }

    PlainLabel {
      objectName: "chipText"
      color: root.urgent ? root.foreground : root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      elide: Text.ElideNone
      text: root.text
    }
  }
}
