import QtQuick

// Omarchy's qs.Ui ConfirmDialog: its properties and signals, the message as plain text.
Item {
  id: root

  property bool opened: false
  property string message: ""
  property string cancelText: "Cancel"
  property string confirmText: "Confirm"
  property int selectedIndex: 1
  property color background: "#101315"
  property color foreground: "#cacccc"
  property string fontFamily: "monospace"
  property int cornerRadius: 0

  signal canceled()
  signal confirmed()

  visible: opened

  Text {
    objectName: "dialogMessage"
    textFormat: Text.PlainText
    text: root.message
  }
}
