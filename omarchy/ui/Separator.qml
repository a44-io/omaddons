import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui

// The line between client sections (design doc §5.3): qs.Ui PanelSeparator.
PanelSeparator {
  foreground: Commons.Color.foreground
  Accessible.role: Accessible.Separator
}
