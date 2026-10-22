import QtQuick

// Omarchy's qs.Ui BorderSurface: a Rectangle with a border spec and padding insets.
Rectangle {
  property var borderSpec: ({})
  property real padding: 0
  property real topPadding: padding
  property real rightPadding: padding
  property real bottomPadding: padding
  property real leftPadding: padding
  readonly property real contentTopInset: topPadding
  readonly property real contentRightInset: rightPadding
  readonly property real contentBottomInset: bottomPadding
  readonly property real contentLeftInset: leftPadding
}
