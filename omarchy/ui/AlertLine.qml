import QtQuick
import qs.Commons
import qs.Commons as Commons
import ".."
import "../Model.js" as Model

// A line that may be an alert (1n2p.40, design doc §5.1), as a chip carries one: marked,
// its words in the foreground beside an urgent glyph, so urgent stays a mark's colour
// (as text it falls below 4.5:1 on some themes) and the alert is never told by colour
// alone; unmarked, dim words and no glyph ("Reading…" before "Couldn't read…"). The
// words wrap beside the glyph, and come from the CLI or COPY as plain text.
Item {
  id: root

  property string text: ""
  property bool marked: true
  // U+F0028 nf-md-alert_circle (§5.4: failed, refused); U+F0026 nf-md-alert for what
  // needs attention.
  property int glyph: 0xF0028
  property bool bold: false
  property real pixelSize: Style.font.caption
  property int wrapMode: Text.WordWrap

  property color foreground: Commons.Color.foreground
  property color dim: Model.dimColor(foreground, Commons.Color.popups.background)
  property color urgent: Commons.Color.urgent
  property string fontFamily: Style.font.family

  readonly property real gap: mark.visible ? mark.implicitWidth + Style.spacing.sm : 0

  implicitWidth: gap + label.implicitWidth
  implicitHeight: Math.max(mark.visible ? mark.implicitHeight : 0, label.implicitHeight)

  Accessible.role: marked ? Accessible.AlertMessage : Accessible.StaticText
  Accessible.name: text

  Text {
    id: mark
    objectName: "alertGlyph"
    visible: root.marked
    textFormat: Text.PlainText
    text: String.fromCodePoint(root.glyph)
    color: root.urgent
    font.family: root.fontFamily
    font.pixelSize: root.pixelSize
  }

  PlainLabel {
    id: label
    objectName: "alertText"
    x: root.gap
    width: Math.max(0, root.width - root.gap)
    height: root.height
    wrapMode: root.wrapMode
    elide: root.wrapMode === Text.NoWrap ? Text.ElideRight : Text.ElideNone
    color: root.marked ? root.foreground : root.dim
    font.family: root.fontFamily
    font.pixelSize: root.pixelSize
    font.bold: root.bold
    text: root.text
  }
}
