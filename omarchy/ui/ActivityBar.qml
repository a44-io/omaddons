import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model

// The activity bar (design doc §2.1, §6.4; 1n2p.18), in the pinned area: what runs that
// the player waits on (a change, else a check or a read), in words, with the spinner,
// a determinate progress bar when the CLI reports counts (else the fixed 15 % fill),
// how long it has run after a few seconds, a line when it has gone quiet, Cancel for a
// read (never for a change), and what waits behind it. It draws Model.activityView's
// `view` and decides nothing.
Column {
  id: root

  // Model.activityView's `{kind, text, fraction, elapsed, stalled, cancellable}`, or null.
  property var view: null
  // "Next: Update Retail", or "".
  property string queued: ""
  // Why the change buttons are off while a change runs.
  property string busyNote: Model.copy("activity.busy")

  property color foreground: Commons.Color.foreground
  property color dim: Model.dimColor(foreground, Commons.Color.popups.background)
  property color accent: Commons.Color.accent
  property string fontFamily: Style.font.family

  signal cancelRequested()

  readonly property bool running: !!view
  readonly property bool change: running && view.kind === "change"

  spacing: Style.spacing.xs
  visible: running || queued !== ""

  Accessible.role: Accessible.StatusBar
  Accessible.name: running ? String(view.text) : queued

  // The verb line: the spinner, what runs, how long, and Cancel for a read.
  Item {
    objectName: "activityLine"
    visible: root.running
    width: parent.width
    height: Math.max(lineLabel.implicitHeight, cancelButton.visible ? cancelButton.implicitHeight : 0)

    Text {
      id: spinner
      objectName: "activitySpinner"
      anchors.verticalCenter: parent.verticalCenter
      textFormat: Text.PlainText
      // U+F04E6 nf-md-sync: one turn per 1.2 s, static under reduced motion (§5.7).
      text: String.fromCodePoint(0xF04E6)
      color: root.accent
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption

      RotationAnimation on rotation {
        running: root.running && Style.duration(1200) > 0
        from: 0
        to: 360
        duration: 1200
        loops: Animation.Infinite
        onRunningChanged: if (!running) spinner.rotation = 0
      }
    }

    PlainLabel {
      id: lineLabel
      objectName: "progressLine"
      anchors.verticalCenter: parent.verticalCenter
      x: spinner.implicitWidth + Style.spacing.sm
      width: Math.max(0, parent.width - x - (elapsedLabel.visible ? elapsedLabel.implicitWidth + Style.spacing.sm : 0)
        - (cancelButton.visible ? cancelButton.width + Style.spacing.sm : 0))
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: root.running ? String(root.view.text || "") : ""
    }

    PlainLabel {
      id: elapsedLabel
      objectName: "activityElapsed"
      visible: root.running && String(root.view.elapsed || "") !== ""
      anchors.verticalCenter: parent.verticalCenter
      x: lineLabel.x + lineLabel.width + Style.spacing.sm
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      elide: Text.ElideNone
      text: root.running ? String(root.view.elapsed || "") : ""
    }

    Button {
      id: cancelButton
      objectName: "activityCancel"
      visible: root.running && root.view.cancellable === true
      // A whole-pixel width: right-anchored at a fractional x, its right border rounds
      // past the content's edge and is clipped (omad-kldz).
      width: Math.ceil(implicitWidth)
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      text: Model.copy("activity.cancel")
      bordered: true
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.caption
      Accessible.role: Accessible.Button
      Accessible.name: text
      onClicked: root.cancelRequested()
    }
  }

  ProgressBar {
    objectName: "progressTrack"
    visible: root.running
    width: parent.width
    fraction: root.running && typeof root.view.fraction === "number" ? root.view.fraction : -1
    foreground: root.foreground
    accent: root.accent
  }

  // Gone quiet: "Still working (no news for 30 s)".
  PlainLabel {
    objectName: "activityStalled"
    visible: text !== ""
    width: parent.width
    wrapMode: Text.WordWrap
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    text: root.running ? String(root.view.stalled || "") : ""
  }

  PlainLabel {
    objectName: "busyNote"
    visible: root.change
    width: parent.width
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    text: root.busyNote
  }

  PlainLabel {
    objectName: "queued"
    visible: text !== ""
    width: parent.width
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    text: root.queued
  }
}
