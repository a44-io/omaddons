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
    // The first way back from the change before an error (RG-61), the binary's best.
    if (name === "enter") {
      var first = Model.errorPageAction(page)
      if (first && canChange) runWayBack(first)
      return true
    }
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

  // Whether there was a report to copy; the flash follows in the corner toast once
  // wl-copy answers (omad-92d5).
  function copyReport() {
    if (!svc || !row) return false
    var addon = row.addon
    var shown = frame
    return svc.copyCommand(Model.errorReport(row, statusTargets), function(ok) {
      if (shown) shown.flash(Model.copyFlash(ok, Model.copy("errors.copied", { "addon": addon })), ok)
    })
  }

  // A way back the binary gave (RG-61): its argv as the envelope has it, through the
  // CLI-argv path, which classifies it, previews it and confirms it through the plan
  // sheet. The page holds none of the rules that chose it.
  function runWayBack(button) {
    if (!frame || !button || !button.run || !canChange) return false
    return frame.runCli(button.run, button.label)
  }

  function toggleRaw() {
    if (!svc || !row) return
    rawShown = !rawShown
    if (rawShown) svc.readRawErrors(row.target, row.addon)
    else svc.clearRawErrors()
  }

  // Leaving the page drops a raw read still on its way.
  Component.onDestruction: if (rawShown && svc) svc.clearRawErrors()

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
        // A button's label is its words; its tooltip names the key (omad-vqkd).
        text: Model.copy("errors.action.update", { "addon": root.row ? root.row.addon : "" })
        tooltipText: Model.withKey(text, "update", "page")
        Accessible.role: Accessible.Button
        Accessible.name: text
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
        text: Model.copy("errors.action.copy")
        tooltipText: Model.withKey(text, "copy", "page")
        Accessible.role: Accessible.Button
        Accessible.name: text
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.copyReport()
      }

      Button {
        objectName: "pageRestore"
        text: Model.copy("installed.action.restore")
        tooltipText: Model.withKey(text, "restore", "page")
        Accessible.role: Accessible.Button
        Accessible.name: text
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
        // The warning a sighted player reads in its tooltip, said with it.
        Accessible.role: Accessible.Button
        Accessible.name: text
        Accessible.description: tooltipText
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.toggleRaw()
      }
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

        // When it began (RG-61): a new error's basis in words.
        PlainLabel {
          objectName: "errorFirstSeen"
          visible: text !== ""
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: error.modelData.firstSeen
        }

        // What changed before this (RG-61, design 5.1): the change, its grade as a word
        // (never a colour alone), what weakens it, and a button per way back the binary
        // gave; a weak link, or one that no longer stands, is words only.
        Column {
          id: change
          objectName: "change"
          readonly property var section: error.modelData.change
          visible: !!section
          width: parent.width
          spacing: Style.spacing.xs

          PlainLabel {
            objectName: "changeHeading"
            width: parent.width
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.bodySmall
            font.bold: true
            text: change.section ? change.section.heading : ""
          }

          Repeater {
            objectName: "changeLines"
            model: change.section ? change.section.lines : []

            PlainLabel {
              required property string modelData
              objectName: "changeLine"
              width: change.width
              wrapMode: Text.WordWrap
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: modelData
            }
          }

          PlainLabel {
            objectName: "changeWhen"
            width: parent.width
            wrapMode: Text.WordWrap
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: change.section ? change.section.when : ""
          }

          PlainLabel {
            objectName: "changeWord"
            visible: text !== ""
            width: parent.width
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            font.bold: true
            text: change.section ? change.section.word : ""
          }

          Repeater {
            objectName: "changeFacts"
            model: change.section ? change.section.facts : []

            PlainLabel {
              required property string modelData
              objectName: "changeFact"
              width: change.width
              wrapMode: Text.WordWrap
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: modelData
            }
          }

          PlainLabel {
            objectName: "changeState"
            visible: text !== ""
            width: parent.width
            wrapMode: Text.WordWrap
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: change.section ? change.section.state : ""
          }

          Flow {
            width: parent.width
            spacing: Style.spacing.sm

            Repeater {
              objectName: "changeButtons"
              model: change.section ? change.section.buttons : []

              Button {
                required property var modelData
                required property int index
                objectName: "changeButton"
                text: modelData.label
                // The first is the page's main action: its tooltip names Enter.
                tooltipText: index === 0 ? Model.withKey(text, "enter", "page") : text
                Accessible.role: Accessible.Button
                Accessible.name: text
                enabled: root.canChange
                opacity: enabled ? 1.0 : 0.4
                bordered: true
                foreground: root.foreground
                fontFamily: root.fontFamily
                fontSize: Style.font.bodySmall
                onClicked: root.runWayBack(modelData)
              }
            }
          }
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

      // The warning beside the alert glyph, its words in the foreground (1n2p.40).
      AlertLine {
        objectName: "rawLabel"
        width: parent.width
        glyph: 0xF0026
        bold: true
        foreground: root.foreground
        urgent: root.urgent
        fontFamily: root.fontFamily
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
