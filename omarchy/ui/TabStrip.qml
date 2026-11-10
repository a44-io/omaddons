import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model

// The panel's tab strip (design doc §2.1, §5.5): one kit Button per view, drawn as
// qs.Ui ButtonGroup draws its chips (bordered, the selected one in the selected
// state), each with its key and title and its count as a pill (TabCount). A thin
// wrapper, decided: ButtonGroup has no place for a count per chip. Below 480 px
// (`narrow`) a tab is its view's glyph and count; the content's first line names the
// view. The strip decides nothing: `views` comes from the frame, `{id, title, key,
// glyph, count, urgent}` each.
Flow {
  id: root

  property var views: []
  property string value: ""
  property bool narrow: false
  // The tab under the panel's cursor, or -1.
  property int cursorIndex: -1

  property color foreground: Commons.Color.foreground
  property string fontFamily: Style.font.family
  property real fontSize: Style.font.caption

  signal changed(string value)

  spacing: Style.spacing.sm

  Accessible.role: Accessible.PageTabList

  Repeater {
    objectName: "tabs"
    model: root.views

    Button {
      id: tab
      required property var modelData
      required property int index
      objectName: "tab_" + modelData.id
      text: root.narrow ? "" : modelData.key + " " + modelData.title
      iconText: root.narrow ? String.fromCodePoint(modelData.glyph) : ""
      tooltipText: modelData.title + " (" + modelData.key + ")"
      leftAlign: true
      // Room for the pill inside the chip, at its right.
      width: implicitWidth + (pill.visible ? pill.implicitWidth + Style.spacing.sm : 0)
      selected: modelData.id === root.value
      hasCursor: root.cursorIndex === index
      bordered: true
      foreground: root.foreground
      fontFamily: root.fontFamily
      fontSize: root.fontSize
      Accessible.role: Accessible.PageTab
      Accessible.name: modelData.title + (modelData.count > 0 ? ", " + modelData.count : "")
      // The tab whose view shows, said as well as filled (1n2p.40).
      Accessible.selectable: true
      Accessible.selected: selected
      onClicked: root.changed(modelData.id)

      TabCount {
        id: pill
        anchors.right: parent.right
        anchors.rightMargin: tab.horizontalPadding
        anchors.verticalCenter: parent.verticalCenter
        count: tab.modelData.count
        urgent: tab.modelData.urgent
        foreground: root.foreground
        // The selected tab's fill (the kit's foreground at 0.18) takes the dim below
        // 4.5:1: its figures are in the foreground there (§5.1, 1n2p.40).
        dim: tab.selected ? root.foreground : Model.dimColor(root.foreground, Commons.Color.popups.background)
        fontFamily: root.fontFamily
      }
    }
  }
}
