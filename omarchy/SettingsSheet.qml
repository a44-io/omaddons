import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
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
// What the sheet shows comes back from the service, so it shows what was applied.
Sheet {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property var settings: svc && svc.settings ? svc.settings : ({})
  readonly property var sections: Model.settingsSections({ "settings": settings, "config": svc ? svc.config : null,
    "configFailed": svc && svc.configFailed ? svc.configFailed : "" })
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy
  readonly property var rows: {
    var out = []
    for (var i = 0; i < sections.length; i++) out = out.concat(sections[i].rows)
    return out
  }
  // The key of the row whose box has the keys (the binary's path), or "".
  property string editing: ""
  property string editError: ""
  // Where the keys go: the box while one is open.
  readonly property Item focusItem: editing !== "" ? editField : keyItem
  property var rowItems: ({})

  signal settingRequested(string key, var value)

  title: Model.copy("settings.title")
  choosing: true
  keysText: editing !== "" ? Model.copy("settings.panel.binary.edit") : Model.copy("settings.keys")
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

  // Row `index` changed by one step: a switch flips, a number moves, a path opens its
  // box, an omaddons setting runs its `config set`.
  function change(index, step, wrap) {
    var row = rows[index]
    if (!row) return
    if (row.section === "omaddons") {
      var request = Model.configRequest(row, step, wrap)
      if (request && canChange) frame.report(svc.run(request.template, request.slots, request.label))
      return
    }
    if (row.kind === "path") {
      if (step > 0 || wrap) startEditing(row)
      return
    }
    settingRequested(row.key, Model.steppedSetting(settings, row.key, step, wrap))
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

  // The box's Enter: a full path or nothing is saved; anything else says what a path is.
  function commit() {
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

            Item {
              width: parent.width
              height: Math.max(label.implicitHeight, value.implicitHeight)

              PlainLabel {
                id: label
                objectName: "settingLabel"
                anchors.left: parent.left
                anchors.right: value.left
                anchors.rightMargin: Style.spacing.md
                anchors.verticalCenter: parent.verticalCenter
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                text: entry.modelData.label
              }

              // The value, "‹ 540 px ›" under the cursor where h and l step it.
              PlainLabel {
                id: value
                objectName: "settingValue"
                anchors.right: parent.right
                anchors.verticalCenter: parent.verticalCenter
                width: Math.min(implicitWidth, parent.width * 0.55)
                visible: !entry.editingHere
                horizontalAlignment: Text.AlignRight
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.body
                font.bold: entry.cursored
                text: entry.cursored && entry.modelData.kind !== "path" ? "‹ " + entry.modelData.text + " ›" : entry.modelData.text
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

          MouseArea {
            anchors.fill: parent
            enabled: !entry.editingHere
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
