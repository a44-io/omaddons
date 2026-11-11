import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// A change's page (1n2p.30, design doc §2.4), pushed from History by Enter: what the
// change did, when, by whom and why; the plan it was applied from; each client's addons,
// one line each (Model.historyPage); and its undo: all of it, and with several clients
// each client's alone, previewed in the plan sheet first. A change undo never reverts,
// or one already undone, says why instead. The addon lines take the cursor: Enter opens
// that addon's page, and on a restore the backup line shows the backup in Backups.
// Ctrl+Z undoes the change. The page is read from History's data by the change's key, so
// a doorbell's re-read keeps it current. The keys are Model.KEYMAP's page rows.
Item {
  id: root

  property var frame: null
  // The History row this page opened on: its run's key.
  property string changeKey: ""

  readonly property var svc: frame ? frame.svc : null
  readonly property var statusTargets: svc && Array.isArray(svc.targets) ? svc.targets : []
  readonly property double nowMs: frame ? frame.nowMs : Date.now()
  readonly property var page: Model.historyPage(svc ? svc.history : null, changeKey, statusTargets, nowMs)
  // The cursorable lines: each addon's, then a restore's backup.
  readonly property var rows: {
    if (!page) return []
    var out = page.items.slice()
    if (page.backup) out.push({ "kind": "backup", "text": Model.copy("history.page.backup"), "addon": "", "target": page.backup.target })
    return out
  }
  readonly property int rowCount: rows.length
  readonly property bool typing: false
  property var lineItems: ({})

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) {
    if (page && page.backup && i === rows.length - 1) return backupLine
    return lineItems[i] || null
  }

  function handleKey(name, cursor) {
    if (!page) return false
    var row = cursor >= 0 && cursor < rows.length ? rows[cursor] : null
    if (name === "enter") {
      if (!row) return true
      if (row.kind === "backup") frame.showView("backups")
      else if (row.addon) frame.openAddon(row.addon, "")
      return true
    }
    if (name === "undo") {
      undo(page.undos.length ? page.undos[0] : null)
      return true
    }
    return false
  }

  // One of the page's undos (all of it, or one client's), previewed first; none: why.
  function undo(choice) {
    if (!choice) {
      frame.report(page.why || Model.copy("history.why.undone"))
      return
    }
    if (!canChange) return
    frame.report(svc.run(choice.request.template, choice.request.slots, choice.request.label))
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  Column {
    id: column
    width: root.width
    spacing: Style.spacing.md

    // History was read again and no longer lists this change (it fell past the limit).
    PlainLabel {
      objectName: "changeGone"
      visible: !root.page
      width: parent.width
      wrapMode: Text.WordWrap
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      text: Model.copy("history.page.gone")
    }

    PlainLabel {
      objectName: "pageTitle"
      visible: !!root.page
      width: parent.width
      wrapMode: Text.WordWrap
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      font.bold: true
      text: root.page ? root.page.header : ""
    }

    Repeater {
      objectName: "notes"
      model: root.page ? root.page.notes : []

      PlainLabel {
        required property string modelData
        width: column.width
        wrapMode: Text.WordWrap
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: modelData
      }
    }

    // Each client's name (several clients, or one that changed no addon), then its
    // addons, one line each: the addon lines take the cursor.
    Repeater {
      objectName: "lines"
      model: root.page ? root.page.lines : []

      Item {
        id: line
        required property var modelData
        readonly property bool item: modelData.kind === "item"
        width: column.width
        implicitHeight: item ? itemSurface.implicitHeight : clientLabel.implicitHeight

        PlainLabel {
          id: clientLabel
          objectName: "clientLine"
          visible: !line.item
          width: parent.width
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          text: line.modelData.text
        }

        CursorSurface {
          id: itemSurface
          objectName: "itemLine_" + (line.item ? line.modelData.index : -1)
          visible: line.item
          width: parent.width
          implicitHeight: itemText.implicitHeight + Style.spacing.sm * 2
          hasCursor: line.item && !!root.frame && root.frame.cursorActive && root.frame.cursor === line.modelData.index
          foreground: root.foreground
          // What the change did to one addon; Enter opens its page (1n2p.40).
          Accessible.role: Accessible.ListItem
          Accessible.name: String(line.modelData.text || "")

          Component.onCompleted: if (line.item) root.lineItems[line.modelData.index] = itemSurface
          Component.onDestruction: if (line.item && root.lineItems[line.modelData.index] === itemSurface) delete root.lineItems[line.modelData.index]

          PlainLabel {
            id: itemText
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.leftMargin: Style.spacing.lg
            anchors.verticalCenter: parent.verticalCenter
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            text: line.modelData.text
          }

          // Hover moves the cursor (through the frame's PointerMoveGate), one click
          // opens the addon's page, as Enter does (§4, 1n2p.15).
          MouseArea {
            id: itemArea
            anchors.fill: parent
            enabled: line.item
            hoverEnabled: true
            onPositionChanged: function(mouse) {
              if (!root.frame || root.frame.pointerGate.moved(itemArea, mouse)) root.select(line.modelData.index)
            }
            onClicked: {
              root.select(line.modelData.index)
              root.handleKey("enter", line.modelData.index)
            }
          }
        }
      }
    }

    // A restore's backup: Enter (or a click) shows it in Backups.
    CursorSurface {
      id: backupLine
      objectName: "backupLine"
      visible: !!root.page && !!root.page.backup
      width: parent.width
      implicitHeight: backupText.implicitHeight + Style.spacing.sm * 2
      hasCursor: !!root.frame && root.frame.cursorActive && root.frame.cursor === root.rows.length - 1 && visible
      foreground: root.foreground
      Accessible.role: Accessible.Link
      Accessible.name: Model.copy("history.page.backup")

      PlainLabel {
        id: backupText
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.leftMargin: Style.spacing.lg
        anchors.verticalCenter: parent.verticalCenter
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: Model.copy("history.page.backup")
      }

      MouseArea {
        id: backupArea
        anchors.fill: parent
        hoverEnabled: true
        onPositionChanged: function(mouse) {
          if (!root.frame || root.frame.pointerGate.moved(backupArea, mouse)) root.select(root.rows.length - 1)
        }
        onClicked: {
          root.select(root.rows.length - 1)
          root.handleKey("enter", root.rows.length - 1)
        }
      }
    }

    // Why it cannot be undone, when it cannot.
    PlainLabel {
      objectName: "why"
      visible: !!root.page && root.page.undos.length === 0 && root.page.why !== ""
      width: parent.width
      wrapMode: Text.WordWrap
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: root.page ? root.page.why : ""
    }

    Flow {
      objectName: "undos"
      width: parent.width
      spacing: Style.spacing.sm

      Repeater {
        model: root.page ? root.page.undos : []

        Button {
          required property var modelData
          required property int index
          objectName: "pageUndo_" + index
          // The first is all of it, the frame's Ctrl+Z here: its tooltip names the key,
          // never its label (omad-vqkd).
          text: modelData.text
          tooltipText: index === 0 ? Model.withKey(modelData.text, "undo") : ""
          Accessible.role: Accessible.Button
          Accessible.name: text
          enabled: root.canChange
          opacity: enabled ? 1.0 : 0.4
          bordered: true
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          onClicked: root.undo(modelData)
        }
      }
    }
  }
}
