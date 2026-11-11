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
// Folders omaddons does not manage are not listed here: Issues suggests taking over
// those a take-over would adopt (omad-3oru).
// Last, the share line (1n2p.37): Copy addon list (`y`) puts the chosen client's set, or
// every client's, on the clipboard as `export` prints it; Paste addon list… (Ctrl+V)
// previews a list someone shared as `sync -`, and the plan sheet says what it would
// change before anything does. It shows with no addons yet too: a new computer starts
// from a shared list.
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
  readonly property int rowCount: rows.length
  // Past LONG_LIST_ROWS the rows are a VirtualList that builds only those in view and
  // scrolls itself (1n2p.38): the frame asks it to position the cursor's row.
  readonly property bool scrollsItself: rows.length > Model.LONG_LIST_ROWS
  // The filter box has the keyboard: the frame's key catcher lets it type.
  readonly property bool typing: filterField.activeFocus
  readonly property bool filterShown: query !== "" || filterField.activeFocus
  // The share line, once the list was read: a copy needs a managed addon in the scope.
  readonly property bool canShare: !!svc && !!svc.lists
  readonly property bool hasAddons: canShare && Model.installedRows(svc.lists, filter).length > 0
  // What the last copy put on the clipboard, said for a moment (the copy flash).
  property string copied: ""

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
    // Enter: the addon's page (1n2p.23), with every action on each client.
    if (name === "enter") {
      if (row) frame.openAddon(row.name, row.ref)
      return true
    }
    // The cursored row's Repair (a problem's): its files put back, previewed first.
    if (name === "repair") {
      if (row && row.problem) run(Model.rowAction(row, "reinstall"))
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
    if (name === "copy") {
      copyList()
      return true
    }
    if (name === "paste") {
      pasteList()
      return true
    }
    return false
  }

  // `y`: the scope's addon list on the clipboard. The flash says what went on it once
  // wl-copy took it; why nothing did is the panel's notice.
  function copyList() {
    if (!canShare) return false
    return svc.copyAddonList(filter, function(ok, line) {
      if (ok) {
        root.copied = line
        copiedTimer.restart()
        if (root.frame) root.frame.report("")
      } else if (root.frame) root.frame.report(line)
    })
  }

  // Ctrl+V: a shared list from the clipboard, previewed on the scope; the plan sheet asks
  // before anything changes. Why it ran nothing is the panel's notice.
  function pasteList() {
    if (!canShare || !canChange) return false
    return svc.pasteAddonList(filter, function(refusal) { if (root.frame) root.frame.report(refusal) })
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
        Accessible.role: Accessible.EditableText
        Accessible.name: placeholderText
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
        // "0 of 42" stands out in bold foreground, never urgent text (§5.1, 1n2p.40).
        color: root.count.none ? root.foreground : root.dim
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
        ? Math.max(Style.space(200), (root.frame ? root.frame.contentRoom - root.frame.viewTop : Style.space(480))
          - y - shareLine.height - column.spacing * 2)
        : 0
      spacing: column.spacing
      model: root.scrollsItself ? root.rows : []
      delegate: rowDelegate
      cursor: root.frame && root.frame.cursorActive ? root.frame.cursor : -1
    }

    // Sharing (1n2p.37): the line in words, its two buttons with their keys, and what the
    // last copy put on the clipboard, for a moment.
    Column {
      id: shareLine
      objectName: "shareLine"
      visible: root.canShare
      width: column.width
      height: visible ? implicitHeight : 0
      spacing: Style.spacing.xs

      PlainLabel {
        objectName: "shareText"
        width: parent.width
        wrapMode: Text.WordWrap
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: Model.copy("share.line")
      }

      // The buttons wrap at a narrow panel or a large text zoom, never past its edge.
      Flow {
        width: parent.width
        spacing: Style.spacing.sm

        Button {
          objectName: "copyList"
          width: Math.min(implicitWidth, shareLine.width)
          // The key is the tooltip's, never the label's (omad-xdwy).
          text: Model.copy("share.copy")
          tooltipText: Model.keyTip("copy", "installed")
          Accessible.role: Accessible.Button
          Accessible.name: text
          enabled: root.hasAddons
          opacity: enabled ? 1.0 : 0.4
          bordered: true
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.caption
          onClicked: root.copyList()
        }

        Button {
          objectName: "pasteList"
          width: Math.min(implicitWidth, shareLine.width)
          text: Model.copy("share.paste")
          tooltipText: Model.keyTip("paste", "installed")
          Accessible.role: Accessible.Button
          Accessible.name: text
          enabled: root.canChange
          opacity: enabled ? 1.0 : 0.4
          bordered: true
          foreground: root.foreground
          fontFamily: root.fontFamily
          fontSize: Style.font.caption
          onClicked: root.pasteList()
        }
      }

      // The copy flash (design doc §5.5): what went on the clipboard.
      PlainLabel {
        objectName: "copyFlash"
        visible: root.copied !== ""
        width: parent.width
        // Accent is never text's colour (§5.1, 1n2p.40).
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: root.copied
      }
    }
  }

  Timer {
    id: copiedTimer
    interval: 1500
    onTriggered: root.copied = ""
  }

  // One row, as both lists build it: under All, its client's header above the client's
  // first row; the AddonRow, a dependency indented under what needs it.
  Component {
    id: rowDelegate

      Column {
        id: entry
        required property var modelData
        required property int index
        objectName: "installedRow_" + index
        width: column.width
        spacing: Style.spacing.sm
        // While a change runs on it, its phase; just after, what happened (1n2p.18).
        readonly property var rowState: Model.activityRow(root.svc ? root.svc.activity : null, modelData.key,
          root.svc ? root.svc.doneRows : null)

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
          meta: entry.rowState && !entry.rowState.pending ? entry.rowState.text : entry.modelData.meta
          pending: !!entry.rowState && entry.rowState.pending
          pendingText: entry.rowState ? entry.rowState.text : ""
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
          pointerGate: root.frame ? root.frame.pointerGate : null
          onHovered: root.select(entry.index)
          // One click opens the addon's page, as Enter does (§4, 1n2p.15).
          onActivated: {
            root.select(entry.index)
            root.handleKey("enter", entry.index)
          }
          onAction: function(name) {
            root.select(entry.index)
            root.handleKey(name, entry.index)
          }
        }
      }
  }
}
