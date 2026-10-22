import QtQuick

// Omarchy's qs.Ui KeyboardPanel without the layer-shell window: the card as an item, its
// size functions, and the focus it hands to `focusTarget` when it opens.
Item {
  id: root

  property Item anchorItem: null
  property QtObject bar: null
  property var owner: null
  property bool open: false
  property Item focusTarget: null
  property int contentWidth: 280
  property int contentHeight: 200

  default property alias contentItem: holder.children

  function fittedContentWidth(width, cap) {
    var desired = Math.max(1, Number(width) || 1)
    return Math.round(cap !== undefined && Number(cap) > 0 ? Math.min(desired, Number(cap)) : desired)
  }

  function fittedContentHeight(implicitHeight, cap) {
    var desired = Math.max(0, Number(implicitHeight) || 0)
    return Math.round(cap !== undefined && Number(cap) > 0 ? Math.min(desired, Number(cap)) : desired)
  }

  function close() {
    if (owner && "close" in owner) owner.close()
    else root.open = false
  }

  width: contentWidth
  height: contentHeight
  visible: open

  onOpenChanged: if (open && focusTarget) Qt.callLater(function() {
    if (root.open && root.focusTarget) root.focusTarget.forceActiveFocus()
  })

  Item {
    id: holder
    anchors.fill: parent
  }
}
