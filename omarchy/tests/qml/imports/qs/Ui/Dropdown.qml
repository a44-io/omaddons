import QtQuick

// Omarchy's qs.Ui Dropdown: its properties, `changed(value)`, and the trigger's label
// as plain text. Selecting an option is `choose(value)` (the kit's popup emits
// `changed` the same way), so a test picks one without a popup.
Item {
  id: root

  property string label: ""
  property string value: ""
  property var options: []
  property color foreground: "#cacccc"
  property color background: "#1a1b26"
  property color popupBorder: "#707880"
  property color accent: "#7aa2f7"
  property string fontFamily: "monospace"
  property int rowHeight: 24
  property int popupRowHeight: 22
  property bool showLabel: true
  property bool hasCursor: false
  property bool popupOpen: false

  signal changed(string value)
  signal hovered(bool isHovered)

  function open() { popupOpen = true }
  function close() { popupOpen = false }
  function toggle() { popupOpen = !popupOpen }
  function optionValue(o) { return (o && typeof o === "object") ? String(o.value) : String(o) }
  function optionLabel(o) { return (o && typeof o === "object") ? String(o.label) : String(o) }
  function currentLabel() {
    for (var i = 0; i < options.length; i++) if (optionValue(options[i]) === value) return optionLabel(options[i])
    return value
  }
  function choose(v) {
    popupOpen = false
    if (String(v) !== value) changed(String(v))
  }

  implicitWidth: 200
  implicitHeight: rowHeight

  Text {
    objectName: "dropdownText"
    anchors.verticalCenter: parent.verticalCenter
    textFormat: Text.PlainText
    text: root.currentLabel()
  }

  MouseArea {
    anchors.fill: parent
    onClicked: root.toggle()
  }
}
