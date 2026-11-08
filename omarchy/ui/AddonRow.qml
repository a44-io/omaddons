import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../Model.js" as Model

// One row of any list (design doc §5.3): a band mark, a state glyph, the title (it elides
// first), a version transition (never elides), chips, a meta line, and the actions of
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

  // ---- theme tokens (the frame passes them; §5.1)
  property color foreground: Commons.Color.foreground
  property color background: Commons.Color.popups.background
  property color dim: Model.dimColor(foreground, background)
  property color urgent: Commons.Color.urgent
  property color accent: Commons.Color.accent
  property color muted: Commons.Color.muted
  property string fontFamily: Style.font.family

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
  Accessible.name: title + (toVersion !== "" ? ", " + fromVersion + " to " + toVersion : "") + (meta !== "" ? ", " + meta : "")

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
    x: Style.spacing.lg
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
    width: root.width - x - Style.spacing.lg
    spacing: Style.spacing.xs

    // The first line: the title takes what the versions and chips leave.
    Item {
      width: parent.width
      height: Math.max(titleLabel.implicitHeight, versions.implicitHeight)

      PlainLabel {
        id: titleLabel
        objectName: "title"
        width: Math.max(0, Math.min(implicitWidth, parent.width - versions.width - chipRow.width - Style.spacing.lg * 2))
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
        text: root.title
      }

      Row {
        id: versions
        objectName: "versions"
        x: titleLabel.width + Style.spacing.lg
        spacing: Style.spacing.sm
        visible: root.toVersion !== "" || root.fromVersion !== ""

        PlainLabel {
          visible: root.fromVersion !== "" && root.toVersion !== ""
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          text: root.fromVersion
          elide: Text.ElideNone
        }

        Text {
          visible: root.fromVersion !== "" && root.toVersion !== ""
          textFormat: Text.PlainText
          text: "→"
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
        }

        PlainLabel {
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.body
          text: root.toVersion !== "" ? root.toVersion : root.fromVersion
          elide: Text.ElideNone
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

  // The whole row is the target (§4): hover reports, a click opens. Action buttons
  // above it take their own clicks.
  MouseArea {
    anchors.fill: parent
    z: -1
    hoverEnabled: true
    enabled: !root.inert
    onEntered: root.hovered()
    onClicked: root.activated()
  }
}
