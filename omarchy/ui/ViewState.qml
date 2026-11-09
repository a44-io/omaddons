import QtQuick
import qs.Commons
import qs.Commons as Commons
import ".."
import "../Model.js" as Model

// A view's state around its rows (design doc §7, 1n2p.19), as Model.viewState decides it:
// a quiet "Updating…" line while a read runs (the rows beneath it stay), a notice per
// thing that qualifies them (a re-read that failed, a source that did not answer, a read
// cut short, a check gone stale), each with its one action, and, when there are no rows
// to show, the empty state: loading, failed, empty or filtered empty, its sentence and
// its next action. Every view puts one above its rows; `action(name)` goes to the frame,
// which carries it out (PanelView.stateAction). It decides nothing.
Column {
  id: root

  // Model.viewState's answer, or null.
  property var viewState: null

  readonly property bool showsEmpty: !!viewState && viewState.state !== "rows" && String(viewState.sentence || "") !== ""
  readonly property bool refreshing: !!viewState && viewState.refreshing === true
  // Any list the notices come as (a JS array, or a list a model handed over).
  readonly property var notices: viewState && viewState.notices && typeof viewState.notices.length === "number" ? viewState.notices : []

  property color foreground: Commons.Color.foreground
  property color dim: Model.dimColor(foreground, Commons.Color.popups.background)
  property string fontFamily: Style.font.family

  signal action(string name)

  spacing: Style.spacing.sm

  // Refreshing: the rows stay, and this says so quietly.
  Row {
    objectName: "stateRefreshing"
    visible: root.refreshing
    spacing: Style.spacing.sm

    PlainLabel {
      id: spinner
      width: implicitWidth
      elide: Text.ElideNone
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: String.fromCodePoint(0xF04E6)

      RotationAnimation on rotation {
        running: root.refreshing
        loops: Animation.Infinite
        from: 0
        to: 360
        duration: 1200
      }
    }

    PlainLabel {
      width: implicitWidth
      elide: Text.ElideNone
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: Model.copy("state.refreshing")
    }
  }

  Repeater {
    objectName: "stateNotices"
    model: root.notices

    NoticeCard {
      required property var modelData
      objectName: "stateNotice"
      width: root.width
      kind: modelData.kind === "failed" ? "failure" : "info"
      title: String(modelData.sentence || "")
      buttons: modelData.action ? [{ "name": modelData.action.name, "text": modelData.action.text }] : []
      foreground: root.foreground
      fontFamily: root.fontFamily
      onClicked: function(name) { root.action(name) }
    }
  }

  EmptyState {
    objectName: "stateEmpty"
    visible: root.showsEmpty
    width: root.width
    glyph: root.viewState && root.viewState.glyph ? String.fromCodePoint(root.viewState.glyph) : ""
    sentence: root.viewState ? String(root.viewState.sentence || "") : ""
    detail: root.viewState ? String(root.viewState.detail || "") : ""
    actionText: root.viewState && root.viewState.action ? String(root.viewState.action.text || "") : ""
    foreground: root.foreground
    fontFamily: root.fontFamily
    onAction: if (root.viewState && root.viewState.action) root.action(root.viewState.action.name)
  }
}
