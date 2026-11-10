import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "ui"
import "Model.js" as Model

// The restore chooser (1n2p.29, design doc §2.4), a sheet over the panel: the narrow fix
// first. From an addon (`r` on its row, its page or its Lua error) it opens on the
// newest restorable backup with that addon's settings first ("WeakAuras' settings
// (written by 5.19.2)"), all of the client's settings second, and the backup above them
// as a choice of its own (Enter lists the others, newest first, each with the version of
// that addon that wrote it): `r`, Enter, then `y` in the plan sheet. From a backup (its
// row's `r`, its page's Restore…) it offers one addon's settings (which addon, next) or
// all of them. Choosing one runs its preview through the service; the plan sheet takes
// over from there. The chooser decides no folder, file or version: the CLI's preview does
// (D7). While the client runs, or nobody can tell, it says "Quit Retail first" and
// restores nothing (M3-D29). Esc goes back a step, then closes. The choices are
// Model.restoreChooser's.
Sheet {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  property string target: ""
  property string backup: ""
  property string addon: ""
  // Where it was opened: "addon" (an addon) or "backup" (a backup).
  property string origin: "backup"
  // "scope", "backup" or "addon".
  property string step: "scope"
  // Whether this chooser asked for the client's `backup list -v` read: a backup's page
  // under it may hold that read, and keeps it.
  property bool ownsDetail: false

  readonly property var model: Model.restoreChooser({ "origin": origin, "step": step, "target": target, "addon": addon,
    "backup": backup, "backups": svc ? svc.backups : null, "detail": svc ? svc.backupsDetail : null, "lists": svc ? svc.lists : null,
    "statusTargets": svc && Array.isArray(svc.targets) ? svc.targets : [], "nowMs": frame ? frame.nowMs : Date.now() })
  readonly property var choices: model.choices
  // What the chooser lists, as text (tests and the accessible name read it).
  readonly property var choiceTexts: choices.map(function(c) { return c.text })
  readonly property string blocked: model.blocked

  title: model.title
  choosing: true
  keysText: Model.copy("restore.keys")
  choiceCount: choices.length
  choiceItem: function(i) { return choiceRepeater.itemAt(i) }
  foreground: frame ? frame.foreground : Commons.Color.foreground
  urgent: frame ? frame.urgent : Commons.Color.urgent
  accent: frame ? frame.accent : Commons.Color.accent
  fontFamily: frame ? frame.fontFamily : Style.font.family

  onCanceled: back()
  onChosen: function(index) { root.pick(index) }

  // Choices that arrive after it opened (the backups read meanwhile) start where the
  // chooser would have opened: on the narrow fix.
  property int shown: 0
  onChoiceTextsChanged: {
    if (opened && shown === 0 && choices.length > 0) choice = model.start
    shown = choices.length
  }

  // `spec`: `{target, backup}` from a backup, or `{target, addon}` from an addon; with
  // `keepDetail`, a backup's page under it holds the client's `-v` read (asked, maybe not
  // answered yet).
  function open(spec) {
    target = String(spec.target || "")
    backup = String(spec.backup || "")
    addon = String(spec.addon || "")
    origin = backup ? "backup" : "addon"
    step = "scope"
    var held = spec.keepDetail === true
      || (!!svc && !!svc.backupsDetail && !!svc.backupsDetail.selection && svc.backupsDetail.selection.target === target)
    ownsDetail = !!svc && !held
    if (ownsDetail) svc.readBackupsDetail(target)
    choice = model.start
    opened = true
    // The keys are the chooser's from the next key on.
    keyItem.forceActiveFocus()
  }

  function close() {
    opened = false
    if (svc && ownsDetail) svc.clearBackupsDetail()
    ownsDetail = false
    if (frame) frame.chooserClosed()
  }

  // Esc: back to the scopes from a list of backups or addons, then closed.
  function back() {
    if (step === "scope") {
      close()
      return
    }
    step = "scope"
    choice = model.start
  }

  // A choice: a step it opens, a backup it picks, or the restore it previews. One that
  // is off (the client runs) does nothing; the sheet says why.
  function pick(index) {
    var c = choices[index]
    if (!c || !c.enabled) return
    if (c.next) {
      step = c.next
      choice = model.start
      return
    }
    if (c.backup) {
      backup = c.backup
      step = "scope"
      choice = model.start
      return
    }
    if (!c.run || !svc) return
    if (frame) frame.report(svc.run(c.run.template, c.run.slots, c.run.label))
    close()
  }

  // The backup a list of addons restores from.
  PlainLabel {
    objectName: "chooserFrom"
    visible: root.model.from !== ""
    width: root.bodyWidth
    wrapMode: Text.WordWrap
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    text: root.model.from
  }

  PlainLabel {
    objectName: "chooserBlocked"
    visible: root.blocked !== "" && root.step !== "backup"
    width: root.bodyWidth
    wrapMode: Text.WordWrap
    color: root.urgent
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    font.bold: true
    text: root.blocked
  }

  PlainLabel {
    objectName: "chooserEmpty"
    visible: root.choices.length === 0
    width: root.bodyWidth
    wrapMode: Text.WordWrap
    color: root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.caption
    text: root.model.empty
  }

  Repeater {
    id: choiceRepeater
    objectName: "chooserChoices"
    model: root.choices

    CursorSurface {
      id: entry
      required property var modelData
      required property int index
      objectName: "restoreChoice_" + index
      width: root.bodyWidth
      implicitHeight: lines.implicitHeight + Style.spacing.md * 2
      hasCursor: root.choice === index
      opacity: modelData.enabled ? 1.0 : 0.4
      foreground: root.foreground

      Column {
        id: lines
        anchors.left: parent.left
        anchors.right: parent.right
        anchors.leftMargin: Style.spacing.lg
        anchors.rightMargin: Style.spacing.lg
        anchors.verticalCenter: parent.verticalCenter
        spacing: Style.spacing.xs

        PlainLabel {
          objectName: "choiceText"
          width: parent.width
          wrapMode: Text.WordWrap
          elide: Text.ElideNone
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          text: entry.modelData.text
        }

        // What else a choice changes (all of a client's settings).
        PlainLabel {
          objectName: "choiceNote"
          visible: entry.modelData.note !== ""
          width: parent.width
          wrapMode: Text.WordWrap
          elide: Text.ElideNone
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: entry.modelData.note
        }
      }

      // Hover moves the choice on real pointer motion; one click chooses (§4).
      MouseArea {
        id: choiceArea
        anchors.fill: parent
        hoverEnabled: true
        onPositionChanged: function(mouse) { root.hoverChoice(entry.index, choiceArea, mouse) }
        onClicked: {
          root.choice = entry.index
          root.choose(entry.index)
        }
      }
    }
  }
}
