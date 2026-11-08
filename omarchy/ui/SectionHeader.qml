import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model

// A section header (design doc §5.2, §5.5), drawn as qs.Ui PanelSectionHeader draws one
// (caption, bold, capitals, the glyph overshoot reserved above) but through PlainLabel:
// its title can be a client's name, CLI data that must stay plain (F7), and the kit's
// header dims with Qt.darker, which darkens on a light theme (UX-44). An optional count
// and an optional trailing button.
Item {
  id: root

  property string title: ""
  property string count: ""
  property string buttonText: ""
  property bool buttonEnabled: true

  property color foreground: Commons.Color.foreground
  property color dim: Model.dimColor(foreground, Commons.Color.popups.background)
  property string fontFamily: Style.font.family

  signal buttonClicked()

  readonly property bool titleTruncated: header.truncated

  implicitWidth: 400
  implicitHeight: Math.max(header.implicitHeight + header.y, button.visible ? button.implicitHeight : 0)

  Accessible.role: Accessible.Heading
  Accessible.name: title + (count !== "" ? ", " + count : "")

  PlainLabel {
    id: header
    objectName: "sectionTitle"
    y: Math.ceil(Style.font.caption * 0.15)
    width: Math.max(0, Math.min(implicitWidth, root.width - (countLabel.visible ? countLabel.width + Style.spacing.md : 0)
      - (button.visible ? button.width + Style.spacing.lg : 0)))
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.bold: true
    font.capitalization: Font.AllUppercase
    text: root.title
  }

  PlainLabel {
    id: countLabel
    objectName: "sectionCount"
    visible: root.count !== ""
    x: header.width + Style.spacing.md
    y: header.y
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    elide: Text.ElideNone
    text: root.count
  }

  Button {
    id: button
    objectName: "sectionButton"
    visible: root.buttonText !== ""
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    text: root.buttonText
    enabled: root.buttonEnabled
    opacity: enabled ? 1.0 : 0.4
    bordered: true
    foreground: root.foreground
    fontFamily: root.fontFamily
    fontSize: Style.font.caption
    Accessible.role: Accessible.Button
    Accessible.name: root.buttonText
    onClicked: root.buttonClicked()
  }
}
