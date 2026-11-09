import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// The Issues view (F6): what is wrong and its fix one key away (Model.issueSections).
// Problems from `status` by severity: Enter previews a repair the doctor can make (its
// confirmed run bound to that preview, M4-D44), runs another fix through the classifier
// (destructive ones through the confirm dialog), or shows the doctor's explanation; a
// repair that needs deliberate consent (`--force --yes`) shows its terminal command
// instead, never a button. Each client's Lua errors
// from `errors` (strict text only: a custom message shows its length, received reports
// only their count), Enter updating the likely source or showing its stack, `u`
// updating it, a button on the cursored row showing its raw details under a label that
// says what they may hold; the run warnings with their fixes. `d` (frame-wide)
// dismisses what the panel showed, bound to it (M3-D46). The keys are Model.KEYMAP's
// issues rows.
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var sections: Model.issueSections(svc && Array.isArray(svc.targets) ? svc.targets : [],
    svc ? svc.errors : null, svc && Array.isArray(svc.statusWarnings) ? svc.statusWarnings : [], filter,
    svc && Array.isArray(svc.machineIssues) ? svc.machineIssues : [])
  readonly property var rows: Model.issueRows(sections)
  // What the view says around its rows (design doc §7): checking, nothing to fix, errors
  // read only in part, a read that failed.
  readonly property var viewState: Model.viewState("issues", { "service": svc, "filter": filter, "rows": rows.length,
    "allRows": filter ? Model.issueRows(Model.issueSections(svc && Array.isArray(svc.targets) ? svc.targets : [], svc ? svc.errors : null,
      svc && Array.isArray(svc.statusWarnings) ? svc.statusWarnings : [], "", svc && Array.isArray(svc.machineIssues) ? svc.machineIssues : [])).length
      : rows.length, "nowMs": frame ? frame.nowMs : Date.now() })
  readonly property int rowCount: rows.length
  readonly property bool typing: false
  readonly property var plan: Model.dismissPlan(svc ? svc.errors : null, filter)
  // The addon row whose stack (Enter) or raw details (its button) are open, by key.
  property string expanded: ""
  property string rawOpen: ""
  // The problem row whose explanation (`doctor explain`) is open, by key.
  property string explainOpen: ""
  // The row whose command `y` copied last, by key.
  property string copied: ""
  // Each row's delegate, by its cursor index (for rowItem).
  property var items: ({})

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) { return items[i] || null }

  function handleKey(name, cursor) {
    var row = cursor >= 0 && cursor < rows.length ? rows[cursor] : null
    if (name === "enter") {
      if (row) activate(row)
      return true
    }
    // `u`: the update of the addon a Lua error comes from, when one is waiting.
    if (name === "update") {
      if (row && row.kind === "addon" && row.updateAvailable) activate(row)
      return true
    }
    // `r`: the erroring addon's settings from a backup (the restore chooser, F9).
    if (name === "restore") {
      if (row && row.kind === "addon") frame.openChooser({ "target": row.target, "addon": row.addon })
      return true
    }
    // `y`: the command a row shows instead of running (a purge, a forced repair), copied
    // to run from a terminal.
    if (name === "copy") {
      if (row && svc && svc.copyCommand(Model.copyText(row))) copied = row.key
      return true
    }
    return false
  }

  function activate(row) {
    if (row.kind === "addon") {
      if (row.updateAvailable) {
        if (canChange) frame.report(svc.run("update_addon", { "target": row.target, "addon": row.addon }, "Update " + row.addon,
          Model.shownForAddon(svc.lists, row.target, row.addon)))
      } else {
        expanded = expanded === row.key ? "" : row.key
      }
      return
    }
    // A repair the doctor makes: its preview, through the bound template (never the
    // remediation argv the CLI printed).
    if (row.kind === "problem" && row.fix) {
      if (canChange) frame.report(svc.run(row.fix.template, row.fix.slots, row.fix.label))
      return
    }
    if (row.action) {
      frame.runCli(row.action, row.kind === "capture" ? "Install BugGrabber" : "Fix")
      return
    }
    // No fix to run here (a forced repair, a command for a terminal): why, from the doctor.
    if (row.kind === "problem" && row.explain) toggleExplain(row)
  }

  // An explanation is a read: it opens while a change runs, as raw details do.
  function toggleExplain(row) {
    if (!svc) return
    if (explainOpen === row.key) {
      explainOpen = ""
      svc.clearExplanation()
      return
    }
    explainOpen = row.key
    svc.readExplanation(row.key, row.explain.argv)
  }

  function toggleRaw(row) {
    if (rawOpen === row.key) {
      rawOpen = ""
      svc.clearRawErrors()
      return
    }
    rawOpen = row.key
    svc.readRawErrors(row.target, row.addon)
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
      objectName: "dismiss"
      visible: root.plan.length > 0
      text: Model.withKey("Dismiss", "dismiss", "issues")
      tooltipText: Model.keyTip("dismiss", "issues")
      enabled: root.canChange
      opacity: enabled ? 1.0 : 0.4
      bordered: true
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      onClicked: root.frame.dismissErrors()
    }

    Repeater {
      objectName: "sections"
      model: root.sections

      Column {
        id: section
        required property var modelData
        width: column.width
        spacing: Style.spacing.sm

        // "Problems", "Lua errors on <client>", "Run warnings".
        SectionHeader {
          objectName: "issuesSection"
          width: parent.width
          title: section.modelData.title
          foreground: root.foreground
          dim: root.dim
          fontFamily: root.fontFamily
        }

        Repeater {
          model: section.modelData.rows

          Column {
            id: entry
            required property var modelData
            objectName: "issueRow_" + modelData.index
            width: section.width
            spacing: Style.spacing.sm
            readonly property bool open: root.expanded === modelData.key
            readonly property bool rawShown: root.rawOpen === modelData.key
            readonly property var raw: rawShown ? Model.rawLines(root.svc ? root.svc.rawErrors : null, modelData) : null
            // The doctor's explanation of this row only: its lines, or why it could not
            // be read (view-bound, as raw details).
            readonly property bool explainShown: root.explainOpen === modelData.key
            readonly property var explanationOf: explainShown && root.svc ? root.svc.explanation : null
            readonly property var explained: explainShown ? Model.explainFor(explanationOf, modelData) : null
            readonly property string explainFailed: !!explanationOf && !!explanationOf.selection
              && explanationOf.selection.key === modelData.key && typeof explanationOf.failed === "string" ? explanationOf.failed : ""

            Component.onCompleted: root.items[modelData.index] = entry
            Component.onDestruction: if (root.items[modelData.index] === entry) delete root.items[modelData.index]

            CursorSurface {
              width: parent.width
              implicitHeight: rowColumn.implicitHeight + Style.spacing.md * 2
              hasCursor: !!root.frame && root.frame.cursorActive && root.frame.cursor === entry.modelData.index
              foreground: root.foreground

              Column {
                id: rowColumn
                anchors.left: parent.left
                anchors.right: parent.right
                anchors.leftMargin: Style.spacing.lg
                anchors.rightMargin: Style.spacing.lg
                anchors.verticalCenter: parent.verticalCenter
                spacing: Style.spacing.sm

                // The row's headline: a problem's severity and title, an addon's counts
                // and likely-source confidence, a missing BugGrabber, a warning.
                Row {
                  spacing: Style.spacing.lg

                  PlainLabel {
                    visible: entry.modelData.kind === "problem"
                    text: entry.modelData.kind === "problem" ? entry.modelData.severity : ""
                    color: entry.modelData.urgent ? root.urgent : root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                    font.bold: true
                  }

                  PlainLabel {
                    objectName: "headline"
                    color: root.foreground
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.body
                    font.bold: entry.modelData.kind === "addon"
                    text: entry.modelData.kind === "addon" ? entry.modelData.addon : entry.modelData.title
                  }

                  PlainLabel {
                    objectName: "counts"
                    visible: entry.modelData.kind === "addon"
                    color: root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                    text: entry.modelData.kind === "addon"
                      ? entry.modelData.count + "×" + (entry.modelData["new"] > 0 ? "  " + entry.modelData["new"] + " new" : "")
                        + "  likely source " + Model.confidenceDots(entry.modelData.confidence)
                        + (entry.modelData.updateAvailable ? "  · " + (entry.modelData.latest || "") + " available (may fix it)" : "")
                      : ""
                  }

                  // Problems of every client mix under All: each names its client by its
                  // player name; under one client the scope already does (§2.1).
                  PlainLabel {
                    objectName: "problemClient"
                    visible: root.filter === "" && entry.modelData.kind === "problem"
                    color: root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                    text: entry.modelData.client || ""
                  }
                }

                // Each error's strict line (an addon), or the command shown as text
                // when it is no button.
                Repeater {
                  model: entry.modelData.kind === "addon" ? entry.modelData.lines : []

                  PlainLabel {
                    required property string modelData
                    width: rowColumn.width
                    elide: Text.ElideRight
                    color: root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                    text: modelData
                  }
                }

                // A command that needs deliberate consent (`--force`, `--yes`): read why when
                // the doctor explains it, then run it yourself.
                PlainLabel {
                  objectName: "consentNote"
                  visible: entry.modelData.kind === "problem" && entry.modelData.forced === true
                  width: rowColumn.width
                  wrapMode: Text.WordWrap
                  color: root.urgent
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  text: entry.modelData.explain
                    ? "A deliberate repair: read why (" + Model.keyHint("enter", "issues") + "), then run this in a terminal:"
                    : "This needs your deliberate consent: run it in a terminal:"
                }

                // The command, when it is no button; a repair's is not shown, since its
                // preview lists what it does.
                PlainLabel {
                  objectName: "commandText"
                  visible: !!entry.modelData.text && !entry.modelData.fix
                  width: rowColumn.width
                  wrapMode: Text.WordWrap
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  text: entry.modelData.text || ""
                }

                // What an irreversible command (a purge) deletes, and why it is no button.
                PlainLabel {
                  objectName: "irreversibleNote"
                  visible: !!entry.modelData.note
                  width: rowColumn.width
                  wrapMode: Text.WordWrap
                  color: root.urgent
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  text: entry.modelData.note || ""
                }

                PlainLabel {
                  objectName: "copyHint"
                  visible: Model.copyText(entry.modelData) !== "" && !entry.modelData.fix
                  text: root.copied === entry.modelData.key ? "Copied"
                    : Model.keyHint("copy", "issues") + " copies the command"
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                }

                // A problem's hint is its row's (Model.problemRow: previews the repair,
                // runs the fix, explains it); a capture or warning row's says what Enter runs.
                PlainLabel {
                  objectName: "enterHint"
                  visible: entry.modelData.kind === "problem" ? !!entry.modelData.hint : !!entry.modelData.action
                  text: entry.modelData.kind === "problem" ? (entry.modelData.hint || "")
                    : Model.keyHint("enter", "issues") + (entry.modelData.kind === "capture" ? " installs it" : " runs the fix")
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                }
              }

              MouseArea {
                anchors.fill: parent
                onClicked: root.select(entry.modelData.index)
                onDoubleClicked: root.handleKey("enter", entry.modelData.index)
              }
            }

            // Enter on a problem with no fix to run here: the doctor's explanation, its
            // lines as plain text, under the row.
            Column {
              objectName: "explanation"
              visible: entry.explainShown
              width: entry.width
              leftPadding: Style.spacing.lg
              spacing: Style.spacing.sm

              Repeater {
                objectName: "explanationLines"
                model: entry.explained || []

                PlainLabel {
                  required property string modelData
                  width: entry.width - Style.spacing.lg * 2
                  wrapMode: Text.WordWrap
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  text: modelData
                }
              }

              Text {
                objectName: "explanationReading"
                visible: entry.explainShown && entry.explained === null && entry.explainFailed === ""
                textFormat: Text.PlainText
                text: "Reading…"
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }

              PlainLabel {
                objectName: "explanationFailed"
                visible: entry.explainFailed !== ""
                width: entry.width - Style.spacing.lg * 2
                wrapMode: Text.WordWrap
                color: root.urgent
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                text: "Could not read the explanation: " + entry.explainFailed
              }
            }

            // Enter on an addon without an update: its strict stack.
            Repeater {
              objectName: "stack"
              model: entry.open ? entry.modelData.stack : []

              PlainLabel {
                required property string modelData
                width: entry.width - Style.spacing.lg
                x: Style.spacing.lg
                elide: Text.ElideRight
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                text: modelData
              }
            }

            // An addon's raw details, from the row under the cursor (or the one showing
            // them): a button, no key (design doc §3.3 moved `s` to Search).
            Button {
              objectName: "rawButton"
              x: Style.spacing.lg
              visible: entry.modelData.kind === "addon" && (entry.rawShown || (!!root.frame
                && root.frame.cursorActive && root.frame.cursor === entry.modelData.index))
              text: entry.rawShown ? "Hide raw details" : "Raw details…"
              tooltipText: "Raw details may contain other players' names and your own code"
              bordered: true
              foreground: root.foreground
              fontFamily: root.fontFamily
              fontSize: Style.font.caption
              onClicked: root.toggleRaw(entry.modelData)
            }

            // Raw details, under the label that says what they may hold.
            Column {
              objectName: "raw"
              visible: entry.rawShown
              width: entry.width
              leftPadding: Style.spacing.lg
              spacing: Style.spacing.sm

              Text {
                objectName: "rawLabel"
                textFormat: Text.PlainText
                text: "raw details: may contain other players' names and your own code"
                color: root.urgent
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
              }

              Repeater {
                objectName: "rawLines"
                model: entry.raw || []

                PlainLabel {
                  required property string modelData
                  width: entry.width - Style.spacing.lg * 2
                  wrapMode: Text.WrapAnywhere
                  color: root.foreground
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                  text: modelData
                }
              }

              Text {
                visible: entry.rawShown && entry.raw === null
                textFormat: Text.PlainText
                text: "Reading…"
                color: root.dim
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
              }
            }
          }
        }

        // Received reports (a count, never a name), an incomplete read.
        Repeater {
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
      }
    }
  }
}
