import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "Model.js" as Model

// The restore chooser (F9), over the panel: the safest fix first. From an addon row
// (Installed or Issues `r`) it asks which backup (each with the version of that addon
// that wrote its settings, from `backup list -v`), then the scope; from a Backups row,
// the scope, and for one addon's settings, which addon. The scopes come in this order on
// purpose: the addon's settings, then the whole WTF/, labelled with everything else it
// changes (risk 11). Choosing one runs its dry run through the service; the frame's
// confirm dialog takes over from there. The chooser decides no folder, file or version:
// the CLI's dry run does (D7). While the client runs, the scopes are off (M3-D29).
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  property bool opened: false
  property string target: ""
  property string backup: ""
  property string addon: ""
  // "backup", "scope" or "addon".
  property string step: "scope"
  property int cursor: 0
  readonly property Item keyItem: keys

  readonly property string blocked: Model.restoreBlocked(svc && Array.isArray(svc.targets) ? svc.targets : [], target)
  readonly property var detail: svc && svc.backupsDetail && svc.backupsDetail.selection
    && svc.backupsDetail.selection.target === target ? svc.backupsDetail.data : null
  readonly property var backupChoices: Model.chooserBackups(svc ? svc.backups : null, detail, target, addon, frame ? frame.nowMs : Date.now())
  readonly property var scopes: Model.restoreScopes(addon)
  readonly property var addonChoices: Model.managedAddons(svc ? svc.lists : null, target)
  readonly property var choices: step === "backup" ? backupChoices.map(function(b) { return b.label })
    : step === "addon" ? addonChoices
    : scopes.map(function(s) { return s.label + (s.note ? ": " + s.note : "") })
  readonly property string title: step === "backup" ? "Restore " + addon + "'s settings on " + target + " from which backup?"
    : step === "addon" ? "Whose settings, on " + target + "?"
    : "Restore on " + target + " from backup " + backup

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family

  // Where it was opened: "addon" (an addon row) or "backup" (a Backups row).
  property string origin: "backup"

  // `spec`: `{target, backup}` from a Backups row, or `{target, addon}` from an addon row.
  function open(spec) {
    target = String(spec.target || "")
    backup = String(spec.backup || "")
    addon = String(spec.addon || "")
    origin = backup ? "backup" : "addon"
    step = backup ? "scope" : "backup"
    cursor = 0
    opened = true
    if (svc) svc.readBackupsDetail(target)
    keys.forceActiveFocus()
  }

  function close() {
    opened = false
    if (svc) svc.clearBackupsDetail()
    if (frame) frame.chooserClosed()
  }

  // Esc goes back a step, then closes.
  function back() {
    if (step === "addon") {
      step = "scope"
      addon = ""
    } else if (step === "scope" && origin === "addon") {
      step = "backup"
      backup = ""
    } else {
      close()
      return
    }
    cursor = 0
  }

  function choose() {
    if (step === "backup") {
      var picked = backupChoices[cursor]
      if (!picked) return
      backup = picked.id
      step = "scope"
      cursor = 0
      return
    }
    if (step === "addon") {
      var name = addonChoices[cursor]
      if (!name || blocked) return
      addon = name
      run("restore_addon")
      return
    }
    var scope = scopes[cursor]
    if (!scope || blocked) return
    if (scope.needsAddon) {
      step = "addon"
      cursor = 0
      return
    }
    run(scope.template)
  }

  function run(template) {
    var whole = template === "restore"
    var slots = whole ? { "target": target, "backup": backup } : { "target": target, "addon": addon, "backup": backup }
    var label = whole ? "Restore the whole WTF/" : "Restore " + addon + "'s settings"
    if (frame) frame.report(svc.run(template, slots, label))
    close()
  }

  // A dialog's keys, as Model.KEYMAP's sheet rows say: Esc and n back out a step, j/k
  // move between the choices, Enter and Space choose (never from auto-repeat, M3-D46).
  // Every other key is the chooser's: nothing behind it acts.
  function key(code, text, autoRepeat, modifiers) {
    var name = Model.keyName(code, text, modifiers)
    var action = Model.dialogKey(name, autoRepeat === true)
    if (action === "cancel") back()
    else if (action === "line") cursor = Math.max(0, Math.min(choices.length - 1, cursor + Model.keyStep(name)))
    else if (action === "press") choose()
    return true
  }

  visible: opened

  Item {
    id: keys
    objectName: "chooserKeys"
    Keys.onPressed: function(event) {
      event.accepted = root.key(event.key, event.text, event.isAutoRepeat, event.modifiers)
    }
  }

  // The panel behind stays visible, dimmed; a click outside the card closes.
  Rectangle {
    anchors.fill: parent
    color: Qt.rgba(Commons.Color.background.r, Commons.Color.background.g, Commons.Color.background.b, 0.7)

    MouseArea {
      anchors.fill: parent
      onClicked: root.close()
    }
  }

  BorderSurface {
    id: card
    objectName: "chooserCard"
    anchors.centerIn: parent
    width: Math.min(parent.width - Style.space(24), Style.space(460))
    implicitHeight: cardColumn.implicitHeight + Style.spacing.xl * 2
    color: Commons.Color.popups.background
    borderSpec: Border.flat(Commons.Color.accent, 1)
    radius: Style.cornerRadius

    MouseArea { anchors.fill: parent }

    Column {
      id: cardColumn
      anchors.left: parent.left
      anchors.right: parent.right
      anchors.verticalCenter: parent.verticalCenter
      anchors.leftMargin: Style.space(14)
      anchors.rightMargin: Style.space(14)
      spacing: Style.spacing.md

      PlainLabel {
        objectName: "chooserTitle"
        width: parent.width
        wrapMode: Text.WordWrap
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
        text: root.title
      }

      Text {
        objectName: "chooserBlocked"
        visible: root.blocked !== "" && root.step !== "backup"
        width: parent.width
        textFormat: Text.PlainText
        text: "Restores are off: " + root.blocked
        color: root.urgent
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }

      Text {
        visible: root.choices.length === 0
        width: parent.width
        textFormat: Text.PlainText
        text: root.step === "backup" ? "No backup of this client can be restored" : "Nothing to choose"
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }

      Repeater {
        objectName: "chooserChoices"
        model: root.choices

        CursorSurface {
          required property string modelData
          required property int index
          width: cardColumn.width
          implicitHeight: choice.implicitHeight + Style.spacing.md * 2
          hasCursor: root.cursor === index
          opacity: root.step !== "backup" && root.blocked !== "" ? 0.4 : 1.0
          foreground: root.foreground

          PlainLabel {
            id: choice
            anchors.left: parent.left
            anchors.right: parent.right
            anchors.leftMargin: Style.spacing.lg
            anchors.rightMargin: Style.spacing.lg
            anchors.verticalCenter: parent.verticalCenter
            wrapMode: Text.WordWrap
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            text: modelData
          }

          MouseArea {
            anchors.fill: parent
            onClicked: {
              root.cursor = index
              root.choose()
            }
          }
        }
      }

      Text {
        width: parent.width
        textFormat: Text.PlainText
        text: "j/k to move, Enter to choose, Esc to go back"
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
      }
    }
  }
}
