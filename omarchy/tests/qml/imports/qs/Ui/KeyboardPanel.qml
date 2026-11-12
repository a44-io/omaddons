import QtQuick

// Omarchy's qs.Ui KeyboardPanel without the layer-shell window: the card as an item, its
// size functions, and the focus it hands to `focusTarget` when it opens. Its geometry is
// the kit's (omad-zppa): `contentHeight` is the card's, padding and border included, the
// content gets the card less that inset, and fittedContentHeight adds the inset to the
// height it is given, then rounds. The kit's defaults: popupPadding 14, a 2 px border.
Item {
  id: root

  property Item anchorItem: null
  property QtObject bar: null
  property var owner: null
  property bool open: false
  property Item focusTarget: null
  property int contentWidth: 280
  property int contentHeight: 200
  property int padding: 14
  property int borderWidth: 2
  // The screen's room for the card; 0 is no screen, as the kit reads it.
  property real availableCardHeight: 0
  readonly property real verticalContentInset: padding * 2 + borderWidth * 2

  default property alias contentItem: holder.children

  function fittedContentWidth(width, cap) {
    var desired = Math.max(1, Number(width) || 1)
    return Math.round(cap !== undefined && Number(cap) > 0 ? Math.min(desired, Number(cap)) : desired)
  }

  function fittedContentHeight(implicitHeight, cap) {
    var desired = Math.max(root.verticalContentInset, (Number(implicitHeight) || 0) + root.verticalContentInset)
    var maxHeight = root.availableCardHeight > 0 ? root.availableCardHeight : desired
    if (cap !== undefined && Number(cap) > 0) maxHeight = Math.min(maxHeight, Number(cap))
    return Math.round(Math.min(desired, maxHeight))
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
    x: root.padding + root.borderWidth
    y: root.padding + root.borderWidth
    width: root.contentWidth - 2 * (root.padding + root.borderWidth)
    height: root.contentHeight - root.verticalContentInset
  }
}
