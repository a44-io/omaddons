import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// The History view (1n2p.30, design doc §2.3): every change omaddons recorded, newest
// first and under its day, from `history -v` (Model.historyRows). Each row is one run
// (the transactions of one command): what it did in words ("Updated 3 addons"), its
// clients as chips under All (§2.2: History interleaves them), the addons it changed,
// and when, by whom and why. Ctrl+Z undoes the cursored change (`undo --run`, previewed
// in the plan sheet first; a change already undone says so, the undo of an undo is a
// Redo); Enter opens its page (ChangePage), with each client's addons and an undo per
// client. `/` filters by what it did, the addons, the clients, who or why. The keys are
// Model.KEYMAP's history and filter rows.
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var statusTargets: svc && Array.isArray(svc.targets) ? svc.targets : []
  readonly property double nowMs: frame ? frame.nowMs : Date.now()
  // The filter's text, in memory only (M3-D3).
  property string query: ""
  readonly property var rows: Model.historyRows(svc ? svc.history : null, filter, statusTargets, { "query": query, "nowMs": nowMs })
  readonly property var count: Model.historyCount(svc ? svc.history : null, filter, statusTargets, query, nowMs)
  // What the view says around its rows (design doc §7): reading, a read that failed, no
  // changes yet. A filter that matches nothing is no empty view: its count says "0 of 42".
  readonly property var viewState: Model.viewState("history", { "service": svc, "filter": filter,
    "rows": query.trim() !== "" ? count.total : rows.length,
    "allRows": filter ? Model.historyRows(svc ? svc.history : null, "", statusTargets, { "nowMs": nowMs }).length : rows.length,
    "nowMs": nowMs })
  readonly property int rowCount: rows.length
  // Past LONG_LIST_ROWS the rows are a VirtualList that builds only those in view and
  // scrolls itself (1n2p.38): the frame asks it to position the cursor's row.
  readonly property bool scrollsItself: rows.length > Model.LONG_LIST_ROWS
  readonly property bool typing: filterField.activeFocus
  readonly property bool filterShown: query !== "" || filterField.activeFocus

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property color accent: frame ? frame.accent : Commons.Color.accent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) { return scrollsItself ? rowList.rowItem(i) : rowRepeater.itemAt(i) }

  function positionCursor() { rowList.positionCursor() }

  function handleKey(name, cursor) {
    var row = cursor >= 0 && cursor < rows.length ? rows[cursor] : null
    if (name === "enter") {
      if (row) frame.openChange(row.key, row.title)
      return true
    }
    // Ctrl+Z: the cursored change (the frame asks this view first); with no row under
    // the cursor, the frame's own undo of the last change.
    if (name === "undo") {
      if (!row || !frame.cursorActive) return false
      undo(row)
      return true
    }
    if (name === "filter") {
      focusFilter()
      return true
    }
    return false
  }

  // A change's undo, previewed first (the plan sheet names it); one that cannot be
  // undone says why, never a guess (the CLI's preview says what the rows cannot know).
  function undo(row) {
    if (!row.undo) {
      frame.report(row.why || Model.copy("history.why.undone"))
      return
    }
    if (!canChange) return
    frame.report(svc.run(row.undo.template, row.undo.slots, row.undo.label))
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  // A change of the filter, the cursor kept on the same change (by its key, never its
  // index); on the first row when that change is no longer shown.
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

  // Esc where the view has the keys (design doc §2.4): a filter's text goes; false when
  // there is none, so Esc closes the panel.
  function escapeFilter() {
    if (query === "") return false
    keepCursor(function() {
      filterField.text = ""
      root.query = ""
    })
    return true
  }

  // The filter box's keys (KEYMAP's filter rows), as the Addons view's.
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

  Column {
    id: column
    width: root.width
    spacing: Style.spacing.sm

    // `/`: the filter, "2 of 42" beside it (urgent at none).
    Item {
      objectName: "filterRow"
      visible: root.filterShown
      width: parent.width
      height: visible ? filterField.implicitHeight : 0

      TextField {
        id: filterField
        objectName: "filterField"
        // Hidden until `/` (an Item that is not visible cannot take focus: it is kept
        // in the tree at no width instead).
        width: root.filterShown ? parent.width - countLabel.width - Style.spacing.lg : 0
        opacity: root.filterShown ? 1.0 : 0.0
        placeholderText: Model.copy("history.filter")
        foreground: root.foreground
        onTextChanged: if (text !== root.query) root.keepCursor(function() { root.query = filterField.text })
        Keys.onPressed: function(event) { event.accepted = root.filterKey(event.key, event.text, event.modifiers) }
      }

      PlainLabel {
        id: countLabel
        objectName: "filterCount"
        visible: root.filterShown
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        width: visible ? implicitWidth : 0
        elide: Text.ElideNone
        color: root.count.none ? root.urgent : root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        font.bold: root.count.none
        text: root.count.text
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

    // Short lists: every row built, in the content the frame scrolls.
    Repeater {
      id: rowRepeater
      model: root.scrollsItself ? [] : root.rows
      delegate: rowDelegate
    }

    // Long lists: only the rows in view, in a list that fills the room left and scrolls
    // itself (the frame's content then fits, 1n2p.38).
    VirtualList {
      id: rowList
      objectName: "rowList"
      visible: root.scrollsItself
      width: column.width
      height: root.scrollsItself
        ? Math.max(Style.space(200), (root.frame ? root.frame.contentRoom - root.frame.viewTop : Style.space(480)) - y - column.spacing)
        : 0
      spacing: column.spacing
      model: root.scrollsItself ? root.rows : []
      delegate: rowDelegate
      cursor: root.frame && root.frame.cursorActive ? root.frame.cursor : -1
    }
  }

  // One change, as both lists build it: its day's heading above the day's first change.
  Component {
    id: rowDelegate

      Column {
        id: entry
        required property var modelData
        required property int index
        objectName: "historyRow_" + index
        width: column.width
        spacing: Style.spacing.sm

        // The day's heading above its first change (design doc §6.9: "Today",
        // "Yesterday", a weekday, a date).
        SectionHeader {
          objectName: "dayHeader"
          visible: entry.modelData.group !== ""
          width: parent.width
          title: entry.modelData.group
          count: ""
          foreground: root.foreground
          dim: root.dim
          fontFamily: root.fontFamily
        }

        AddonRow {
          objectName: "changeRow"
          width: parent.width
          title: entry.modelData.title
          chips: entry.modelData.chips
          detail: entry.modelData.detail
          meta: entry.modelData.meta
          band: entry.modelData.band
          glyph: String.fromCodePoint(entry.modelData.glyph)
          actions: entry.modelData.actions
          cursored: !!root.frame && root.frame.cursorActive && root.frame.cursor === entry.index
          // A change undone, or one undo never reverts, reads dimmer than one in effect.
          foreground: entry.modelData.state === "undoable" || entry.modelData.state === "redo" ? root.foreground : root.dim
          dim: root.dim
          urgent: root.urgent
          accent: root.accent
          fontFamily: root.fontFamily
          onActivated: root.select(entry.index)
          onAction: function(name) {
            root.select(entry.index)
            root.handleKey(name, entry.index)
          }
        }
      }
  }
}
