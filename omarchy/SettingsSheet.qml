import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import Quickshell
import "ui"
import "Model.js" as Model

// The settings sheet (1n2p.31, design doc §2.7), `,` or the hero's cog, in sections
// (Model.settingsSections), one row per setting with its value in words; the cursored
// row says what it does. Enter (or a click) changes the row's value, the next one past
// the last; h and l step it back or on; a path is typed: Enter opens its box, Enter
// there saves, Esc keeps the one before.
// - Panel: the widget's own settings. Each change goes out as `settingRequested`:
//   BarWidget saves it through the shell's facade (`updateEntryInline`), which writes
//   shell.json and hands the widget its new settings; the widget writes no file (M3-D3).
// - omaddons: its own settings, read with `config` when the sheet opens and changed one
//   key at a time with `config set` (a template per key), applied directly, the result
//   shown, then read again: config.toml stays the one source.
// - Clients: each client's name (typed: `config set targets.<key>.name`), its game where
//   omaddons can't tell it, the default client (`targets default`), each client no
//   longer installed (Enter forgets it through the plan sheet, never a purge), Scan for
//   clients, and Add a client by its folder (its page, under the closed sheet).
// - GitHub (omad-r0to): the token's state from `status` in words, never the token; Use
//   the gh login (`auth github --from-gh`) and Forget the token (`auth github clear`)
//   where a stored token counts; setting one by hand is a command copied for a terminal.
// - Everything else: config.toml, opened in the player's editor (Model.configEditorArgv,
//   the path `config` reported).
// What the sheet shows comes back from the service, so it shows what was applied.
Sheet {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property var settings: svc && svc.settings ? svc.settings : ({})
  readonly property var sections: Model.settingsSections({ "settings": settings, "config": svc ? svc.config : null,
    "configFailed": svc && svc.configFailed ? svc.configFailed : "",
    "targets": svc && Array.isArray(svc.targets) ? svc.targets : [], "github": svc && svc.github ? svc.github : null })
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy
  readonly property var rows: {
    var out = []
    for (var i = 0; i < sections.length; i++) out = out.concat(sections[i].rows)
    return out
  }
  // The key of the row whose box has the keys (the binary's path, a client's name), or "".
  property string editing: ""
  // The row whose command was just copied (GitHub's "Set a token in a terminal"), or "".
  property string copiedKey: ""
  Timer {
    id: copiedTimer
    interval: 1500
    onTriggered: root.copiedKey = ""
  }
  property string editError: ""
  readonly property var editRow: {
    for (var i = 0; i < rows.length; i++) if (rows[i].key === editing) return rows[i]
    return null
  }
  // Where the keys go: the box while one is open.
  readonly property Item focusItem: editing !== "" ? editField : keyItem
  property var rowItems: ({})

  signal settingRequested(string key, var value)

  title: Model.copy("settings.title")
  choosing: true
  keysText: editing === "" ? Model.copy("settings.keys")
    : Model.copy(editRow && editRow.kind === "name" ? "settings.clients.name.edit" : "settings.panel.binary.edit")
  choiceCount: rows.length
  choiceItem: function(i) { return root.rowItems[i] || null }
  foreground: frame ? frame.foreground : Commons.Color.foreground
  urgent: frame ? frame.urgent : Commons.Color.urgent
  accent: frame ? frame.accent : Commons.Color.accent
  fontFamily: frame ? frame.fontFamily : Style.font.family

  onChosen: function(index) { root.change(index, 1, true) }
  onStepped: function(index, step) { root.change(index, step, false) }
  onOpenedChanged: {
    if (!opened) {
      stopEditing()
      return
    }
    // omaddons' settings as config.toml holds them now.
    if (svc && typeof svc.readConfig === "function") svc.readConfig()
  }

  // Row `index` changed by one step: a switch flips, a number moves, a path or a name
  // opens its box, a choice runs its change, an action acts (on Enter or a click only).
  function change(index, step, wrap) {
    var row = rows[index]
    if (!row) return
    if (row.kind === "choice") {
      var request = Model.configRequest(row, step, wrap)
      if (request && canChange) frame.report(svc.run(request.template, request.slots, request.label))
      return
    }
    if (row.kind === "path" || row.kind === "name") {
      if (step > 0 || wrap) startEditing(row)
      return
    }
    if (row.kind === "action") {
      if (wrap) act(row)
      return
    }
    settingRequested(row.key, Model.steppedSetting(settings, row.key, step, wrap))
  }

  // A Clients action: scan again, add a client by its folder (its page, the sheet
  // closed), or forget one no longer installed (the plain `targets rm` its own problem
  // offers, through the plan sheet).
  function act(row) {
    if (!frame) return
    if (row.action === "rescan") {
      frame.stateAction("rescan")
    } else if (row.action === "add") {
      frame.settingsOpen = false
      frame.stateAction("add")
    } else if (row.action === "forget") {
      var targets = svc && Array.isArray(svc.targets) ? svc.targets : []
      var target = null
      for (var i = 0; i < targets.length; i++) if (targets[i].id === row.id) target = targets[i]
      var forget = Model.forgetAction(target)
      if (forget && canChange) frame.runCli(forget, Model.copy("settings.clients.forget.label", { "client": row.client }))
    } else if (row.action === "from_gh") {
      // GitHub (omad-r0to): the gh login's token stored, the result line saying so.
      if (canChange) frame.report(svc.run("auth_from_gh", {}, Model.copy("settings.github.from_gh.label")))
    } else if (row.action === "clear") {
      if (canChange) frame.report(svc.run("auth_clear", {}, Model.copy("settings.github.clear.label")))
    } else if (row.action === "copy_set") {
      // The token goes on standard input in a terminal: the command, copied.
      if (svc && svc.copyCommand(Model.GITHUB_SET_COMMAND)) {
        copiedKey = row.key
        copiedTimer.restart()
      }
    } else if (row.action === "open_config") {
      // Everything else: config.toml in the player's editor, the path `config` said.
      if (Model.configEditorArgv(row.path)) Quickshell.execDetached(Model.configEditorArgv(row.path))
    }
  }

  function startEditing(row) {
    editError = ""
    editing = row.key
    editField.text = row.value
    editField.forceActiveFocus()
  }

  function stopEditing() {
    if (editing === "") return
    editing = ""
    editError = ""
    if (opened) keyItem.forceActiveFocus()
  }

  // The box's Enter. A path: a full one or nothing is saved, anything else says what a
  // path is. A client's name: one line of at most 200 characters, run as its `config
  // set` (empty brings back the built-in name).
  function commit() {
    var row = editRow
    if (row && row.kind === "name") {
      var name = editField.text.trim()
      if (name.length > 200) {
        editError = Model.copy("settings.clients.name.invalid")
        return
      }
      if (canChange) {
        var request = Model.clientNameRequest(row, name)
        frame.report(svc.run(request.template, request.slots, request.label))
      }
      stopEditing()
      return
    }
    var path = Model.typedPath(editField.text)
    if (path === null) {
      editError = Model.copy("settings.panel.binary.invalid")
      return
    }
    settingRequested(editing, path)
    stopEditing()
  }

  Repeater {
    objectName: "settingsSections"
    model: root.sections

    Column {
      id: section
      required property var modelData
      width: root.bodyWidth
      spacing: Style.spacing.xs
      bottomPadding: Style.spacing.md

      SectionHeader {
        objectName: "settingsSection"
        width: parent.width
        title: section.modelData.title
        count: ""
        foreground: root.foreground
        dim: root.dim
        fontFamily: root.fontFamily
      }

      // What the section waits for (omaddons' settings being read, or why they weren't).
      PlainLabel {
        objectName: "sectionNote"
        visible: section.modelData.note !== ""
        width: parent.width
        wrapMode: Text.WordWrap
        elide: Text.ElideNone
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: section.modelData.note
      }

      Repeater {
        model: section.modelData.rows

        CursorSurface {
          id: entry
          required property var modelData
          readonly property bool cursored: root.choice === modelData.index
          readonly property bool editingHere: root.editing !== "" && root.editing === modelData.key
          objectName: "setting_" + modelData.key
          width: section.width
          implicitHeight: lines.implicitHeight + Style.spacing.sm * 2
          hasCursor: cursored
          foreground: root.foreground

          Component.onCompleted: root.rowItems[modelData.index] = entry
          Component.onDestruction: if (root.rowItems[modelData.index] === entry) delete root.rowItems[modelData.index]

          Column {
            id: lines
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.leftMargin: Style.spacing.lg
            anchors.rightMargin: Style.spacing.lg
            anchors.verticalCenter: parent.verticalCenter
            spacing: Style.spacing.xs

            // The label, the value on its right; where both don't fit (400 px, a large
            // text zoom), the value under the label, so neither is cut (1n2p.39).
            Item {
              id: line
              objectName: "settingLine"
              readonly property bool stacked: label.implicitWidth + value.implicitWidth + Style.spacing.md > width
              width: parent.width
              height: stacked ? label.implicitHeight + (value.visible ? value.implicitHeight : 0)
                : Math.max(label.implicitHeight, value.implicitHeight)

              PlainLabel {
                id: label
                objectName: "settingLabel"
                x: 0
                width: line.stacked ? line.width : Math.max(0, line.width - value.implicitWidth - Style.spacing.md)
                y: line.stacked ? 0 : (line.height - implicitHeight) / 2
                // Stacked, it has the row's width to wrap in, and never elides.
                wrapMode: line.stacked ? Text.WordWrap : Text.NoWrap
                elide: line.stacked ? Text.ElideNone : Text.ElideRight
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                text: entry.modelData.label
              }

              // The value, "‹ 540 px ›" under the cursor where h and l step it.
              PlainLabel {
                id: value
                objectName: "settingValue"
                x: line.stacked ? 0 : line.width - width
                y: line.stacked ? label.implicitHeight : (line.height - implicitHeight) / 2
                width: line.stacked ? line.width : Math.min(implicitWidth, line.width)
                // Stacked, it wraps too (a path anywhere), never elides.
                wrapMode: line.stacked ? Text.WrapAtWordBoundaryOrAnywhere : Text.NoWrap
                elide: line.stacked ? Text.ElideNone : Text.ElideRight
                visible: !entry.editingHere
                horizontalAlignment: line.stacked ? Text.AlignLeft : Text.AlignRight
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                font.bold: entry.cursored
                // The marks only where h and l step the value; a row whose command was
                // just copied says so for a moment.
                text: root.copiedKey === entry.modelData.key ? Model.copy("settings.github.set.copied")
                  : entry.cursored && ["switch", "integer", "choice"].indexOf(entry.modelData.kind) !== -1
                  ? "‹ " + entry.modelData.text + " ›" : entry.modelData.text
              }
            }

            // What the cursored row does.
            PlainLabel {
              objectName: "settingAbout"
              visible: entry.cursored && !entry.editingHere
              width: parent.width
              wrapMode: Text.WordWrap
              elide: Text.ElideNone
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: entry.modelData.about
            }

            PlainLabel {
              objectName: "settingError"
              visible: entry.editingHere && root.editError !== ""
              width: parent.width
              wrapMode: Text.WordWrap
              elide: Text.ElideNone
              color: root.urgent
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: root.editError
            }
          }

          // Hover moves the choice on real pointer motion; one click chooses (§4).
          MouseArea {
            id: rowArea
            anchors.fill: parent
            enabled: !entry.editingHere
            hoverEnabled: true
            onPositionChanged: function(mouse) { root.hoverChoice(entry.modelData.index, rowArea, mouse) }
            onClicked: {
              root.choice = entry.modelData.index
              root.choose(entry.modelData.index)
            }
          }
        }
      }
    }
  }

  // The box a path is typed in, under the sheet's rows while open.
  TextField {
    id: editField
    objectName: "settingEdit"
    visible: root.editing !== ""
    width: root.bodyWidth
    foreground: root.foreground
    Keys.onPressed: function(event) {
      if (event.key === Qt.Key_Return || event.key === Qt.Key_Enter) {
        root.commit()
        event.accepted = true
      } else if (event.key === Qt.Key_Escape) {
        root.stopEditing()
        event.accepted = true
      }
    }
  }
}
