import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// A problem's page (1n2p.28, design doc §2.4), pushed on the navigation stack from the
// Issues view by Explain, `e`, or Enter on a problem with nothing to run: its severity,
// the problem in a sentence and its client, then what the doctor says of it (`doctor
// explain`, a local read made when the page opens and dropped when it closes: what is
// wrong, why it matters, what the repair does, the evidence), then its fix. The fix and
// Copy command are the row's own, run by the Issues view (`owner`), so a repair still
// previews and runs bound to that preview, and a command the panel never runs stays
// text. The row is read again from `status` with every refresh; once `status` no longer
// reports it, the page says so. The keys are Model.KEYMAP's page rows: Enter runs the
// fix, `y` copies the command.
Item {
  id: root

  property var frame: null
  // The Issues row this page opened on: its key, the row as it was then, and the view
  // that runs its buttons.
  property string rowKey: ""
  property var initial: null
  property var owner: null

  readonly property var svc: frame ? frame.svc : null
  // The row now, from the latest `status`; null once it is no longer reported.
  readonly property var current: {
    if (!svc) return null
    var rows = Model.issueRows(Model.issueSections(Array.isArray(svc.targets) ? svc.targets : [],
      Array.isArray(svc.statusWarnings) ? svc.statusWarnings : [], "", Array.isArray(svc.machineIssues) ? svc.machineIssues : [],
      Array.isArray(svc.commands) ? svc.commands : []))
    for (var i = 0; i < rows.length; i++) if (rows[i].key === rowKey) return rows[i]
    return null
  }
  readonly property bool gone: current === null && !!svc && !!svc.summary
  readonly property var row: current || initial
  readonly property var explained: row ? Model.explainFor(svc ? svc.explanation : null, row) : null
  readonly property string explainFailed: !!svc && !!svc.explanation && !!svc.explanation.selection && !!row
    && svc.explanation.selection.key === row.key && typeof svc.explanation.failed === "string" ? svc.explanation.failed : ""
  // The page's buttons: the row's, but Explain (this is it).
  readonly property var buttons: row ? row.buttons.filter(function(b) { return b.name !== "explain" }) : []
  readonly property int rowCount: 0
  readonly property bool typing: false

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property color accent: frame ? frame.accent : Commons.Color.accent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) { return null }

  function handleKey(name, cursor) {
    if (!row || !owner || gone) return false
    if (name === "enter") {
      if (row.primary === "fix") owner.press(row, "fix")
      return true
    }
    if (name === "copy") {
      if (row.text !== "") owner.press(row, "copy")
      return true
    }
    return false
  }

  // An explanation is a read: it is asked for once the page has its row (the frame's
  // loader hands it the frame and the row after the page is made), and dropped with it.
  property bool asked: false
  readonly property bool ready: !!svc && !!row && !!row.explain
  onReadyChanged: askExplanation()
  function askExplanation() {
    if (asked || !ready) return
    asked = true
    svc.readExplanation(row.key, row.explain.argv)
  }
  Component.onCompleted: askExplanation()
  Component.onDestruction: if (svc) svc.clearExplanation()

  Column {
    id: column
    width: root.width
    spacing: Style.spacing.md

    // "Needs attention", the problem, and its client. An urgent severity is its row's
    // glyph in the urgent colour beside the word in the foreground (1n2p.40).
    AlertLine {
      objectName: "pageSeverity"
      visible: !!root.row && root.row.severityWord !== ""
      width: parent.width
      marked: !!root.row && root.row.urgent === true
      glyph: root.row && root.row.glyph ? root.row.glyph : 0xF0026
      bold: true
      foreground: root.foreground
      dim: root.dim
      urgent: root.urgent
      fontFamily: root.fontFamily
      text: root.row ? root.row.severityWord : ""
    }

    PlainLabel {
      objectName: "pageTitle"
      width: parent.width
      wrapMode: Text.WordWrap
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
      font.bold: true
      text: root.row ? root.row.title : ""
    }

    PlainLabel {
      objectName: "pageClient"
      visible: !!root.row && root.row.client !== ""
      width: parent.width
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: root.row ? root.row.client : ""
    }

    // Fixed meanwhile (a repair, a change elsewhere): said, and nothing more to run. In
    // the foreground: accent is never text's colour (§5.1, 1n2p.40).
    PlainLabel {
      objectName: "pageGone"
      visible: root.gone
      width: parent.width
      wrapMode: Text.WordWrap
      color: root.foreground
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: Model.copy("issues.page.gone")
    }

    // Why a command is no button, the command, and the row's fix and Copy command.
    AlertLine {
      objectName: "pageNote"
      visible: !root.gone && !!root.row && root.row.note !== ""
      width: parent.width
      glyph: 0xF0026
      foreground: root.foreground
      urgent: root.urgent
      fontFamily: root.fontFamily
      text: root.row ? root.row.note : ""
    }

    PlainLabel {
      objectName: "pageCommand"
      visible: !root.gone && !!root.row && root.row.text !== ""
      width: parent.width
      wrapMode: Text.WrapAnywhere
      color: root.dim
      font.family: "monospace"
      font.pixelSize: Style.font.caption
      text: root.row ? root.row.text : ""
    }

    Flow {
      objectName: "pageButtons"
      visible: !root.gone && root.buttons.length > 0
      width: parent.width
      spacing: Style.spacing.sm

      Repeater {
        model: root.buttons

        Button {
          required property var modelData
          objectName: "pageButton_" + modelData.name
          text: modelData.name === "copy" && !!root.owner && !!root.row && root.owner.copied === root.row.key
            ? Model.copy("issues.copied") : Model.withKey(modelData.text, modelData.name === "fix" ? "enter" : "copy", "page")
          tooltipText: modelData.tooltip
          Accessible.role: Accessible.Button
          Accessible.name: text
          // Never wider than the page: a long fix shrinks, then elides (1n2p.40).
          width: Math.min(implicitWidth, column.width)
          enabled: modelData.name !== "fix" || root.canChange
          opacity: enabled ? 1.0 : 0.4
          bordered: true
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          onClicked: if (root.owner) root.owner.press(root.row, modelData.name)
        }
      }
    }

    // What the doctor says: its lines as plain text, or that it is still reading, or why
    // it could not be read.
    Column {
      objectName: "explanation"
      width: parent.width
      spacing: Style.spacing.xs

      Repeater {
        objectName: "explanationLines"
        model: root.explained || []

        PlainLabel {
          required property string modelData
          width: column.width
          wrapMode: Text.WordWrap
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: modelData
        }
      }

      PlainLabel {
        objectName: "explanationReading"
        visible: !!root.row && !!root.row.explain && root.explained === null && root.explainFailed === ""
        width: column.width
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: Model.copy("issues.page.reading")
      }

      AlertLine {
        objectName: "explanationFailed"
        visible: root.explainFailed !== ""
        width: column.width
        foreground: root.foreground
        urgent: root.urgent
        fontFamily: root.fontFamily
        text: Model.copy("issues.page.failed", { "why": root.explainFailed })
      }
    }
  }
}
