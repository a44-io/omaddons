import QtQuick

// Omarchy's qs.Ui PanelHero: its properties, the icon and the trailing control loaded.
Item {
  id: root

  property Component iconComponent: null
  property string title: ""
  property string meta: ""
  property string detail: ""
  property color foreground: "#cacccc"
  property string fontFamily: "monospace"
  property real iconSize: 24
  property real iconOpacity: 1.0
  property real metaOpacity: 1.0
  property Component trailingControl: null

  width: parent ? parent.width : implicitWidth
  implicitHeight: Math.max(30, trailingLoader.implicitHeight)

  Loader {
    sourceComponent: root.iconComponent
  }

  Loader {
    id: trailingLoader
    objectName: "trailing"
    anchors.right: parent.right
    sourceComponent: root.trailingControl
  }
}
