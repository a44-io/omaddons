import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// An addon's error page (1n2p.27, design doc §2.4), pushed on the navigation stack from
// the Errors view: who most likely threw them, how often and on which client, then
// each distinct error with its message as the widget may show it (an addon's own
// message only as its length, M3-D21), where, how often and when last, and its stack in
// full, wrapped, the addon's own frames in bold. Show raw details… reads them on demand
// under the line that says what they may hold. `u` updates the addon when a release
// waits, `r` restores its settings, `y` copies a report (never raw details). Its rows
// are read again from the service, so a doorbell refresh keeps the page open with what
// is current. The keys are Model.KEYMAP's page rows.
Item {
  id: root

  property var frame: null
  // The Errors view's row this page opened on: its key, and the row as it was then.
  property string rowKey: ""
  property var initial: null

  readonly property var svc: frame ? frame.svc : null
  readonly property var statusTargets: svc && Array.isArray(svc.targets) ? svc.targets : []
  readonly property double nowMs: frame ? frame.nowMs : Date.now()
  // The row now (it is re-read with every `errors` read), else as it opened.
  readonly property var row: {
    var rows = Model.errorRows(Model.errorSections(svc ? svc.errors : null, statusTargets, "", nowMs))
    for (var i = 0; i < rows.length; i++) if (rows[i].key === rowKey) return rows[i]
    return initial
  }
  readonly property var page: Model.errorPage(row, nowMs)
  readonly property int rowCount: 0
  readonly property bool typing: false
  property bool rawShown: false
  readonly property var raw: rawShown && row ? Model.rawLines(svc ? svc.rawErrors : null, row) : null
  property string copied: ""

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property color accent: frame ? frame.accent : Commons.Color.accent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) { return null }

  function handleKey(name, cursor) {
    if (!row) return false
    if (name === "update") {
      if (row.updateAvailable && canChange) frame.report(svc.run("update_addon", { "target": row.target, "addon": row.addon },
        "Update " + row.addon, Model.shownForAddon(svc.lists, row.target, row.addon)))
      return true
    }
    if (name === "restore") {
      frame.openChooser({ "target": row.target, "addon": row.addon })
      return true
    }
    if (name === "copy") {
      copyReport()
      return true
    }
    return false
  }

  function copyReport() {
    if (!svc || !row || !svc.copyCommand(Model.errorReport(row, statusTargets))) return false
    copied = Model.copy("errors.copied", { "addon": row.addon })
    copiedTimer.restart()
    return true
  }

  function toggleRaw() {
    if (!svc || !row) return
    rawShown = !rawShown
    if (rawShown) svc.readRawErrors(row.target, row.addon)
    else svc.clearRawErrors()
  }

  // Leaving the page drops a raw read still on its way.
  Component.onDestruction: if (rawShown && svc) svc.clearRawErrors()

  Timer {
    id: copiedTimer
    interval: 1500
    onTriggered: root.copied = ""
  }

  Column {
    id: column
    width: root.width
    spacing: Style.spacing.md

    PlainLabel {
      objectName: "pageTitle"
      width: parent.width
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      font.bold: true
      text: root.page.title
    }

    PlainLabel {
      objectName: "pageSubtitle"
      width: parent.width
      wrapMode: Text.WordWrap
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: root.page.subtitle
    }

    Flow {
      width: parent.width
      spacing: Style.spacing.sm

      Button {
        objectName: "pageUpdate"
        visible: !!root.row && root.row.updateAvailable === true
        text: Model.withKey(Model.copy("errors.action.update", { "addon": root.row ? root.row.addon : "" }), "update", "page")
        enabled: root.canChange
        opacity: enabled ? 1.0 : 0.4
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.handleKey("update", 0)
      }

      Button {
        objectName: "pageCopy"
        text: Model.withKey(Model.copy("errors.action.copy"), "copy", "page")
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.copyReport()
      }

      Button {
        objectName: "pageRestore"
        text: Model.withKey(Model.copy("installed.action.restore"), "restore", "page")
        enabled: root.canChange
        opacity: enabled ? 1.0 : 0.4
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.handleKey("restore", 0)
      }

      Button {
        objectName: "rawButton"
        text: Model.copy(root.rawShown ? "errors.raw.hide" : "errors.raw.show")
        tooltipText: Model.copy("errors.raw.warning")
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.toggleRaw()
      }
    }

    PlainLabel {
      objectName: "copyFlash"
      visible: root.copied !== ""
      width: parent.width
      color: root.accent
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: root.copied
    }

    // Each distinct error, its stack in full.
    Repeater {
      objectName: "errors"
      model: root.page.errors

      Column {
        id: error
        required property var modelData
        width: column.width
        spacing: Style.spacing.xs

        PlainLabel {
          objectName: "errorMessage"
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          text: error.modelData.message
        }

        PlainLabel {
          objectName: "errorMeta"
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: [error.modelData.location, error.modelData.meta].filter(function(p) { return p !== "" }).join(" · ")
        }

        Repeater {
          objectName: "stack"
          model: error.modelData.stack

          PlainLabel {
            required property var modelData
            objectName: "frame"
            x: Style.spacing.lg
            width: error.width - Style.spacing.lg
            wrapMode: Text.WrapAnywhere
            color: modelData.own ? root.foreground : root.dim
            font.family: "monospace"
            font.pixelSize: Style.font.caption
            font.bold: modelData.own
            text: modelData.text
          }
        }
      }
    }

    // Raw details, under the line that says what they may hold.
    Column {
      objectName: "raw"
      visible: root.rawShown
      width: parent.width
      spacing: Style.spacing.sm

      PlainLabel {
        objectName: "rawLabel"
        width: parent.width
        wrapMode: Text.WordWrap
        color: root.urgent
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        font.bold: true
        text: Model.copy("errors.raw.warning")
      }

      Repeater {
        objectName: "rawLines"
        model: root.raw || []

        PlainLabel {
          required property string modelData
          width: column.width
          wrapMode: Text.WrapAnywhere
          color: root.foreground
          font.family: "monospace"
          font.pixelSize: Style.font.caption
          text: modelData
        }
      }

      PlainLabel {
        visible: root.rawShown && root.raw === null
        width: parent.width
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: Model.copy("errors.raw.reading")
      }
    }
  }
}
