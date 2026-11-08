import QtQuick
import qs.Commons
import qs.Commons as Commons

// The one text component for strings that come from the CLI (addon names, messages,
// commands): always plain text, so a name like "<b>x</b>" or "<img src=…>" shows as
// written. It wraps its Text instead of being one, so no user can set textFormat; F7
// checks that every CLI string goes through it.
Item {
  id: root

  property alias text: label.text
  property alias color: label.color
  property alias font: label.font
  property alias elide: label.elide
  property alias wrapMode: label.wrapMode
  property alias maximumLineCount: label.maximumLineCount
  property alias horizontalAlignment: label.horizontalAlignment
  property alias verticalAlignment: label.verticalAlignment
  readonly property alias truncated: label.truncated

  implicitWidth: label.implicitWidth
  implicitHeight: label.implicitHeight

  Text {
    id: label
    anchors.fill: parent
    textFormat: Text.PlainText
    elide: Text.ElideRight
    wrapMode: Text.NoWrap
    // Qualified: a bare `Color` does not reach the qs.Commons singleton here, so an
    // unset colour drew black.
    color: Commons.Color.foreground
    font.family: Style.font.family
    font.pixelSize: Style.font.body
  }
}
