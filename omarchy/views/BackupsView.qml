import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// The Backups view (F9): per client, its automatic backups and space, then each backup
// (Model.backupSections): when, kind, size, stable or best effort, and what it proves
// ("verified" only for a verified one, M3-D47); one in use says so. Enter (or `r`)
// opens the restore chooser for that backup; `x` prunes it, confirmed and bound to its
// preview; `b` (frame-wide) backs up (Model.KEYMAP's backups rows). Every action names
// the row's exact backup id, never "newest" (M3-D9); restores wait for the game to
// close (M3-D29).
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var sections: Model.backupSections(svc ? svc.backups : null, filter, frame ? frame.nowMs : Date.now(),
    svc && Array.isArray(svc.targets) ? svc.targets : [])
  readonly property var rows: {
    var out = []
    for (var i = 0; i < sections.length; i++) out = out.concat(sections[i].rows)
    return out
  }
  readonly property int rowCount: rows.length
  // What the view says around its client cards (design doc §7): reading, no backups at
  // all, details out of date, a read that failed. A client's own card says when it has
  // none, so the cards are what the view counts.
  readonly property var viewState: Model.viewState("backups", { "service": svc, "filter": filter, "rows": sections.length,
    "allRows": filter ? Model.backupSections(svc ? svc.backups : null, "", frame ? frame.nowMs : Date.now(),
      svc && Array.isArray(svc.targets) ? svc.targets : []).length : sections.length, "nowMs": frame ? frame.nowMs : Date.now() })
  readonly property bool typing: false
  property var items: ({})

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) { return items[i] || null }

  function handleKey(name, cursor) {
    var row = cursor >= 0 && cursor < rows.length ? rows[cursor] : null
    if (name === "enter" || name === "restore") {
      if (row) frame.openChooser({ "target": row.target, "backup": row.id })
      return true
    }
    if (name === "remove") {
      if (row) prune(row)
      return true
    }
    return false
  }

  // A backup another process reads is never pruned from here (M3-D48).
  function prune(row) {
    if (!canChange) return
    if (row.busy) {
      frame.report("this backup is in use; try again when it is free")
      return
    }
    frame.report(svc.run("prune_backup", { "target": row.target, "backup": row.id }, "Delete backup " + row.id))
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  Column {
    id: column
    width: root.width
    spacing: Style.spacing.md

    ViewState {
      objectName: "viewState"
      width: parent.width
      viewState: root.viewState
      foreground: root.foreground
      fontFamily: root.fontFamily
      onAction: function(name) { if (root.frame) root.frame.stateAction(name) }
    }

    Button {
      objectName: "backUp"
      visible: !!root.svc && !!root.svc.backups
      text: Model.withKey(root.filter ? "Back up " + root.filter : "Back up all", "backup", "backups")
      tooltipText: Model.keyTip("backup", "backups")
      enabled: root.canChange
      opacity: enabled ? 1.0 : 0.4
      bordered: true
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      onClicked: root.frame.backUp()
    }

    Repeater {
      objectName: "sections"
      model: root.sections

      Column {
        id: section
        required property var modelData
        width: column.width
        spacing: Style.spacing.sm

        // The client's header: its player name, whether it runs (design doc §2.2).
        SectionHeader {
          objectName: "clientHeader"
          width: parent.width
          title: section.modelData.title
          count: section.modelData.count || ""
          foreground: root.foreground
          dim: root.dim
          fontFamily: root.fontFamily
        }

        Repeater {
          objectName: "headerLines"
          model: section.modelData.lines

          PlainLabel {
            required property string modelData
            width: section.width
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: modelData
          }
        }

        Repeater {
          model: section.modelData.rows

          CursorSurface {
            id: entry
            required property var modelData
            objectName: "backupRow_" + modelData.index
            width: section.width
            implicitHeight: line.implicitHeight + Style.spacing.md * 2
            hasCursor: !!root.frame && root.frame.cursorActive && root.frame.cursor === entry.modelData.index
            foreground: root.foreground

            Component.onCompleted: root.items[modelData.index] = entry
            Component.onDestruction: if (root.items[modelData.index] === entry) delete root.items[modelData.index]

            PlainLabel {
              id: line
              objectName: "line"
              anchors.left: parent.left
              anchors.right: parent.right
              anchors.leftMargin: Style.spacing.lg
              anchors.rightMargin: Style.spacing.lg
              anchors.verticalCenter: parent.verticalCenter
              color: entry.modelData.restorable ? root.foreground : root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              text: entry.modelData.line
            }

            MouseArea {
              anchors.fill: parent
              onClicked: root.select(entry.modelData.index)
              onDoubleClicked: root.handleKey("enter", entry.modelData.index)
            }
          }
        }
      }
    }
  }
}
