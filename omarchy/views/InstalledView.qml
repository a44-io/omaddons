import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model

// The Installed view (F5): every managed addon per client, from `list -v`
// (Model.installedRows): version, source, and the markers dep, orphan, pinned, and
// modified or missing (its files checked on disk). Enter reinstalls drifted files, or
// updates when a release is waiting; `p` pins or unpins; `x` removes, through the
// confirm dialog: the CLI's dry run first, then the run bound to it (M3-D9, M3-D46);
// `r` restores its settings from a backup (the restore chooser, F9).
// Unmanaged folders are a line of text: adoption is the CLI's in M3.
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var rows: Model.installedRows(svc ? svc.lists : null, filter)
  readonly property var unmanaged: Model.unmanagedLines(svc ? svc.lists : null, filter)
  readonly property int rowCount: rows.length
  readonly property bool typing: false

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) { return rowRepeater.itemAt(i) }

  function handleKey(name, cursor) {
    var row = cursor >= 0 && cursor < rows.length ? rows[cursor] : null
    if (name === "enter") {
      if (row && row.primary !== "") run(Model.rowAction(row, row.primary))
      return true
    }
    if (name === "x") {
      if (row) run(Model.rowAction(row, "remove"))
      return true
    }
    if (name === "pin") {
      if (row) run(Model.rowAction(row, "pin"))
      return true
    }
    // `r`: this addon's settings from a backup (the restore chooser).
    if (name === "restore") {
      if (row) frame.openChooser({ "target": row.target, "addon": row.name })
      return true
    }
    return false
  }

  function run(action) {
    if (!canChange || !action) return false
    return frame.report(svc.run(action.template, action.slots, action.label, action.shown))
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  Column {
    id: column
    width: root.width
    spacing: Style.spacing.sm

    Text {
      objectName: "reading"
      visible: !!root.svc && !root.svc.lists
      width: parent.width
      textFormat: Text.PlainText
      text: "Reading the clients…"
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }

    Text {
      objectName: "installedEmpty"
      visible: !!root.svc && !!root.svc.lists && root.rows.length === 0
      width: parent.width
      textFormat: Text.PlainText
      text: "No managed addons here yet: omaddons install <ref> adds one"
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
    }

    Repeater {
      id: rowRepeater
      model: root.rows

      Column {
        id: entry
        required property var modelData
        required property int index
        objectName: "installedRow_" + index
        width: column.width
        spacing: Style.spacing.sm

        PlainLabel {
          visible: entry.modelData.group !== ""
          width: parent.width
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          text: entry.modelData.group
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
              text: entry.modelData.version || "?"
            }

            PlainLabel {
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: entry.modelData.source
            }

            // Modified or missing files are the ones Enter repairs.
            PlainLabel {
              objectName: "markers"
              visible: entry.modelData.markers.length > 0
              color: entry.modelData.primary === "reinstall" ? root.urgent : root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: entry.modelData.markers.join(" · ")
            }

            PlainLabel {
              visible: root.filter === ""
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: entry.modelData.target
            }
          }

          MouseArea {
            anchors.fill: parent
            onClicked: root.select(entry.index)
            onDoubleClicked: root.handleKey("enter", entry.index)
          }
        }
      }
    }

    // Adoption is the CLI's in M3: a hint, not a button.
    Repeater {
      objectName: "unmanaged"
      model: root.unmanaged

      PlainLabel {
        required property string modelData
        width: column.width
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: modelData
      }
    }
  }
}
