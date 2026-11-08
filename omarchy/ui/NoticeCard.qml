import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model

// A notice (design doc §5.5): "failure" (urgent tint 0.10, border 0.35, the alert glyph),
// "info" (a muted border, the information glyph) or "success" (an accent tint, the
// check glyph). A title, a body, up to three buttons ({name, text}), and an optional
// dismiss. The kind always has its glyph, so it never says so by colour alone.
BorderSurface {
  id: root

  property string kind: "info"
  property string title: ""
  property string body: ""
  property var buttons: []
  property bool dismissible: false

  property color foreground: Commons.Color.foreground
  property color dim: Model.dimColor(foreground, Commons.Color.popups.background)
  property color urgent: Commons.Color.urgent
  property color accent: Commons.Color.accent
  property color muted: Commons.Color.muted
  property string fontFamily: Style.font.family

  signal clicked(string name)
  signal dismissed()

  function alpha(c, a) { return Qt.rgba(c.r, c.g, c.b, a) }

  readonly property string glyph: kind === "failure" ? String.fromCodePoint(0xF0028)
    : kind === "success" ? String.fromCodePoint(0xF05E0) : String.fromCodePoint(0xF02FD)
  readonly property color glyphColor: kind === "failure" ? urgent : kind === "success" ? accent : dim

  implicitWidth: 400
  implicitHeight: column.implicitHeight + Style.spacing.lg * 2
  radius: Style.cornerRadius
  color: kind === "failure" ? alpha(urgent, 0.10) : kind === "success" ? alpha(accent, 0.08) : "transparent"
  borderSpec: Border.flat(kind === "failure" ? alpha(urgent, 0.35) : kind === "success" ? alpha(accent, 0.35) : muted, 1)

  Accessible.role: Accessible.AlertMessage
  Accessible.name: title + (body !== "" ? ". " + body : "")

  Text {
    id: glyphLabel
    objectName: "noticeGlyph"
    x: Style.spacing.lg
    y: Style.spacing.lg
    textFormat: Text.PlainText
    text: root.glyph
    color: root.glyphColor
    font.family: root.fontFamily
    font.pixelSize: Style.font.icon
  }

  Column {
    id: column
    x: glyphLabel.x + glyphLabel.implicitWidth + Style.spacing.md
    y: Style.spacing.lg
    width: root.width - x - Style.spacing.lg - (dismiss.visible ? dismiss.width + Style.spacing.sm : 0)
    spacing: Style.spacing.sm

    PlainLabel {
      objectName: "noticeTitle"
      width: parent.width
      wrapMode: Text.WordWrap
      elide: Text.ElideNone
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      font.bold: true
      text: root.title
    }

    PlainLabel {
      objectName: "noticeBody"
      visible: root.body !== ""
      width: parent.width
      wrapMode: Text.WordWrap
      elide: Text.ElideNone
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      text: root.body
    }

    Flow {
      visible: root.buttons.length > 0
      width: parent.width
      spacing: Style.spacing.sm

      Repeater {
        model: root.buttons.slice(0, 3)

        Button {
          required property var modelData
          required property int index
          objectName: "noticeButton_" + index
          text: String(modelData.text || "")
          bordered: true
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.caption
          Accessible.role: Accessible.Button
          Accessible.name: String(modelData.text || "")
          onClicked: root.clicked(String(modelData.name || ""))
        }
      }
    }
  }

  PanelActionButton {
    id: dismiss
    objectName: "noticeDismiss"
    visible: root.dismissible
    anchors.right: parent.right
    anchors.top: parent.top
    anchors.margins: Style.spacing.sm
    // U+F0156 nf-md-close.
    iconText: String.fromCodePoint(0xF0156)
    tooltipText: "Dismiss"
    foreground: root.foreground
    fontFamily: root.fontFamily
    Accessible.role: Accessible.Button
    Accessible.name: "Dismiss"
    onClicked: root.dismissed()
  }
}
