import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// The Addons view (1n2p.22, design doc §2.3): every managed addon per client, from
// `list -v` (Model.installedRows). Each row is an AddonRow: its state as a glyph, a band
// and chips in words (files changed on disk, folder gone, update ready, pinned,
// dependency, not needed), its source and what needs it on the meta line. Problems come
// first, then waiting updates, then the rest; a dependency sits under the addon that
// needs it. `/` filters the list (each word a subsequence of the name, the source or a
// state word), with "2 of 42" beside the box; `S` sorts by name, by recently updated or
// by source; the cursor stays on the same addon through both. Enter repairs a problem's
// files or updates when a release waits (the addon page, .23, will open instead); `u`
// updates; `p` pins or unpins; `x` removes, through the plan sheet; `r` restores its
// settings (the restore chooser). The keys are Model.KEYMAP's installed and filter rows.
// Folders omaddons does not manage are one line per client, in words; their Take over…
// leads to the take-over flow, which the first-run bead (.32) registers.
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var statusTargets: svc && Array.isArray(svc.targets) ? svc.targets : []
  // The filter's text and the order, in memory only (M3-D3).
  property string query: ""
  property string sort: "name"
  readonly property var rows: Model.installedRows(svc ? svc.lists : null, filter, statusTargets, { "query": query, "sort": sort })
  readonly property var count: Model.installedCount(svc ? svc.lists : null, filter, query)
  // What the view says around its rows (design doc §7): loading, a read that failed, no
  // addons yet (and where to find some). A filter that matches nothing is no empty
  // view: its count says "0 of 42".
  readonly property var viewState: Model.viewState("installed", { "service": svc, "filter": filter,
    "rows": query.trim() !== "" ? count.total : rows.length,
    "allRows": filter ? Model.installedRows(svc ? svc.lists : null, "").length : rows.length, "nowMs": frame ? frame.nowMs : Date.now() })
  readonly property var unmanaged: Model.unmanagedRows(svc ? svc.lists : null, filter, statusTargets)
  readonly property int rowCount: rows.length
  // The filter box has the keyboard: the frame's key catcher lets it type.
  readonly property bool typing: filterField.activeFocus
  readonly property bool filterShown: query !== "" || filterField.activeFocus
  // The take-over flow, once the first-run bead registers it (`frame.keyHandlers.adopt`).
  readonly property bool canTakeOver: !!frame && !!frame.keyHandlers && typeof frame.keyHandlers["adopt"] === "function"

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property color accent: frame ? frame.accent : Commons.Color.accent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) { return rowRepeater.itemAt(i) }

  function handleKey(name, cursor) {
    var row = cursor >= 0 && cursor < rows.length ? rows[cursor] : null
    if (name === "enter") {
      if (row && row.primary !== "") run(Model.rowAction(row, row.primary))
      return true
    }
    // `u`: its update, also when its files changed (Enter puts those back first).
    if (name === "update") {
      if (row && row.updateAvailable) run(Model.rowAction(row, "update"))
      return true
    }
    if (name === "remove") {
      if (row) run(Model.rowAction(row, "remove"))
      return true
    }
    if (name === "pin") {
      if (row) run(Model.rowAction(row, "pin"))
      return true
    }
    // `r`: this addon's settings from a backup (the restore chooser).
    if (name === "restore") {
      if (row) frame.openChooser({ "target": row.target, "addon": row.name })
      return true
    }
    if (name === "filter") {
      focusFilter()
      return true
    }
    if (name === "sort") {
      keepCursor(function() { root.sort = Model.nextSort(root.sort) })
      return true
    }
    return false
  }

  function run(action) {
    if (!canChange || !action) return false
    return frame.report(svc.run(action.template, action.slots, action.label, action.shown))
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  // A change of the filter or the order, the cursor kept on the same addon (by its
  // key, never its index); on the first row when that addon is no longer shown.
  function keepCursor(change) {
    var before = frame && frame.cursorActive && frame.cursor >= 0 && frame.cursor < rows.length ? rows[frame.cursor].key : ""
    change()
    if (!frame) return
    var at = -1
    for (var i = 0; before !== "" && i < rows.length; i++) if (rows[i].key === before) at = i
    frame.cursor = Math.max(0, at)
    if (at < 0) frame.cursorActive = false
  }

  function focusFilter() {
    filterField.forceActiveFocus()
  }

  // Esc where the view has the keys (design doc §2.4): a filter's text goes, the cursor
  // stays on its addon; false when there is none, so Esc closes the panel.
  function escapeFilter() {
    if (query === "") return false
    keepCursor(function() {
      filterField.text = ""
      root.query = ""
    })
    return true
  }

  // The filter box's keys (KEYMAP's filter rows): Down and Up move the list's cursor,
  // Enter goes to the first match and leaves the box, Esc leaves it with the filter
  // kept; every other key is typed.
  function filterKey(code, text, modifiers) {
    var name = Model.keyName(code, text, modifiers)
    var row = Model.keyRow(name, "filter")
    if (!row || !frame) return false
    if (row.action === "move") frame.moveCursor(frame.cursorActive ? frame.cursor + Model.keyStep(name) : 0)
    else if (row.action === "first") {
      if (rows.length > 0) select(0)
      frame.leaveField()
    } else if (row.action === "leave") frame.leaveField()
    return true
  }

  // A client's Take over…: the flow the first-run bead registers; nothing before it.
  function takeOver(target) {
    if (!canTakeOver || !canChange) return false
    frame.keyHandlers["adopt"](frame, target)
    return true
  }

  Column {
    id: column
    width: root.width
    spacing: Style.spacing.sm

    // `/`: the filter, "2 of 42" beside it (urgent at none), and the order when it is
    // not by name.
    Item {
      objectName: "filterRow"
      visible: root.filterShown || root.sort !== "name"
      width: parent.width
      height: visible ? Math.max(filterField.visible ? filterField.implicitHeight : 0, sortLabel.implicitHeight) : 0

      TextField {
        id: filterField
        objectName: "filterField"
        // Hidden until `/` (an Item that is not visible cannot take focus: it is kept
        // in the tree at no width instead).
        width: root.filterShown ? parent.width - countLabel.width - sortLabel.width - Style.spacing.lg * 2 : 0
        opacity: root.filterShown ? 1.0 : 0.0
        placeholderText: Model.copy("installed.filter")
        foreground: root.foreground
        onTextChanged: if (text !== root.query) root.keepCursor(function() { root.query = filterField.text })
        Keys.onPressed: function(event) { event.accepted = root.filterKey(event.key, event.text, event.modifiers) }
      }

      PlainLabel {
        id: countLabel
        objectName: "filterCount"
        visible: root.filterShown
        anchors.right: sortLabel.left
        anchors.rightMargin: sortLabel.visible ? Style.spacing.lg : 0
        anchors.verticalCenter: parent.verticalCenter
        width: visible ? implicitWidth : 0
        elide: Text.ElideNone
        color: root.count.none ? root.urgent : root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        font.bold: root.count.none
        text: root.count.text
      }

      PlainLabel {
        id: sortLabel
        objectName: "sortLabel"
        visible: root.sort !== "name"
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        width: visible ? implicitWidth : 0
        elide: Text.ElideNone
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: Model.copy("installed.sort." + root.sort)
      }
    }

    ViewState {
      objectName: "viewState"
      width: parent.width
      viewState: root.viewState
      foreground: root.foreground
      fontFamily: root.fontFamily
      onAction: function(name) { if (root.frame) root.frame.stateAction(name) }
    }

    Repeater {
      id: rowRepeater
      model: root.rows

      Column {
        id: entry
        required property var modelData
        required property int index
        objectName: "installedRow_" + index
        width: column.width
        spacing: Style.spacing.sm

        // Under All, the client's header above its first row: its player name, whether
        // it runs, its addons (design doc §2.2). The rows under it name no client.
        SectionHeader {
          objectName: "clientHeader"
          visible: entry.modelData.group !== ""
          width: parent.width
          title: entry.modelData.group
          count: entry.modelData.groupCount
          foreground: root.foreground
          dim: root.dim
          fontFamily: root.fontFamily
        }

        // A dependency sits under the addon that needs it, indented.
        AddonRow {
          id: addonRow
          objectName: "addonRow"
          x: entry.modelData.depth > 0 ? Style.spacing.xl : 0
          width: parent.width - x
          title: entry.modelData.name
          fromVersion: entry.modelData.version || "?"
          toVersion: entry.modelData.updateAvailable && entry.modelData.latest ? entry.modelData.latest : ""
          chips: entry.modelData.chips
          meta: entry.modelData.meta
          band: entry.modelData.band
          glyph: entry.modelData.glyph ? String.fromCodePoint(entry.modelData.glyph) : ""
          actions: entry.modelData.actions
          cursored: !!root.frame && root.frame.cursorActive && root.frame.cursor === entry.index
          foreground: root.foreground
          dim: root.dim
          urgent: root.urgent
          accent: root.accent
          fontFamily: root.fontFamily
          // A click puts the cursor on it; its buttons act (design doc §4 waits for the
          // addon page, .23, before a click opens).
          onActivated: root.select(entry.index)
          onAction: function(name) {
            root.select(entry.index)
            root.handleKey(name, entry.index)
          }
        }
      }
    }

    // The folders omaddons does not manage, in words, never a command line (UX-13).
    Repeater {
      objectName: "unmanaged"
      model: root.unmanaged

      Item {
        required property var modelData
        objectName: "unmanaged_" + modelData.target
        width: column.width
        height: Math.max(unmanagedText.implicitHeight, takeOverButton.visible ? takeOverButton.implicitHeight : 0)

        PlainLabel {
          id: unmanagedText
          objectName: "unmanagedText"
          anchors.verticalCenter: parent.verticalCenter
          width: parent.width - (takeOverButton.visible ? takeOverButton.width + Style.spacing.lg : 0)
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: modelData.text
        }

        Button {
          id: takeOverButton
          objectName: "takeOver"
          visible: root.canTakeOver
          anchors.right: parent.right
          anchors.verticalCenter: parent.verticalCenter
          text: Model.copy("installed.take_over")
          enabled: root.canChange
          opacity: enabled ? 1.0 : 0.4
          bordered: true
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.caption
          onClicked: root.takeOver(modelData.target)
        }
      }
    }
  }
}
