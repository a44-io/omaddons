import QtQuick

// Omarchy's qs.Ui PanelActionButton: an icon button with a tooltip and `clicked`.
Rectangle {
  id: root

  property string iconText: ""
  property string tooltipText: ""
  property color foreground: "#cacccc"
  property color hoverColor: foreground
  property string fontFamily: "monospace"
  property real fontSize: 14
  property real size: 22
  property bool focusable: false
  property bool hasCursor: false
  property bool bordered: false

  signal clicked()
  signal hovered(bool isHovered)

  implicitWidth: size
  implicitHeight: size
  color: "transparent"

  MouseArea {
    anchors.fill: parent
    onClicked: root.clicked()
  }
}
