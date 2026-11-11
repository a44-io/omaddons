import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// The Backups view (1n2p.29, design doc §2.3): a card per client (Model.backupSections),
// with its player name and whether it runs, its automatic backups and free space, how
// many backups and the room they take, its undo data, Back up and, with undo data past
// the cleanup rules, Clean up undo data… (to Issues, where its repair is); then its
// backups as two-line rows: when and why, size
// and what it proves, markers as chips ("in use", "damaged", "can't be restored"). Enter
// opens the backup's page (what it holds, Restore…, Delete…); `r` opens the restore
// chooser on it; `x` deletes it, previewed and bound (never one in use, M3-D48); `b`
// (frame-wide) backs up. A backup is named by its date and kind, never its id (UX-29);
// restores wait for the game to close (M3-D29). The keys are Model.KEYMAP's backups rows.
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var statusTargets: svc && Array.isArray(svc.targets) ? svc.targets : []
  readonly property double nowMs: frame ? frame.nowMs : Date.now()
  readonly property var sections: Model.backupSections(svc ? svc.backups : null, filter, nowMs, statusTargets)
  readonly property var rows: {
    var out = []
    for (var i = 0; i < sections.length; i++) out = out.concat(sections[i].rows)
    return out
  }
  readonly property int rowCount: rows.length
  // What the view says around its client cards (design doc §7): reading, no backups at
  // all, details out of date, a read that failed. A client's own card says when it has
  // none, so the cards are what the view counts.
  readonly property var viewState: Model.viewState("backups", { "service": svc, "filter": filter, "rows": sections.length,
    "allRows": filter ? Model.backupSections(svc ? svc.backups : null, "", nowMs, statusTargets).length : sections.length,
    "nowMs": nowMs })
  readonly property bool typing: false
  property var items: ({})

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property color accent: frame ? frame.accent : Commons.Color.accent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) { return items[i] || null }

  function handleKey(name, cursor) {
    var row = cursor >= 0 && cursor < rows.length ? rows[cursor] : null
    if (name === "enter") {
      if (row) frame.openBackup(row.target, row.id, row.title)
      return true
    }
    if (name === "restore") {
      if (row) restore(row)
      return true
    }
    if (name === "remove") {
      if (row) prune(row)
      return true
    }
    return false
  }

  // The chooser on this backup: which settings (one addon's, or all of them).
  function restore(row) {
    if (!row.restorable) {
      frame.report(Model.copy("restore.unrestorable"))
      return
    }
    frame.openChooser({ "target": row.target, "backup": row.id })
  }

  // A backup another process reads is never deleted from here (M3-D48).
  function prune(row) {
    if (!canChange) return
    if (row.busy) {
      frame.report(Model.copy("backups.busy"))
      return
    }
    run(Model.pruneRequest(row, statusTargets))
  }

  function run(request) {
    if (!canChange || !request) return
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

    ViewState {
      objectName: "viewState"
      width: parent.width
      viewState: root.viewState
      foreground: root.foreground
      fontFamily: root.fontFamily
      onAction: function(name) { if (root.frame) root.frame.stateAction(name) }
    }

    Repeater {
      objectName: "sections"
      model: root.sections

      Column {
        id: section
        required property var modelData
        // `b` backs up exactly this client when it is the scope: its button says so.
        readonly property bool scoped: root.filter === modelData.target
        width: column.width
        spacing: Style.spacing.sm

        // The card's head: the client's player name and whether it runs (design doc
        // §2.2), Back up on the right; under 480 px the button moves below the name.
        Item {
          id: head
          objectName: "cardHead"
          readonly property bool stacked: width < Style.space(480)
          width: parent.width
          height: stacked ? clientHeader.implicitHeight + Style.spacing.sm + backUp.implicitHeight
            : Math.max(clientHeader.implicitHeight, backUp.implicitHeight)

          SectionHeader {
            id: clientHeader
            objectName: "clientHeader"
            anchors.left: parent.left
            anchors.right: head.stacked ? parent.right : backUp.left
            anchors.rightMargin: head.stacked ? 0 : Style.spacing.md
            y: head.stacked ? 0 : (head.height - implicitHeight) / 2
            title: section.modelData.title
            count: section.modelData.count || ""
            foreground: root.foreground
            dim: root.dim
            fontFamily: root.fontFamily
          }

          Button {
            id: backUp
            objectName: "backUp"
            anchors.right: parent.right
            y: head.stacked ? clientHeader.implicitHeight + Style.spacing.sm : (head.height - implicitHeight) / 2
            // The same label in every scope (omad-xdwy): the tooltip names the client the
            // label leaves out ("Back up Retail"), and under one client, where `b` is
            // this button, its key ("Back up Retail (b)").
            text: Model.copy("backups.back_up")
            tooltipText: section.scoped ? Model.withKey(section.modelData.backUp.label, "backup", "backups") : section.modelData.backUp.label
            Accessible.role: Accessible.Button
            Accessible.name: section.modelData.backUp.label !== "" ? section.modelData.backUp.label : text
            enabled: root.canChange
            opacity: enabled ? 1.0 : 0.4
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.caption
            onClicked: root.run(section.modelData.backUp)
          }
        }

        // Automatic backups and free space; the backups and their room; the undo data.
        Repeater {
          objectName: "cardLines"
          model: section.modelData.lines

          PlainLabel {
            required property string modelData
            objectName: "cardLine"
            width: section.width
            wrapMode: Text.WordWrap
            elide: Text.ElideNone
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: modelData
          }
        }

        // Undo data past the cleanup rules (bmcx.4): its repair is the client's problem in
        // Issues, previewed there like any repair.
        Button {
          objectName: "cleanUp"
          visible: section.modelData.cleanUp
          text: Model.copy("backups.clean_up")
          tooltipText: Model.copy("backups.clean_up.tip")
          Accessible.role: Accessible.Button
          Accessible.name: text
          Accessible.description: tooltipText
          bordered: true
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.caption
          onClicked: root.frame.showView("issues")
        }

        Repeater {
          model: section.modelData.rows

          AddonRow {
            id: entry
            required property var modelData
            objectName: "backupRow_" + modelData.index
            width: section.width
            title: modelData.title
            meta: modelData.meta
            chips: modelData.chips
            band: modelData.band
            glyph: modelData.glyph
            actions: modelData.actions
            cursored: !!root.frame && root.frame.cursorActive && root.frame.cursor === modelData.index
            // Another build: visible, inert.
            inert: !!root.frame && root.frame.readOnly === true
            foreground: root.foreground
            dim: root.dim
            urgent: root.urgent
            accent: root.accent
            fontFamily: root.fontFamily

            Component.onCompleted: root.items[modelData.index] = entry
            Component.onDestruction: if (root.items[modelData.index] === entry) delete root.items[modelData.index]

            // A click puts the cursor on it; its buttons act (design doc §4).
            pointerGate: root.frame ? root.frame.pointerGate : null
            onHovered: root.select(modelData.index)
            // One click opens its page, as Enter does (§4, 1n2p.15).
            onActivated: {
              root.select(modelData.index)
              root.handleKey("enter", modelData.index)
            }
            onAction: function(name) {
              root.select(modelData.index)
              root.handleKey(name, modelData.index)
            }
          }
        }
      }
    }
  }
}
