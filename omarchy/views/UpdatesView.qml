import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// The Updates view (F5): what is behind on each client, from `list -v` (Model.updateRows:
// updates first, then pinned with a newer release, blocked, and failed checks; grouped
// by client in the All filter). Enter and `u` update the row's addon, `p` pins or
// unpins it, `e` shows its newest release notes (a network read, on demand, kept only
// for the row that asked); `U` (frame-wide) updates everything in the filter. Updates
// preview first and apply a routine plan that matches the row (M4-D28); pins run
// directly. The keys are Model.KEYMAP's updates rows.
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var rows: Model.updateRows(svc ? svc.lists : null, filter, svc && Array.isArray(svc.targets) ? svc.targets : [])
  // What the view says around its rows (design doc §7): loading, empty, a check whose
  // sources did not answer, a stale check.
  readonly property var viewState: Model.viewState("updates", { "service": svc, "filter": filter, "rows": rows.length,
    "allRows": filter ? Model.updateRows(svc ? svc.lists : null, "", svc && Array.isArray(svc.targets) ? svc.targets : []).length : rows.length,
    "nowMs": frame ? frame.nowMs : Date.now() })
  readonly property int count: Model.updateCount(rows)
  readonly property int rowCount: rows.length
  readonly property bool typing: false
  // The row whose release notes are open (`e`), by its key.
  property string expanded: ""

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) { return rowRepeater.itemAt(i) }

  function handleKey(name, cursor) {
    var row = cursor >= 0 && cursor < rows.length ? rows[cursor] : null
    if (name === "enter" || name === "update") {
      if (row && row.kind === "update") run(Model.rowAction(row, "update"))
      return true
    }
    if (name === "pin") {
      if (row) run(Model.rowAction(row, "pin"))
      return true
    }
    if (name === "notes") {
      if (row) toggleNotes(row)
      return true
    }
    return false
  }

  function run(action) {
    if (!canChange || !action) return false
    return frame.report(svc.run(action.template, action.slots, action.label, action.shown))
  }

  function toggleNotes(row) {
    if (expanded === row.key) {
      expanded = ""
      return
    }
    expanded = row.key
    svc.readChangelog(row.target, row.name)
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  Column {
    id: column
    width: root.width
    spacing: Style.spacing.sm

    Button {
      objectName: "updateAll"
      visible: root.count > 0
      text: (root.filter ? "Update " + Model.heroTitle(root.svc ? root.svc.targets : [], root.filter) : "Update all") + " (" + root.count + ")"
      tooltipText: Model.keyTip("update_all", "updates")
      enabled: root.canChange
      opacity: enabled ? 1.0 : 0.4
      bordered: true
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      onClicked: root.frame.updateAll()
    }

    ViewState {
      objectName: "viewState"
      width: parent.width
      viewState: root.viewState
      foreground: root.foreground
      fontFamily: root.fontFamily
      onAction: function(name) { if (root.frame) root.frame.stateAction(name) }
    }

    Repeater {
      id: rowRepeater
      model: root.rows

      Column {
        id: entry
        required property var modelData
        required property int index
        objectName: "updateRow_" + index
        width: column.width
        spacing: Style.spacing.sm
        readonly property bool open: root.expanded === modelData.key
        readonly property var notes: open ? Model.notesFor(root.svc ? root.svc.changelog : null, modelData) : null

        // Under All, the client's header above its first row: its player name, whether
        // it runs, its updates (design doc §2.2). The rows under it name no client.
        SectionHeader {
          objectName: "clientHeader"
          visible: entry.modelData.group !== ""
          width: parent.width
          title: entry.modelData.group
          count: entry.modelData.groupCount
          foreground: root.foreground
          dim: root.dim
          fontFamily: root.fontFamily
        }

        CursorSurface {
          width: parent.width
          implicitHeight: line.implicitHeight + Style.spacing.md * 2
          hasCursor: !!root.frame && root.frame.cursorActive && root.frame.cursor === entry.index
          foreground: root.foreground

          Row {
            id: line
            anchors.left: parent.left
            anchors.leftMargin: Style.spacing.lg
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.spacing.lg

            PlainLabel {
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              font.bold: true
              text: entry.modelData.name
            }

            PlainLabel {
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.body
              text: (entry.modelData.version || "?") + (entry.modelData.latest ? " → " + entry.modelData.latest : "")
            }

            PlainLabel {
              visible: entry.modelData.label !== ""
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: entry.modelData.label
            }

            // A running client loads the update after a /reload.
            Text {
              objectName: "reloadChip"
              visible: entry.modelData.reload && entry.modelData.kind === "update"
              textFormat: Text.PlainText
              text: "then /reload"
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
            }
          }

          MouseArea {
            anchors.fill: parent
            onClicked: root.select(entry.index)
            onDoubleClicked: root.handleKey("enter", entry.index)
          }
        }

        // `e`: the newest release notes, plain text, with a link only to a safe host.
        Column {
          objectName: "notes"
          visible: entry.open
          width: parent.width
          leftPadding: Style.spacing.lg
          spacing: Style.spacing.sm

          PlainLabel {
            objectName: "notesText"
            width: parent.width - Style.spacing.lg
            wrapMode: Text.WordWrap
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: entry.notes ? entry.notes.text : "Reading the release notes…"
          }

          Button {
            objectName: "notesLink"
            visible: !!entry.notes && entry.notes.url !== ""
            text: entry.notes ? entry.notes.link : ""
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.caption
            onClicked: if (Model.isSafeUrl(entry.notes.url)) Quickshell.execDetached(["omarchy-launch-browser", entry.notes.url])
          }
        }
      }
    }
  }
}
