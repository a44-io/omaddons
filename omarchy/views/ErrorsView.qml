import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// The Errors view (1n2p.27, design doc §2.3, §6.6): Lua errors from `errors`, per client
// (Model.errorSections): its header (its name, its last play session, its new errors),
// a card where it records no errors (Install BugGrabber, the CLI's own command, previewed
// as any change), then each addon with errors, the ones with new errors first. A row
// says who most likely threw them, in words, how often, how many are new and when
// last, and the first error's message as the widget may show it (an addon's own
// message only as its length, M3-D21); errors other players sent are a count. Enter
// opens the addon's error page (ErrorPage.qml) with every error and its stack; `u`
// updates the addon when a release waits; `r` restores its settings; `y` copies a
// report (never raw details); `d` (frame-wide) marks what the panel shows as seen,
// bound to that read (M3-D46). The keys are Model.KEYMAP's errors rows.
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var statusTargets: svc && Array.isArray(svc.targets) ? svc.targets : []
  readonly property double nowMs: frame ? frame.nowMs : Date.now()
  readonly property var sections: Model.errorSections(svc ? svc.errors : null, statusTargets, filter, nowMs)
  readonly property var rows: Model.errorRows(sections)
  // What the view says around its sections (design doc §7): reading, a read that
  // failed, errors read only in part, no client recording errors. A client's card
  // counts as something shown, as Backups' cards do.
  readonly property var viewState: Model.viewState("errors", { "service": svc, "filter": filter, "rows": sections.length,
    "allRows": filter ? Model.errorSections(svc ? svc.errors : null, statusTargets, "", nowMs).length : sections.length, "nowMs": nowMs })
  readonly property int rowCount: rows.length
  readonly property bool typing: false
  readonly property var plan: Model.dismissPlan(svc ? svc.errors : null, filter)

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
      if (row) openPage(row)
      return true
    }
    if (name === "update") {
      if (row && row.updateAvailable) update(row)
      return true
    }
    if (name === "restore") {
      if (row) frame.openChooser({ "target": row.target, "addon": row.addon })
      return true
    }
    if (name === "copy") {
      if (row) copyReport(row)
      return true
    }
    return false
  }

  function update(row) {
    if (!canChange) return false
    return frame.report(svc.run("update_addon", { "target": row.target, "addon": row.addon }, "Update " + row.addon,
      Model.shownForAddon(svc.lists, row.target, row.addon)))
  }

  // `y`: the report on the addon's errors, strict text only (Model.errorReport). Whether
  // there was a report to copy; the flash follows in the corner toast once wl-copy
  // answers (omad-92d5).
  function copyReport(row) {
    if (!svc) return false
    var addon = row.addon
    var shown = frame
    return svc.copyCommand(Model.errorReport(row, statusTargets), function(ok) {
      if (shown) shown.flash(Model.copyFlash(ok, Model.copy("errors.copied", { "addon": addon })), ok)
    })
  }

  // Enter: the addon's error page, on the navigation stack (§2.4).
  function openPage(row) {
    return frame.push({ "title": row.addon, "component": pageComponent, "props": { "rowKey": row.key, "initial": row } })
  }

  // A page's Install BugGrabber: the CLI's own command, previewed as any install is.
  function installCapture(section) {
    if (!section.capture || !section.capture.action || !canChange) return false
    return frame.runCli(section.capture.action, Model.copy("errors.capture.install"))
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  Component {
    id: pageComponent
    ErrorPage {}
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
      // The key is the tooltip's, never the label's (omad-vqkd).
      text: "Mark as seen"
      tooltipText: Model.keyTip("dismiss", "errors")
      Accessible.role: Accessible.Button
      Accessible.name: text
      enabled: root.canChange
      opacity: enabled ? 1.0 : 0.4
      bordered: true
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: Style.font.bodySmall
      onClicked: root.frame.dismissErrors()
    }


    Repeater {
      id: sectionRepeater
      objectName: "sections"
      model: root.sections

      Column {
        id: section
        required property var modelData
        required property int index
        objectName: "errorSection_" + modelData.target
        width: column.width
        spacing: Style.spacing.sm

        function rowAt(i) {
          for (var r = 0; r < rowRepeater.count; r++) {
            var item = rowRepeater.itemAt(r)
            if (item && item.modelData.index === i) return item
          }
          return null
        }

        // The client: its player name, its last session, its new errors (§2.2).
        SectionHeader {
          objectName: "clientHeader"
          width: parent.width
          title: section.modelData.title
          count: section.modelData.count
          foreground: root.foreground
          dim: root.dim
          fontFamily: root.fontFamily
        }

        // No BugGrabber here: a card, with its install when a build is known.
        NoticeCard {
          objectName: "captureCard"
          visible: !!section.modelData.capture
          width: parent.width
          kind: "info"
          title: section.modelData.capture ? section.modelData.capture.text : ""
          buttons: section.modelData.capture && section.modelData.capture.action
            ? [{ "name": "install", "text": Model.copy("errors.capture.install") }] : []
          foreground: root.foreground
          fontFamily: root.fontFamily
          // The view's own section, not `modelData`: that copy turns the action's argv
          // into a list the classifier refuses.
          onClicked: function(name) { if (name === "install") root.installCapture(root.sections[section.index]) }
        }

        Repeater {
          id: rowRepeater
          model: section.modelData.rows

          Column {
            id: entry
            required property var modelData
            objectName: "errorRow_" + modelData.index
            width: section.width
            spacing: 0

            AddonRow {
              objectName: "addonRow"
              width: parent.width
              title: entry.modelData.addon
              chips: entry.modelData.chips
              meta: entry.modelData.meta
              band: entry.modelData.band
              glyph: String.fromCodePoint(0xF00E4)
              actions: entry.modelData.actions
              cursored: !!root.frame && root.frame.cursorActive && root.frame.cursor === entry.modelData.index
              foreground: root.foreground
              dim: root.dim
              urgent: root.urgent
              accent: root.accent
              fontFamily: root.fontFamily
              pointerGate: root.frame ? root.frame.pointerGate : null
              onHovered: root.select(entry.modelData.index)
              // One click opens, as Enter does (§4, 1n2p.15).
              onActivated: {
                root.select(entry.modelData.index)
                root.handleKey("enter", entry.modelData.index)
              }
              onAction: function(name) {
                root.select(entry.modelData.index)
                root.handleKey(name, entry.modelData.index)
              }
            }

            // The first error's message, as the widget may show it.
            PlainLabel {
              objectName: "message"
              visible: entry.modelData.message !== ""
              x: Style.spacing.xl * 2
              width: parent.width - x - Style.spacing.lg
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: entry.modelData.message
            }
          }
        }

        // A read cut short, the errors other players sent (a count, never a name), or
        // nothing recorded.
        Repeater {
          model: section.modelData.lines.concat(section.modelData.empty ? [Model.copy("errors.none")] : [])

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
