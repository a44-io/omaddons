import QtQuick
import qs.Commons
import qs.Commons as Commons
import ".."
import "../Model.js" as Model

// The plan sheet (design doc §6.7, 1n2p.17): every confirmation the panel asks for, an
// update, install, repair of files, removal, undo, restore, backup deletion, forgotten
// client or doctor repair, on the shared Sheet. Model builds what it says (the `sheet` of
// Model.previewDecision, Model.confirmSheet or Model.doctorPreviewDecision): a title that
// names the change, the "Changed since you looked" banner over a new preview, every item
// (each client's under its name when it spans several, never cut: the body scrolls; a
// long addon name elides, its versions never do), one line per effect with its glyph
// (the costly ones first, their glyph in the urgent colour), and Cancel beside the verb
// ("Update 3", "Remove DBM"), urgent for a destructive plan. It opens on Cancel, and the
// verb takes only a fresh press (the Sheet's keys, M3-D46). Every string is the model's,
// shown through PlainLabel.
Sheet {
  id: root

  // The sheet Model built, or null.
  property var plan: null

  readonly property var items: plan && Array.isArray(plan.items) ? plan.items : []
  readonly property var effects: plan && Array.isArray(plan.effects) ? plan.effects : []
  readonly property string stale: plan && typeof plan.stale === "string" ? plan.stale : ""

  title: plan ? String(plan.title || "") : ""
  cancelText: plan && plan.cancelLabel ? String(plan.cancelLabel) : Model.copy("sheet.cancel")
  confirmText: plan ? String(plan.confirmLabel || "") : ""
  destructive: !!plan && plan.destructive === true
  confirmGlyph: plan && plan.glyph ? String.fromCodePoint(plan.glyph) : ""

  // The banner over a preview made again after its plan was refused, or one that differs
  // from what the view showed.
  NoticeCard {
    objectName: "planStale"
    visible: root.stale !== ""
    width: root.bodyWidth
    kind: "info"
    title: root.stale
    foreground: root.foreground
    fontFamily: root.fontFamily
  }

  Repeater {
    objectName: "planItems"
    model: root.items

    Column {
      id: entry
      objectName: "planItem"
      required property var modelData
      width: root.bodyWidth
      spacing: Style.spacing.xs

      SectionHeader {
        objectName: "planGroup"
        visible: entry.modelData.header !== ""
        width: parent.width
        title: entry.modelData.header
        foreground: root.foreground
        fontFamily: root.fontFamily
      }

      // The name elides when the row is short; the detail (versions, counts) never does.
      // A line with no detail is a sentence of its own, and wraps.
      Item {
        width: parent.width
        height: Math.max(nameLabel.implicitHeight, detailLabel.implicitHeight)

        PlainLabel {
          id: nameLabel
          objectName: "planItemName"
          width: detailLabel.text === "" ? parent.width
            : Math.max(0, Math.min(implicitWidth, parent.width - detailLabel.implicitWidth - Style.spacing.sm))
          wrapMode: detailLabel.text === "" ? Text.WordWrap : Text.NoWrap
          elide: detailLabel.text === "" ? Text.ElideNone : Text.ElideRight
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          text: entry.modelData.name
        }

        PlainLabel {
          id: detailLabel
          objectName: "planItemDetail"
          x: nameLabel.width + Style.spacing.sm
          width: implicitWidth
          elide: Text.ElideNone
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.bodySmall
          text: entry.modelData.detail
        }
      }
    }
  }

  Repeater {
    objectName: "planEffects"
    model: root.effects

    Item {
      id: effect
      objectName: "planEffect"
      required property var modelData
      readonly property bool urgent: modelData.severity === "urgent"
      width: root.bodyWidth
      height: Math.max(glyphLabel.implicitHeight, effectText.implicitHeight)

      PlainLabel {
        id: glyphLabel
        objectName: "planEffectGlyph"
        width: Style.space(20)
        elide: Text.ElideNone
        color: effect.urgent ? root.urgent : root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        text: String.fromCodePoint(effect.modelData.glyph)
      }

      // Its label stays in the foreground (urgent text falls below 3:1 on some themes,
      // §5.1): the glyph's colour, its weight and its place first say it is urgent.
      PlainLabel {
        id: effectText
        objectName: "planEffectText"
        x: glyphLabel.width + Style.spacing.sm
        width: parent.width - x
        wrapMode: Text.WordWrap
        elide: Text.ElideNone
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.bodySmall
        font.bold: effect.urgent
        text: effect.modelData.text
      }
    }
  }
}
