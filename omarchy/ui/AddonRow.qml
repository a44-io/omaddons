import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model

// One row of any list (design doc §5.3): a band mark, a state glyph, the title (it elides
// first), a version transition (never elides; on a line of its own when it leaves the
// title too little), chips, a meta line, and the actions of
// the cursored row only. While a change runs on it, the spinner replaces the glyph and a
// verb replaces the meta line. It draws what the view hands it and decides nothing:
// properties in, signals out. Every string it shows goes through PlainLabel.
Item {
  id: root

  // ---- what it shows
  property string title: ""
  property string fromVersion: ""
  property string toVersion: ""
  property string meta: ""
  // A line between the title and the meta line (a search result's summary, 1n2p.25):
  // one line, elided; it drops at 400 wide (design doc §2.3), and "" draws none.
  property string detail: ""
  // Chips: [{text, urgent}] (client, source, channel, state words).
  property var chips: []
  // "urgent", "accent" or "" (no band).
  property string band: ""
  // The state glyph, a code point's text (§5.4); "" draws none.
  property string glyph: ""
  // [{name, glyph, tooltip}]: shown on the cursored row only, as icon buttons.
  property var actions: []
  property bool cursored: false
  property bool pending: false
  property string pendingText: ""
  // Another build, or a change running elsewhere: visible, inert.
  property bool inert: false
  // At 400 wide the chips but the first drop, and one action shows (§5.3).
  readonly property bool narrow: width < 480
  // The least the title keeps beside the versions (about nine characters), else its own
  // width when that is less: below it the versions take a line of their own (omad-39he).
  readonly property real titleFloor: Style.space(72)
  readonly property bool hasVersions: toVersion !== "" || fromVersion !== ""
  readonly property bool transition: fromVersion !== "" && toVersion !== ""

  // ---- theme tokens (the frame passes them; §5.1)
  property color foreground: Commons.Color.foreground
  property color background: Commons.Color.popups.background
  property color dim: Model.dimColor(foreground, background)
  property color urgent: Commons.Color.urgent
  property color accent: Commons.Color.accent
  property color muted: Commons.Color.muted
  property string fontFamily: Style.font.family

  // The frame's PointerMoveGate (PanelView.pointerGate): hover counts only real motion.
  property var pointerGate: null

  signal activated()
  signal action(string name)
  signal hovered()

  readonly property color bandColor: band === "urgent" ? urgent : band === "accent" ? accent : "transparent"
  readonly property bool titleTruncated: titleLabel.truncated
  readonly property var shownChips: narrow ? chips.slice(0, 1) : chips
  readonly property var shownActions: cursored && !inert && !pending ? (narrow ? actions.slice(0, 1) : actions) : []

  implicitWidth: 400
  implicitHeight: Math.max(Style.space(40), column.implicitHeight + Style.spacing.md * 2)
  opacity: inert ? 0.45 : 1.0

  Accessible.role: Accessible.ListItem
  Accessible.name: title + (toVersion !== "" ? ", " + Model.copy(fromVersion !== "" ? "row.versions" : "row.version",
    { "from": fromVersion, "to": toVersion }) : "") + (meta !== "" ? ", " + meta : "")

  CursorSurface {
    anchors.fill: parent
    hasCursor: root.cursored
    foreground: root.foreground
    accent: root.accent
  }

  Rectangle {
    objectName: "band"
    width: Style.space(3)
    height: parent.height
    color: root.bandColor
    visible: root.band !== ""
  }

  Text {
    id: glyphLabel
    objectName: "glyph"
    // The kit's row inset (rowPaddingX, as its own popup rows use, 1n2p.39).
    x: Style.spacing.rowPaddingX
    y: Style.spacing.md
    width: Style.font.icon + Style.spacing.sm
    visible: root.glyph !== "" || root.pending
    textFormat: Text.PlainText
    text: root.pending ? String.fromCodePoint(0xF04E6) : root.glyph
    color: root.band !== "" ? root.bandColor : root.dim
    font.family: root.fontFamily
    font.pixelSize: Style.font.icon

    RotationAnimation on rotation {
      running: root.pending && Style.duration(1200) > 0
      from: 0
      to: 360
      duration: 1200
      loops: Animation.Infinite
    }
  }

  Column {
    id: column
    x: glyphLabel.x + glyphLabel.width + Style.spacing.sm
    y: Style.spacing.md
    width: root.width - x - Style.spacing.rowPaddingX
    spacing: Style.spacing.xs

    // The first line: the title takes what the versions and chips leave. The versions
    // never elide: when beside the title they would leave it less than titleFloor (or
    // its own width, when that is less), they take a line of their own under it, where
    // a version wider than the line wraps (omad-39he). The chips stay on the title's line.
    Item {
      id: head
      // The title's room with the versions beside it.
      readonly property real inlineRoom: width - versions.natural - chipRow.width - Style.spacing.lg * 2
      readonly property bool stacked: root.hasVersions && inlineRoom < Math.min(titleLabel.implicitWidth, root.titleFloor)

      width: parent.width
      height: stacked ? titleLabel.height + Style.spacing.xs + versions.height
        : Math.max(titleLabel.implicitHeight, versions.implicitHeight)

      PlainLabel {
        id: titleLabel
        objectName: "title"
        width: Math.max(0, Math.min(implicitWidth, head.stacked
          ? head.width - chipRow.width - (chipRow.width > 0 ? Style.spacing.lg : 0) : head.inlineRoom))
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
        text: root.title
      }

      Flow {
        id: versions
        objectName: "versions"
        // Its width on one line, from its parts' own widths (never from its laid-out
        // width, which the decision to stack changes).
        readonly property real natural: !root.hasVersions ? 0
          : (root.transition ? fromLabel.implicitWidth + arrowLabel.implicitWidth + spacing * 2 : 0) + toLabel.implicitWidth

        x: head.stacked ? 0 : titleLabel.width + Style.spacing.lg
        y: head.stacked ? titleLabel.height + Style.spacing.xs : 0
        // Stacked, the line's width, which the parts wrap within; beside the title,
        // undefined resets it to the parts' one line, so nothing wraps.
        width: head.stacked ? head.width : undefined
        spacing: Style.spacing.sm
        visible: root.hasVersions

        PlainLabel {
          id: fromLabel
          objectName: "fromVersion"
          visible: root.transition
          width: head.stacked ? Math.min(implicitWidth, versions.width) : implicitWidth
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          text: root.fromVersion
          elide: Text.ElideNone
          wrapMode: head.stacked ? Text.WrapAtWordBoundaryOrAnywhere : Text.NoWrap
        }

        Text {
          id: arrowLabel
          visible: root.transition
          textFormat: Text.PlainText
          text: "→"
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
        }

        PlainLabel {
          id: toLabel
          objectName: "toVersion"
          width: head.stacked ? Math.min(implicitWidth, versions.width) : implicitWidth
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          text: root.toVersion !== "" ? root.toVersion : root.fromVersion
          elide: Text.ElideNone
          wrapMode: head.stacked ? Text.WrapAtWordBoundaryOrAnywhere : Text.NoWrap
        }
      }

      Row {
        id: chipRow
        objectName: "chips"
        anchors.right: parent.right
        spacing: Style.spacing.sm

        Repeater {
          model: root.shownChips

          Chip {
            required property var modelData
            text: String(modelData.text || "")
            urgent: modelData.urgent === true
            foreground: root.foreground
            dim: root.dim
            muted: root.muted
            urgentColor: root.urgent
            fontFamily: root.fontFamily
          }
        }
      }
    }

    PlainLabel {
      objectName: "detail"
      width: parent.width
      visible: root.detail !== "" && !root.narrow
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.bodySmall
      text: root.detail
    }

    // The second line: the meta line (or the running verb), the actions at its end.
    Item {
      width: parent.width
      height: Math.max(metaLabel.implicitHeight, actionRow.height)
      visible: metaLabel.text !== "" || root.shownActions.length > 0

      PlainLabel {
        id: metaLabel
        objectName: "meta"
        width: Math.max(0, parent.width - actionRow.width - (actionRow.width > 0 ? Style.spacing.lg : 0))
        color: root.pending ? root.foreground : root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        text: root.pending ? root.pendingText : root.meta
      }

      Row {
        id: actionRow
        objectName: "actions"
        anchors.right: parent.right
        spacing: Style.spacing.xs

        Repeater {
          model: root.shownActions

          PanelActionButton {
            required property var modelData
            objectName: "action_" + String(modelData.name || "")
            iconText: String(modelData.glyph || "")
            tooltipText: String(modelData.tooltip || "")
            foreground: root.foreground
            fontFamily: root.fontFamily
            Accessible.role: Accessible.Button
            Accessible.name: String(modelData.tooltip || modelData.name || "")
            onClicked: root.action(String(modelData.name || ""))
          }
        }
      }
    }
  }

  // The whole row is the target (§4, 1n2p.15): the pointer moving over it reports
  // `hovered` (through the frame's PointerMoveGate, so a row scrolled under a still
  // pointer reports nothing), one click reports `activated`, which the views open as
  // Enter does. Action buttons above it take their own clicks.
  MouseArea {
    id: rowArea
    anchors.fill: parent
    z: -1
    hoverEnabled: true
    enabled: !root.inert
    onPositionChanged: function(mouse) {
      if (!root.pointerGate || root.pointerGate.moved(rowArea, mouse)) root.hovered()
    }
    onClicked: root.activated()
  }
}
