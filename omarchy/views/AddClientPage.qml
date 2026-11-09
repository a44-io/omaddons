import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model
import "../Build.js" as Build

// Add a client by its folder, with no terminal (1n2p.32, design doc §6.4): the page the
// welcome card's Add a client folder pushes. One path box (KEYMAP's "pathbox" rows:
// Enter adds, Esc leaves the box), the path made usable as typed (Model.clientPath: a
// leading "~" is the home, it must start at "/"), then `targets add` with the path as a
// slot after `--`; the CLI says what the folder holds, and a failure shows in the panel's
// failure card. Once `status` lists a client on this computer, the page goes back: the
// panel moves on.
Item {
  id: root
  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property int rowCount: 0
  readonly property bool typing: field.activeFocus
  // What the page says under the box: the path's own problem, or that it is adding.
  property string note: ""
  readonly property int liveCount: svc && svc.targets && typeof svc.targets.length === "number"
    ? Model.liveTargets(Model.listOf(svc.targets)).length : 0
  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Model.dimColor(foreground, Commons.Color.popups.background)
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  onLiveCountChanged: if (liveCount > 0 && frame) frame.pop()

  function rowItem(i) { return null }

  // The page's own keys where the frame has them: Enter adds what the box holds.
  function handleKey(name, cursor) {
    if (name !== "enter") return false
    add()
    return true
  }

  // `targets add -- <path>`, after the path is checked here; false when nothing ran.
  function add() {
    var checked = Model.clientPath(field.text, Build.CONTEXT.HOME)
    if (checked.error !== "") {
      note = checked.error
      return false
    }
    if (!svc || !canChange) return false
    var refusal = svc.run("targets_add", { "path": checked.path }, Model.copy("addclient.title"))
    if (refusal) {
      note = String(refusal)
      return false
    }
    note = Model.copy("addclient.adding")
    return true
  }

  function fieldKey(code, text, modifiers) {
    var row = Model.keyRow(Model.keyName(code, text, modifiers), "pathbox")
    if (!row) return false
    if (row.action === "add") add()
    else if (row.action === "leave" && frame) frame.leaveField()
    return true
  }

  // A failure while adding is the panel's failure card; the note stops saying "Adding…".
  Connections {
    target: root.svc
    ignoreUnknownSignals: true
    function onLastErrorChanged() { if (root.svc && root.svc.lastError && root.note === Model.copy("addclient.adding")) root.note = "" }
  }

  Component.onCompleted: Qt.callLater(function() { field.forceActiveFocus() })

  Column {
    id: column
    width: parent.width
    spacing: Style.spacing.md

    PlainLabel {
      objectName: "addClientSentence"
      width: parent.width
      wrapMode: Text.WordWrap
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      text: Model.copy("addclient.sentence")
    }

    TextField {
      id: field
      objectName: "pathField"
      width: parent.width
      placeholderText: Model.copy("addclient.placeholder")
      foreground: root.foreground
      onTextChanged: if (root.note !== Model.copy("addclient.adding")) root.note = ""
      Keys.onPressed: function(event) { event.accepted = root.fieldKey(event.key, event.text, event.modifiers) }
    }

    Row {
      spacing: Style.spacing.md

      Button {
        objectName: "addClient"
        enabled: root.canChange
        opacity: enabled ? 1.0 : 0.4
        text: Model.copy("addclient.add")
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.add()
      }

      PlainLabel {
        objectName: "addClientNote"
        visible: text !== ""
        anchors.verticalCenter: parent.verticalCenter
        wrapMode: Text.WordWrap
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: root.note
      }
    }
  }
}
