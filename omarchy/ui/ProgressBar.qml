import QtQuick
import qs.Commons
import qs.Commons as Commons
import "../Model.js" as Model

// Progress (design doc §5.5): a 4 px track (the foreground at 0.15) and an accent fill.
// `fraction` below 0 is indeterminate: a fixed 15 % fill, no sliding animation (§5.7);
// the activity bar's spinner and verb say it is running.
Rectangle {
  id: root

  property real fraction: -1

  property color foreground: Commons.Color.foreground
  property color accent: Commons.Color.accent

  readonly property bool indeterminate: !(fraction >= 0)
  readonly property real shown: indeterminate ? 0.15 : Math.max(0, Math.min(1, fraction))

  implicitWidth: 400
  implicitHeight: Style.space(4)
  radius: height / 2
  color: Qt.rgba(foreground.r, foreground.g, foreground.b, 0.15)

  Accessible.role: Accessible.ProgressBar
  Accessible.name: indeterminate ? Model.copy("progress.working") : Model.copy("progress.percent", { "n": Math.round(shown * 100) })

  Rectangle {
    objectName: "progressFill"
    width: parent.width * root.shown
    height: parent.height
    radius: parent.radius
    color: root.accent
  }
}
