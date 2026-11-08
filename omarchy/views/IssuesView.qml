import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model

// The Issues view (F6): what is wrong and its fix one key away (Model.issueSections).
// Problems from `status` by severity, Enter running their remediation through the
// classifier (destructive ones through the confirm dialog); each client's Lua errors
// from `errors` (strict text only: a custom message shows its length, received reports
// only their count), Enter updating the likely source or showing its stack, `s` its raw
// details under a label that says what they may hold; the run warnings with their
// fixes. `d` (frame-wide) dismisses what the panel showed, bound to it (M3-D46).
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var sections: Model.issueSections(svc && Array.isArray(svc.targets) ? svc.targets : [],
    svc ? svc.errors : null, svc && Array.isArray(svc.statusWarnings) ? svc.statusWarnings : [], filter,
    svc && Array.isArray(svc.machineIssues) ? svc.machineIssues : [])
  readonly property var rows: Model.issueRows(sections)
  readonly property int rowCount: rows.length
  readonly property bool typing: false
  readonly property var plan: Model.dismissPlan(svc ? svc.errors : null, filter)
  // The addon row whose stack (Enter) or raw details (`s`) are open, by key.
  property string expanded: ""
  property string rawOpen: ""
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
    if (name === "raw") {
      if (row && row.kind === "addon") toggleRaw(row)
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
    if (row.action) frame.runCli(row.action, row.kind === "capture" ? "Install BugGrabber" : "Fix")
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

    Text {
      objectName: "reading"
      visible: !!root.svc && !root.svc.errors && root.sections.length === 0
      width: parent.width
      textFormat: Text.PlainText
      text: "Reading the clients…"
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
    }

    Text {
      objectName: "nothing"
      visible: !!root.svc && !!root.svc.errors && root.sections.length === 0
      width: parent.width
      textFormat: Text.PlainText
      text: "Nothing needs attention"
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.body
    }

    Button {
      objectName: "dismiss"
      visible: root.plan.length > 0
      text: "Dismiss (d)"
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

        // "Problems", "Lua errors (<client>)", "Run warnings".
        PlainLabel {
          objectName: "sectionTitle"
          width: parent.width
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          font.bold: true
          text: section.modelData.title
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

                  PlainLabel {
                    visible: entry.modelData.kind === "problem" || entry.modelData.kind === "capture"
                    color: root.dim
                    font.family: root.fontFamily
                    font.pixelSize: Style.font.caption
                    text: entry.modelData.target || ""
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

                PlainLabel {
                  objectName: "commandText"
                  visible: !!entry.modelData.text
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
                  visible: Model.copyText(entry.modelData) !== ""
                  text: root.copied === entry.modelData.key ? "Copied" : "y copies the command"
                  color: root.dim
                  font.family: root.fontFamily
                  font.pixelSize: Style.font.caption
                }

                PlainLabel {
                  objectName: "enterHint"
                  visible: !!entry.modelData.action
                  text: entry.modelData.kind === "capture" ? "Enter installs it" : "Enter runs the fix"
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

            // `s`: raw details, under the label that says what they may hold.
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
