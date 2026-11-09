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
// n cancel; h, l, ←, →, Tab and Shift+Tab move between the buttons; y confirms, and
// Enter or Space press the focused button. A key held from before the sheet opened
// reaches it only as auto-repeat, which never confirms (M3-D46). Every other key is the
// sheet's: nothing beneath it acts.
//
// With `reference`, it is the key reference (`?`, design doc §2.4's help overlay): no
// buttons, a line saying how it closes, and KEYMAP's help rows (`?` and Esc close it,
// j/k scroll it, every other key is inert). It opens at the top every time.
Item {
  id: root

  property bool opened: false
  property bool reference: false
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

  // The item the frame gives the keyboard to while the sheet is open.
  readonly property Item keyItem: keys
  // The width the body's content lays out in.
  readonly property real bodyWidth: body.width
  readonly property real maxBodyHeight: Math.max(Style.space(60), height * 0.8 - header.implicitHeight
    - (reference ? referenceNote.implicitHeight : footer.implicitHeight) - Style.spacing.xl * 4)

  visible: opened
  onOpenedChanged: {
    selectedIndex = 0
    scroller.contentY = 0
    if (opened) Qt.callLater(function() { if (root.opened) keys.forceActiveFocus() })
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
    if (action === "cancel") cancel()
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
    height: column.implicitHeight + Style.spacing.xl * 2
    anchors.centerIn: parent
    radius: Style.cornerRadius
    color: root.surface
    borderSpec: Border.flat(root.border, Math.max(1, Style.space(2)))

    // Clicks on the card stay on the card.
    MouseArea { anchors.fill: parent }

    Column {
      id: column
      x: Style.spacing.xl
      y: Style.spacing.xl
      width: card.width - Style.spacing.xl * 2
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

      // The key reference closes by its keys alone; letters beneath it do nothing.
      PlainLabel {
        id: referenceNote
        objectName: "referenceNote"
        visible: root.reference
        width: parent.width
        wrapMode: Text.WordWrap
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: Model.keyHint("close", "help") + " closes this · keys beneath it do nothing while it is open"
      }

      Row {
        id: footer
        visible: !root.reference
        anchors.right: parent.right
        spacing: Style.spacing.md

        Button {
          objectName: "sheetCancel"
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
