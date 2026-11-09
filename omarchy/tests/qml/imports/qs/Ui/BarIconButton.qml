import QtQuick

// Omarchy's qs.Ui BarIconButton: a WidgetButton that draws its glyph itself, optically
// centred in a canvas of `canvasSize` (the kit's Style.bar.iconCanvas, 16) inside a
// fixed slot (Style.bar.iconSlot, 27), turned by `textRotation`.
WidgetButton {
  property real slotSize: 27
  readonly property real canvasSize: 16

  labelVisible: false
  implicitWidth: slotSize
}
