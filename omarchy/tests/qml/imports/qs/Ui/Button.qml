import QtQuick

// Omarchy's qs.Ui Button: its properties, `clicked`, and a plain-text label that, as
// the kit's does, elides when the button is held narrower than its text.
Rectangle {
  id: root

  property string text: ""
  property string iconText: ""
  property string tooltipText: ""
  property bool selected: false
  property bool active: false
  property bool hasCursor: false
  property bool focusable: false
  property bool bordered: false
  property color foreground: "#cacccc"
  property color background: "transparent"
  property color accent: "#7aa2f7"
  property string fontFamily: "monospace"
  property real fontSize: 12
  property real iconSize: 14
  property real horizontalPadding: 10
  property real verticalPadding: 6
  property bool leftAlign: false

  signal clicked()
  signal rightClicked()
  signal hovered(bool isHovered)

  implicitWidth: label.implicitWidth + horizontalPadding * 2
  implicitHeight: label.implicitHeight + verticalPadding * 2
  color: background

  Text {
    id: label
    anchors.centerIn: parent
    width: Math.max(0, Math.min(implicitWidth, root.width - root.horizontalPadding * 2))
    elide: Text.ElideRight
    textFormat: Text.PlainText
    text: root.text
  }

  MouseArea {
    anchors.fill: parent
    onClicked: root.clicked()
  }
}
