import QtQuick
import QtQuick.Controls
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model

// A sheet (design doc §2.4, §5.5): every decision the panel asks for. A dimmed backdrop
// (a click on it cancels), a centred card at most 480 px wide, a title, a body that
// scrolls past 80 % of the panel's height, and Cancel then the confirm button. It opens
// on Cancel and owns the keyboard while open, as Model.KEYMAP's dialog rows say: Esc and
// n cancel; j, k, ↓ and ↑ scroll the body; h, l, ←, →, Tab and Shift+Tab move between
// the buttons; y confirms, and Enter or Space press the focused button. A key held from
// before the sheet opened reaches it only as auto-repeat, which never confirms (M3-D46).
// Every other key is the sheet's: nothing beneath it acts.
//
// With `reference`, it is the key reference (`?`, design doc §2.4's help overlay): no
// buttons, a line saying how it closes, and KEYMAP's help rows (`?` and Esc close it,
// j/k scroll it, every other key is inert). It opens at the top every time.
//
// With `choosing`, it is a list to choose from (the restore chooser, 1n2p.29; the
// settings sheet, 1n2p.31): no buttons, `keysText` under the body, and the dialog rows
// read as a list's: j/k move `choice` among `choiceCount` choices (the body scrolls to
// show it), Enter and Space choose it (`chosen`, never from auto-repeat), h/l and
// Tab/Shift+Tab step it back or on (`stepped`: a setting's value), Esc and n cancel,
// and `y` is inert.
Item {
  id: root

  property bool opened: false
  property bool reference: false
  property bool choosing: false
  property int choice: 0
  property int choiceCount: 0
  property string keysText: ""
  // The body's item for choice `i`, so a move can bring it into view; null shows nothing.
  property var choiceItem: function(i) { return null }
  // Hover moves the choice only on real pointer motion (design doc §4, 1n2p.15): a
  // choice's MouseArea calls hoverChoice(i, area, mouse) from onPositionChanged. The gate
  // measures the pointer against the sheet, which never scrolls, so a choice scrolled
  // under a still pointer never takes it; the keys and a new content reset it.
  readonly property var pointerGate: gate
  PointerMoveGate {
    id: gate
    referenceItem: root
  }
  property string title: ""
  property string cancelText: "Cancel"
  property string confirmText: "Confirm"
  // The confirm button in the urgent colour with its glyph (removals, undos, restores).
  property bool destructive: false
  property string confirmGlyph: ""
  // 0 is Cancel, 1 the confirm button.
  property int selectedIndex: 0

  default property alias content: body.data

  property color foreground: Commons.Color.foreground
  property color background: Commons.Color.background
  property color surface: Commons.Color.popups.background
  property color border: Commons.Color.popups.border
  property color dim: Model.dimColor(foreground, surface)
  property color urgent: Commons.Color.urgent
  property color accent: Commons.Color.accent
  property string fontFamily: Style.font.family

  signal confirmed()
  signal canceled()
  signal chosen(int index)
  signal stepped(int index, int step)

  // A list that shrinks under the cursor keeps it on its last choice.
  onChoiceCountChanged: if (choice >= choiceCount) choice = Math.max(0, choiceCount - 1)

  // The item the frame gives the keyboard to while the sheet is open.
  readonly property Item keyItem: keys
  // The width the body's content lays out in.
  readonly property real bodyWidth: body.width
  // The note under the body (how the reference closes, how a list is used) in place of
  // the buttons.
  readonly property bool noted: reference || choosing
  // The card's own height around its parts: the kit's popup padding (popupPadding, as its
  // PopupCard pads, 1n2p.39) above and below, and the column's two gaps.
  readonly property real chrome: Style.spacing.popupPadding * 2 + Style.spacing.lg * 2
  readonly property real maxBodyHeight: Math.max(Style.space(60), height * 0.8 - header.implicitHeight
    - (noted ? referenceNote.implicitHeight : footer.implicitHeight) - chrome)
  // The height the sheet would need to show its whole body without scrolling: what a
  // frame asks for while it is open (it gets at most the panel's own cap, past which the
  // body scrolls).
  readonly property real wantedHeight: (body.implicitHeight + header.implicitHeight
    + (noted ? referenceNote.implicitHeight : footer.implicitHeight) + chrome) / 0.8

  visible: opened
  onOpenedChanged: {
    reset()
    if (opened) Qt.callLater(function() { if (root.opened) keys.forceActiveFocus() })
  }

  // Back to Cancel and the top of the body: a sheet that opens, or whose content is
  // replaced while it is open (a new preview), starts there.
  function reset() {
    selectedIndex = 0
    scroller.contentY = 0
    gate.reset()
  }

  // The dialog's keys; true when the sheet took the key (it takes every key).
  function handleKey(key, text, autoRepeat, modifiers) {
    if (!opened) return false
    var name = Model.keyName(key, text, modifiers)
    if (reference) {
      var what = Model.helpKey(name)
      if (what === "close") cancel()
      else if (what === "scroll") scroll(Model.keyStep(name))
      return true
    }
    var action = Model.dialogKey(name, autoRepeat)
    if (choosing) {
      if (action === "cancel") cancel()
      else if (action === "line") moveChoice(Model.keyStep(name))
      else if (action === "press") choose(choice)
      else if (action === "switch" && choice < choiceCount) stepped(choice, Model.keyStep(name))
      return true
    }
    if (action === "cancel") cancel()
    else if (action === "line") scroll(Model.keyStep(name))
    else if (action === "switch") selectedIndex = selectedIndex === 0 ? 1 : 0
    else if (action === "confirm") confirm()
    else if (action === "press") {
      if (selectedIndex === 1) confirm()
      else cancel()
    }
    return true
  }

  // The body a line of caption text further down (`step` 1) or up (-1), within bounds.
  function scroll(step) {
    var line = Style.font.caption * 2
    var bottom = Math.max(0, scroller.contentHeight - scroller.height)
    scroller.contentY = Math.max(0, Math.min(bottom, scroller.contentY + step * line))
  }

  // The choice `step` further down (1) or up (-1), within bounds, brought into view.
  function moveChoice(step) {
    if (choiceCount === 0) return
    gate.reset()
    choice = Math.max(0, Math.min(choiceCount - 1, choice + step))
    showChoice()
  }

  // The pointer over choice `index`: the cursor follows it once the pointer itself moved.
  function hoverChoice(index, area, mouse) {
    if (!opened || index < 0 || index >= choiceCount) return
    if (gate.moved(area, mouse)) choice = index
  }

  // The body scrolled the least that shows the cursor's choice whole.
  function showChoice() {
    var item = choiceItem(choice)
    if (!item) return
    var top = item.mapToItem(body, 0, 0).y
    var bottom = Math.max(0, scroller.contentHeight - scroller.height)
    if (top < scroller.contentY) scroller.contentY = Math.max(0, top)
    else if (top + item.height > scroller.contentY + scroller.height) scroller.contentY = Math.min(bottom, top + item.height - scroller.height)
  }

  function choose(index) {
    if (!opened || index < 0 || index >= choiceCount) return
    chosen(index)
  }

  function confirm() {
    if (!opened) return
    confirmed()
  }

  function cancel() {
    if (!opened) return
    canceled()
  }

  Accessible.role: Accessible.Dialog
  Accessible.name: title

  Item {
    id: keys
    objectName: "sheetKeys"
    Keys.onPressed: function(event) {
      event.accepted = root.handleKey(event.key, event.text, event.isAutoRepeat, event.modifiers)
    }
  }

  Rectangle {
    objectName: "backdrop"
    anchors.fill: parent
    color: Qt.rgba(root.background.r, root.background.g, root.background.b, 0.7)

    MouseArea {
      anchors.fill: parent
      onClicked: root.cancel()
    }
  }

  BorderSurface {
    id: card
    objectName: "sheetCard"
    width: Math.min(Style.space(480), root.width - Style.space(8) * 2)
    height: column.implicitHeight + Style.spacing.popupPadding * 2
    anchors.centerIn: parent
    radius: Style.cornerRadius
    color: root.surface
    borderSpec: Border.flat(root.border, Math.max(1, Style.space(2)))

    // Clicks on the card stay on the card.
    MouseArea { anchors.fill: parent }

    Column {
      id: column
      x: Style.spacing.popupPadding
      y: Style.spacing.popupPadding
      width: card.width - Style.spacing.popupPadding * 2
      spacing: Style.spacing.lg

      PlainLabel {
        id: header
        objectName: "sheetTitle"
        width: parent.width
        wrapMode: Text.WordWrap
        elide: Text.ElideNone
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.subtitle
        font.bold: true
        text: root.title
      }

      Flickable {
        id: scroller
        objectName: "sheetBody"
        width: parent.width
        height: Math.min(body.implicitHeight, root.maxBodyHeight)
        contentWidth: width
        contentHeight: body.implicitHeight
        clip: true
        interactive: contentHeight > height
        boundsBehavior: Flickable.StopAtBounds
        ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

        Column {
          id: body
          width: scroller.width
          spacing: Style.spacing.sm
        }
      }

      // The key reference closes by its keys alone; letters beneath it do nothing. A list
      // says how it is used.
      PlainLabel {
        id: referenceNote
        objectName: "referenceNote"
        visible: root.noted
        width: parent.width
        wrapMode: Text.WordWrap
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: root.choosing ? root.keysText : Model.keyHint("close", "help") + " closes this · keys beneath it do nothing while it is open"
      }

      Row {
        id: footer
        visible: !root.noted
        anchors.right: parent.right
        spacing: Style.spacing.md

        // Both buttons at least the kit's control height (§4: 28 px), easy to hit with
        // a coarse pointer while the game runs.
        Button {
          objectName: "sheetCancel"
          height: Math.max(implicitHeight, Style.spacing.controlHeight)
          text: root.cancelText
          tooltipText: Model.withKey(root.cancelText, "cancel", "sheet")
          bordered: true
          hasCursor: root.selectedIndex === 0
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          Accessible.role: Accessible.Button
          Accessible.name: root.cancelText
          onClicked: root.cancel()
        }

        Button {
          id: confirmButton
          objectName: "sheetConfirm"
          height: Math.max(implicitHeight, Style.spacing.controlHeight)
          text: (root.destructive && root.confirmGlyph !== "" ? root.confirmGlyph + " " : "") + root.confirmText
          tooltipText: Model.withKey(root.confirmText, "confirm", "sheet")
          bordered: true
          hasCursor: root.selectedIndex === 1
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          Accessible.role: Accessible.Button
          Accessible.name: root.confirmText
          onClicked: root.confirm()

          // A destructive confirm: an urgent 2 px border, its label in the foreground
          // (urgent text falls below 3:1 on some themes, §5.1) beside its glyph.
          Rectangle {
            objectName: "destructiveBorder"
            visible: root.destructive
            anchors.fill: parent
            color: "transparent"
            radius: Style.cornerRadius
            border.width: Math.max(1, Style.space(2))
            border.color: root.urgent
          }
        }
      }
    }
  }
}
