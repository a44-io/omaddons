import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// A backup's page (1n2p.29, design doc §2.4), pushed from Backups by Enter: its client,
// when and its kind; when exactly, its size and what it proves; its reason; its markers;
// and the addons whose settings it holds, each with the version that wrote them
// (Model.backupPage, from the `backup list -v` read the page asks for its client). Each
// addon takes the cursor: Enter restores that addon's settings from this backup, so one
// addon's settings come back in three steps (Enter on the backup, Enter on the addon, y
// in the plan sheet). `r` opens the restore chooser on the backup (one addon's settings
// or all of them), `x` deletes it, previewed (never one in use). Restores wait for the
// game to close (M3-D29). The page is read from Backups' data by the client and id, so a
// doorbell's re-read keeps it current. The keys are Model.KEYMAP's page rows.
Item {
  id: root

  property var frame: null
  // The Backups row this page opened on.
  property string target: ""
  property string backupId: ""

  readonly property var svc: frame ? frame.svc : null
  readonly property var statusTargets: svc && Array.isArray(svc.targets) ? svc.targets : []
  readonly property double nowMs: frame ? frame.nowMs : Date.now()
  readonly property var page: Model.backupPage(svc ? svc.backups : null, svc ? svc.backupsDetail : null, target, backupId,
    statusTargets, nowMs)
  readonly property var rows: page ? page.addons : []
  readonly property int rowCount: rows.length
  readonly property bool typing: false
  // The client whose `-v` read this page asked for: dropped when the page closes.
  property string askedFor: ""

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) { return addonRepeater.itemAt(i) }

  // The read, once the frame has handed the page its backup (its props come after the
  // page is made, one by one: callLater asks once for the last of them).
  onTargetChanged: Qt.callLater(root.ask)
  onSvcChanged: Qt.callLater(root.ask)
  Component.onCompleted: Qt.callLater(root.ask)
  Component.onDestruction: if (svc && askedFor !== "") svc.clearBackupsDetail()
  function ask() {
    if (!svc || target === "" || askedFor === target) return
    askedFor = target
    svc.readBackupsDetail(target)
  }

  function handleKey(name, cursor) {
    if (!page) return false
    if (name === "enter") {
      var addon = frame.cursorActive && cursor >= 0 && cursor < rows.length ? rows[cursor] : null
      if (addon) restore(addon.name)
      return true
    }
    if (name === "restore") {
      // The page holds its client's -v read: the chooser over it reads and drops none.
      if (!page.restorable) frame.report(Model.copy("restore.unrestorable"))
      else frame.openChooser({ "target": target, "backup": backupId, "keepDetail": true })
      return true
    }
    if (name === "remove") {
      prune()
      return true
    }
    return false
  }

  // One addon's settings back from this backup, previewed first.
  function restore(addon) {
    if (!page.restorable) {
      frame.report(Model.copy("restore.unrestorable"))
      return
    }
    if (page.blocked) {
      frame.report(page.blocked)
      return
    }
    if (!canChange) return
    var request = Model.restoreRequest(target, backupId, addon, statusTargets)
    frame.report(svc.run(request.template, request.slots, request.label))
  }

  // A backup another process reads is never deleted from here (M3-D48).
  function prune() {
    if (page.busy) {
      frame.report(Model.copy("backups.busy"))
      return
    }
    if (!canChange) return
    var request = Model.pruneRequest({ "target": target, "id": backupId, "at": page.at }, statusTargets)
    frame.report(svc.run(request.template, request.slots, request.label))
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  Column {
    id: column
    width: root.width
    spacing: Style.spacing.md

    // Backups was read again and no longer lists it (deleted, or past the limit).
    PlainLabel {
      objectName: "backupGone"
      visible: !root.page
      width: parent.width
      wrapMode: Text.WordWrap
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      text: Model.copy("backups.page.gone")
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

    // When exactly, its size and what it proves; its reason.
    Repeater {
      model: root.page ? [root.page.facts, root.page.reason].filter(function(l) { return l !== "" }) : []

      PlainLabel {
        required property string modelData
        objectName: "fact"
        width: column.width
        wrapMode: Text.WordWrap
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: modelData
      }
    }

    Flow {
      objectName: "chips"
      visible: !!root.page && root.page.chips.length > 0
      width: parent.width
      spacing: Style.spacing.sm

      Repeater {
        model: root.page ? root.page.chips : []

        Chip {
          required property var modelData
          text: modelData.text
          urgent: modelData.urgent
          foreground: root.foreground
          dim: root.dim
          urgentColor: root.urgent
          fontFamily: root.fontFamily
        }
      }
    }

    // Restores are off while the client runs (M3-D29).
    PlainLabel {
      objectName: "blocked"
      visible: !!root.page && root.page.blocked !== ""
      width: parent.width
      wrapMode: Text.WordWrap
      color: root.urgent
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: root.page ? root.page.blocked : ""
    }

    SectionHeader {
      objectName: "heldHeader"
      visible: !!root.page
      width: parent.width
      title: Model.copy("backups.page.held")
      count: ""
      foreground: root.foreground
      dim: root.dim
      fontFamily: root.fontFamily
    }

    // Reading, a read that failed, or none by name.
    PlainLabel {
      objectName: "heldState"
      visible: !!root.page && root.page.addonsText !== ""
      width: parent.width
      wrapMode: Text.WordWrap
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: root.page ? root.page.addonsText : ""
    }

    // Each addon whose settings it holds: Enter restores them.
    Repeater {
      id: addonRepeater
      objectName: "held"
      model: root.rows

      CursorSurface {
        id: line
        required property var modelData
        required property int index
        readonly property bool cursored: !!root.frame && root.frame.cursorActive && root.frame.cursor === index
        objectName: "heldAddon_" + index
        width: column.width
        implicitHeight: Math.max(addonName.implicitHeight, hint.implicitHeight) + Style.spacing.sm * 2
        hasCursor: cursored
        foreground: root.foreground

        PlainLabel {
          id: addonName
          objectName: "heldName"
          anchors.left: parent.left
          anchors.right: hint.visible ? hint.left : parent.right
          anchors.leftMargin: Style.spacing.lg
          anchors.rightMargin: Style.spacing.md
          anchors.verticalCenter: parent.verticalCenter
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          text: line.modelData.text
        }

        PlainLabel {
          id: hint
          objectName: "restoreHint"
          visible: line.cursored && !!root.page && root.page.restorable && root.page.blocked === ""
          anchors.right: parent.right
          anchors.rightMargin: Style.spacing.lg
          anchors.verticalCenter: parent.verticalCenter
          width: visible ? implicitWidth : 0
          elide: Text.ElideNone
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: Model.withKey(Model.copy("backups.page.restore_one"), "enter", "page")
        }

        MouseArea {
          anchors.fill: parent
          onClicked: root.select(line.index)
          onDoubleClicked: root.handleKey("enter", line.index)
        }
      }
    }

    Flow {
      objectName: "actions"
      visible: !!root.page
      width: parent.width
      spacing: Style.spacing.sm

      Button {
        objectName: "restore"
        text: Model.withKey(Model.copy("backups.action.restore"), "restore", "page")
        enabled: root.canChange && !!root.page && root.page.restorable && root.page.blocked === ""
        opacity: enabled ? 1.0 : 0.4
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.handleKey("restore", -1)
      }

      Button {
        objectName: "delete"
        text: Model.withKey(Model.copy("backups.action.delete"), "remove", "page")
        enabled: root.canChange && !!root.page && !root.page.busy
        opacity: enabled ? 1.0 : 0.4
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.prune()
      }
    }
  }
}
