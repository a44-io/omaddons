import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model

// The corner toast (omad-92d5, amending design doc §2.1 and §5.5): what the last action
// said, right-aligned in the bottom-right corner of the panel, on the freshness footer's
// row. It draws Model.toastView's `view` (the last result, a refusal, a key's note or a
// copy's flash) and, beside a result, its Undo; it decides nothing, and whoever holds
// the message times it. It fades in and out in 120 ms (§5.7, none under reduced
// motion), keeping its words while it fades, and says when the pointer is over it so
// that hold can wait. A sentence too long for the row wraps upward.
Item {
  id: root

  // Model.toastView's `{kind, text, marked}`, or null.
  property var view: null
  // A result's Undo: shown, and whether it can act now.
  property bool undoShown: false
  property bool undoEnabled: true
  // The room it may take: the row's width.
  property real maxWidth: 0

  property color foreground: Commons.Color.foreground
  property color urgent: Commons.Color.urgent
  property string fontFamily: Style.font.family

  signal undoRequested()

  readonly property bool showing: view !== null
  readonly property bool hovered: hover.hovered
  // The row's height with a toast or without: the Undo button's, so the content above
  // never moves when a toast comes or goes.
  readonly property real slotHeight: Math.max(line.implicitHeight, undoButton.implicitHeight)

  // What it shows, kept while it fades out.
  property var shown: null
  onViewChanged: if (view) shown = view

  readonly property real undoRoom: undoButton.visible ? undoButton.width + Style.spacing.md : 0

  // A whole-pixel width: right-anchored at a fractional x, the Undo's right border would
  // round past the content's edge and be clipped (omad-kldz).
  width: Math.ceil(Math.min(maxWidth, line.implicitWidth + undoRoom))
  height: Math.max(line.implicitHeight, undoButton.visible ? undoButton.implicitHeight : 0)
  opacity: showing ? 1 : 0
  // Shown at once, gone once faded out.
  visible: showing || opacity > 0

  Behavior on opacity {
    NumberAnimation { duration: Style.duration(120) }
  }

  Accessible.role: Accessible.StatusBar
  Accessible.name: shown ? String(shown.text) : ""

  HoverHandler {
    id: hover
  }

  // A failure, a refusal or a copy that did not happen carries the alert glyph; the
  // words stay in the foreground either way (1n2p.40).
  AlertLine {
    id: line
    objectName: "toastLine"
    anchors.verticalCenter: parent.verticalCenter
    width: Math.max(0, root.width - root.undoRoom)
    marked: !!root.shown && root.shown.marked === true
    foreground: root.foreground
    dim: root.foreground
    urgent: root.urgent
    fontFamily: root.fontFamily
    text: root.shown ? String(root.shown.text) : ""
  }

  Button {
    id: undoButton
    objectName: "toastUndo"
    visible: root.undoShown && !!root.shown && root.shown.kind === "result"
    enabled: root.undoEnabled
    opacity: enabled ? 1.0 : 0.4
    width: Math.ceil(implicitWidth)
    anchors.right: parent.right
    anchors.verticalCenter: parent.verticalCenter
    text: Model.copy("activity.undo")
    Accessible.role: Accessible.Button
    Accessible.name: text
    bordered: true
    foreground: root.foreground
    fontFamily: root.fontFamily
    fontSize: Style.font.caption
    onClicked: root.undoRequested()
  }
}
