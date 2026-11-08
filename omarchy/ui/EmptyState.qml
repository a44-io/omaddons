import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model

// A view with nothing to show (design doc §6.4, §5.5): a 24 px glyph in the muted
// colour, the sentence, an optional detail, and at most one action with its key.
Item {
  id: root

  property string glyph: ""
  property string sentence: ""
  property string detail: ""
  property string actionText: ""
  property string actionKey: ""

  property color foreground: Commons.Color.foreground
  property color dim: Model.dimColor(foreground, Commons.Color.popups.background)
  property color muted: Commons.Color.muted
  property string fontFamily: Style.font.family

  signal action()

  implicitWidth: 400
  implicitHeight: column.implicitHeight + Style.spacing.xl * 2

  Accessible.role: Accessible.StaticText
  Accessible.name: sentence + (detail !== "" ? ". " + detail : "")

  Column {
    id: column
    x: Style.spacing.lg
    y: Style.spacing.xl
    width: root.width - Style.spacing.lg * 2
    spacing: Style.spacing.md

    Text {
      objectName: "emptyGlyph"
      visible: root.glyph !== ""
      textFormat: Text.PlainText
      text: root.glyph
      color: root.muted
      font.family: root.fontFamily
      font.pixelSize: Style.font.display
    }

    PlainLabel {
      objectName: "emptySentence"
      width: parent.width
      wrapMode: Text.WordWrap
      elide: Text.ElideNone
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      text: root.sentence
    }

    PlainLabel {
      objectName: "emptyDetail"
      visible: root.detail !== ""
      width: parent.width
      wrapMode: Text.WordWrap
      elide: Text.ElideNone
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: root.detail
    }

    Row {
      visible: root.actionText !== ""
      spacing: Style.spacing.md

      Button {
        objectName: "emptyAction"
        text: root.actionText
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        Accessible.role: Accessible.Button
        Accessible.name: root.actionText
        onClicked: root.action()
      }

      KeyHint {
        visible: root.actionKey !== ""
        anchors.verticalCenter: parent.verticalCenter
        key: root.actionKey
        foreground: root.foreground
        dim: root.dim
        muted: root.muted
        fontFamily: root.fontFamily
      }
    }
  }
}
