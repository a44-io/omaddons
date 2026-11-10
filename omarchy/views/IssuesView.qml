import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// The Issues view (1n2p.28, design doc §2.3, §6.5, §6.6): every problem in words, each
// with one named fix (Model.issueSections). The machine's problems come first ("This
// computer"), then each client's under its player name; then what `status` recommends
// and no problem already offers ("Suggested"), then the notes of its warnings. A row is
// its severity word, one sentence and its buttons: the fix (a repair the doctor makes
// previews in the plan sheet and runs bound to that preview, M4-D44; a reinstall or an
// update previews through its template; anything else runs through the classifier, or
// opens the view that does it), Explain (the problem's page, ProblemPage.qml, with the
// doctor's explanation) and Copy command for what the panel never runs (a forced
// repair, a purge: §6.5.4, with why). Enter runs the fix, else explains; `e` explains;
// `y` copies. The keys are Model.KEYMAP's issues rows.
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var statusTargets: svc && Array.isArray(svc.targets) ? svc.targets : []
  readonly property var warnings: svc && Array.isArray(svc.statusWarnings) ? svc.statusWarnings : []
  readonly property var machineIssues: svc && Array.isArray(svc.machineIssues) ? svc.machineIssues : []
  readonly property var commands: svc && Array.isArray(svc.commands) ? svc.commands : []
  readonly property var sections: Model.issueSections(statusTargets, warnings, filter, machineIssues, commands)
  readonly property var rows: Model.issueRows(sections)
  // What the view says around its rows (design doc §7): checking, or nothing to fix. It
  // counts problems only: suggestions and notes stay listed under "Nothing needs fixing."
  readonly property var viewState: Model.viewState("issues", { "service": svc, "filter": filter,
    "rows": Model.problemCount(sections),
    "allRows": filter ? Model.problemCount(Model.issueSections(statusTargets, warnings, "", machineIssues, commands)) : Model.problemCount(sections),
    "nowMs": frame ? frame.nowMs : Date.now() })
  readonly property int rowCount: rows.length
  readonly property bool typing: false
  // The row whose command `y` copied last, by key, said for a moment (the copy flash).
  property string copied: ""

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property color accent: frame ? frame.accent : Commons.Color.accent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) {
    for (var s = 0; s < sectionRepeater.count; s++) {
      var section = sectionRepeater.itemAt(s)
      var found = section ? section.rowAt(i) : null
      if (found) return found
    }
    return null
  }

  function handleKey(name, cursor) {
    var row = cursor >= 0 && cursor < rows.length ? rows[cursor] : null
    if (name === "enter") {
      if (row) activate(row)
      return true
    }
    if (name === "explain") {
      if (row && row.explain) openPage(row)
      return true
    }
    // `y`: the command a row shows instead of running (a purge, a forced repair), copied
    // to run from a terminal.
    if (name === "copy") {
      if (row) copyRow(row)
      return true
    }
    return false
  }

  // Enter: the row's fix, else its explanation.
  function activate(row) {
    if (row.primary === "fix") return runFix(row)
    if (row.primary === "explain") return openPage(row)
    return false
  }

  // A button of a row, or of its page: its fix, its page, or its command copied.
  function press(row, name) {
    if (name === "fix") return runFix(row)
    if (name === "explain") return openPage(row)
    if (name === "copy") return copyRow(row)
    return false
  }

  // The row's fix: a template (a repair the doctor makes, bound to its preview; a
  // reinstall or update, previewed first), the CLI's own argv through the classifier, or
  // the view or frame action that does it. Never the remediation argv of a repair.
  function runFix(row) {
    if (row.fix) {
      if (canChange) frame.report(svc.run(row.fix.template, row.fix.slots, row.fix.label))
      return true
    }
    if (row.action) {
      frame.runCli(row.action, row.fixLabel)
      return true
    }
    if (row.go) return go(row.go)
    return false
  }

  // A fix the panel has its own way to: Check now, a refresh, reinstalling the panel, or
  // the view that shows it (reading its data again), scoped to the client it is for.
  function go(where) {
    if (where.kind === "check") return frame.checkNow()
    if (where.kind === "refresh") return svc.refresh()
    if (where.kind === "widget") {
      if (canChange) svc.reinstallWidget()
      return true
    }
    // Taking over folders is the first-run flow's, once it registers (as Addons' Take over…).
    if (where.kind === "adopt" && frame.keyHandlers && typeof frame.keyHandlers["adopt"] === "function") {
      if (canChange) frame.keyHandlers["adopt"](frame, where.target)
      return true
    }
    if (where.target) frame.setFilter(where.target)
    frame.showView(where.view)
    if (where.view === "errors" || where.view === "backups") svc.readAgain(where.view)
    return true
  }

  // The problem's page, on the navigation stack (§2.4): what the doctor says of it.
  function openPage(row) {
    if (!row.explain) return false
    return frame.push({ "title": row.severityWord, "component": pageComponent, "props": { "rowKey": row.key, "initial": row, "owner": root } })
  }

  function copyRow(row) {
    if (!svc || !svc.copyCommand(Model.copyText(row))) return false
    copied = row.key
    copiedTimer.restart()
    return true
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  Timer {
    id: copiedTimer
    interval: 1500
    onTriggered: root.copied = ""
  }

  Component {
    id: pageComponent
    ProblemPage {}
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

    Repeater {
      id: sectionRepeater
      objectName: "sections"
      model: root.sections

      Column {
        id: section
        required property var modelData
        objectName: "issuesSection_" + modelData.key
        width: column.width
        spacing: Style.spacing.xs

        function rowAt(i) {
          for (var r = 0; r < rowRepeater.count; r++) {
            var item = rowRepeater.itemAt(r)
            if (item && item.modelData.index === i) return item
          }
          return null
        }

        // "This computer", a client's player name with its problems, "Suggested", "Notes".
        SectionHeader {
          objectName: "issuesSection"
          width: parent.width
          title: section.modelData.title
          count: section.modelData.count
          foreground: root.foreground
          dim: root.dim
          fontFamily: root.fontFamily
        }

        Repeater {
          id: rowRepeater
          model: section.modelData.rows

          Item {
            id: entry
            required property var modelData
            objectName: "issueRow_" + modelData.index
            width: section.width
            implicitHeight: body.implicitHeight + Style.spacing.md * 2
            height: implicitHeight
            readonly property bool cursored: !!root.frame && root.frame.cursorActive && root.frame.cursor === entry.modelData.index
            readonly property string glyph: entry.modelData.glyph ? String.fromCodePoint(entry.modelData.glyph) : ""

            Accessible.role: Accessible.ListItem
            Accessible.name: [entry.modelData.severityWord, entry.modelData.title].filter(function(p) { return p !== "" }).join(": ")

            CursorSurface {
              anchors.fill: parent
              hasCursor: entry.cursored
              foreground: root.foreground
              accent: root.accent
            }

            // The whole row is the target (§4, 1n2p.15): the pointer moving over it puts
            // the cursor on it (through the frame's PointerMoveGate), one click runs
            // Enter (its fix, previewed first, else its explanation). Its buttons take
            // their own clicks.
            MouseArea {
              id: rowArea
              anchors.fill: parent
              hoverEnabled: true
              onPositionChanged: function(mouse) {
                if (!root.frame || root.frame.pointerGate.moved(rowArea, mouse)) root.select(entry.modelData.index)
              }
              onClicked: {
                root.select(entry.modelData.index)
                root.handleKey("enter", entry.modelData.index)
              }
            }

            // A problem's band: urgent for broken or needs attention (§5.3).
            Rectangle {
              objectName: "band"
              width: Style.space(3)
              height: parent.height
              color: root.urgent
              visible: entry.modelData.band === "urgent"
            }

            PlainLabel {
              id: glyphLabel
              objectName: "glyph"
              x: Style.spacing.lg
              y: Style.spacing.md
              width: Style.font.icon + Style.spacing.sm
              color: entry.modelData.band === "urgent" ? root.urgent : root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.icon
              text: entry.glyph
            }

            Column {
              id: body
              x: glyphLabel.x + glyphLabel.width + Style.spacing.sm
              y: Style.spacing.md
              width: entry.width - x - Style.spacing.lg
              spacing: Style.spacing.xs

              // "Broken", "Needs attention", "Worth fixing", "Minor" (§6.3). An urgent
              // one is foreground beside the urgent glyph, which carries the colour
              // (§5.1: urgent is a mark's colour, never text's; 1n2p.40).
              PlainLabel {
                objectName: "severity"
                visible: entry.modelData.severityWord !== ""
                width: parent.width
                color: entry.modelData.urgent ? root.foreground : root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
                text: entry.modelData.severityWord
              }

              // The problem, the suggestion or the note: one sentence, wrapped, never elided.
              PlainLabel {
                objectName: "headline"
                width: parent.width
                wrapMode: Text.WordWrap
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                text: entry.modelData.title
              }

              // A fix that is prose: what to do, as `status` says it.
              PlainLabel {
                objectName: "prose"
                visible: entry.modelData.prose !== ""
                width: parent.width
                wrapMode: Text.WordWrap
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                text: entry.modelData.prose
              }

              // Why a command is no button (§6.5.4), then the command, code-styled.
              AlertLine {
                objectName: "terminalNote"
                visible: entry.modelData.note !== ""
                width: parent.width
                glyph: 0xF0026
                foreground: root.foreground
                urgent: root.urgent
                fontFamily: root.fontFamily
                text: entry.modelData.note
              }

              PlainLabel {
                objectName: "commandText"
                visible: entry.modelData.text !== ""
                width: parent.width
                wrapMode: Text.WrapAnywhere
                color: root.dim
                font.family: "monospace"
                font.pixelSize: Style.font.caption
                text: entry.modelData.text
              }

              // The named fix, Explain and Copy command (§2.3); a key's hint on hover.
              Flow {
                objectName: "buttons"
                visible: entry.modelData.buttons.length > 0
                width: parent.width
                spacing: Style.spacing.sm

                Repeater {
                  model: entry.modelData.buttons

                  Button {
                    required property var modelData
                    objectName: "issueButton_" + modelData.name
                    text: modelData.name === "copy" && root.copied === entry.modelData.key ? Model.copy("issues.copied") : modelData.text
                    tooltipText: modelData.tooltip
                    Accessible.role: Accessible.Button
                    Accessible.name: text
                    // Never wider than the row: at 2x text on 400 px a long fix
                    // ("Take over Retail's addons…") shrinks, then elides (1n2p.40).
                    width: Math.min(implicitWidth, body.width)
                    // A fix waits for a compatible answer and no change running; reading
                    // why and copying a command never do.
                    enabled: modelData.name !== "fix" || root.canChange
                    opacity: enabled ? 1.0 : 0.4
                    bordered: true
                    foreground: root.foreground
                    fontFamily: root.fontFamily
                    fontSize: Style.font.bodySmall
                    onClicked: {
                      root.select(entry.modelData.index)
                      root.press(entry.modelData, modelData.name)
                    }
                  }
                }
              }
            }
          }
        }
      }
    }
  }
}
