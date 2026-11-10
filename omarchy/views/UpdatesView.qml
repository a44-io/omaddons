import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// The Updates view (1n2p.21, design doc §2.3): what is behind on each client, from
// `list -v` (Model.updateRows). Each row is an AddonRow: its name, the installed and the
// new version, a chip for a channel other than stable, and a meta line in words (how old
// the release is, the game running, why it is pinned, blocked or not checked). Updates
// come first, then pinned addons with a newer release, blocked releases and failed
// checks, by name within each; under All each client's header names it and updates it.
// The header updates everything in the filter (`U`) and says when it was checked, with
// Check now (`c`). `e` shows every release note since the installed version (a network
// read, on demand, kept only for the row that asked); `/` filters the rows. Enter and
// `u` update the row's addon, `p` pins or unpins it. A row a change runs on shows its
// phase, and says "Updated" a moment after (the activity bar, 1n2p.18). Updates preview
// first and apply a routine plan that matches the rows (M4-D28); pins run directly. The
// keys are Model.KEYMAP's updates rows.
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var statusTargets: svc && Array.isArray(svc.targets) ? svc.targets : []
  readonly property double nowMs: frame ? frame.nowMs : Date.now()
  // The filter's text, in memory only (M3-D3).
  property string query: ""
  readonly property var done: svc && svc.doneRows ? svc.doneRows : null
  readonly property var rows: Model.updateRows(svc ? svc.lists : null, filter, statusTargets,
    { "nowMs": nowMs, "done": done, "query": query })
  // Every row of the scope, whatever the filter says: what Update all updates, and the
  // filter's "2 of 5".
  readonly property var scopeRows: query === "" ? rows
    : Model.updateRows(svc ? svc.lists : null, filter, statusTargets, { "nowMs": nowMs, "done": done })
  // What the view says around its rows (design doc §7): loading, empty, a check whose
  // sources did not answer, a stale check. A filter that matches nothing is no empty
  // view: its count says "0 of 5".
  readonly property var viewState: Model.viewState("updates", { "service": svc, "filter": filter, "rows": scopeRows.length,
    "allRows": filter ? Model.updateRows(svc ? svc.lists : null, "", statusTargets, { "nowMs": nowMs, "done": done }).length
      : scopeRows.length,
    "nowMs": nowMs })
  readonly property int count: Model.updateCount(scopeRows)
  readonly property int rowCount: rows.length
  // Past LONG_LIST_ROWS the rows are a VirtualList that builds only those in view and
  // scrolls itself (1n2p.38): the frame asks it to position the cursor's row.
  readonly property bool scrollsItself: rows.length > Model.LONG_LIST_ROWS
  // The filter box has the keyboard: the frame's key catcher lets it type.
  readonly property bool typing: filterField.activeFocus
  readonly property bool filterShown: query !== "" || filterField.activeFocus
  // The row whose release notes are open (`e`), by its key.
  property string expanded: ""

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
    // Enter: the addon's page (1n2p.23); `u` updates.
    if (name === "enter") {
      if (row) frame.openAddon(row.name, row.ref)
      return true
    }
    if (name === "update") {
      if (row && row.kind === "update") run(Model.rowAction(row, "update"))
      return true
    }
    if (name === "pin") {
      if (row && row.kind !== "failed" && row.kind !== "done") run(Model.rowAction(row, "pin"))
      return true
    }
    if (name === "notes") {
      if (row && row.kind !== "failed" && row.kind !== "done") toggleNotes(row)
      return true
    }
    // A failed check's Check again.
    if (name === "check") {
      if (frame) frame.checkNow()
      return true
    }
    if (name === "filter") {
      focusFilter()
      return true
    }
    return false
  }

  function run(action) {
    if (!canChange || !action) return false
    return frame.report(svc.run(action.template, action.slots, action.label, action.shown))
  }

  // A client header's Update: that client's updates, as one preview.
  function updateClient(target) {
    return run(Model.groupUpdateRequest(target, scopeRows, statusTargets))
  }

  function toggleNotes(row) {
    if (expanded === row.key) {
      expanded = ""
      return
    }
    expanded = row.key
    svc.readChangelog(row.target, row.name)
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  // A change of the filter, the cursor kept on the same addon (by its key); on the first
  // row when it is no longer shown.
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

    // The header: update everything in the filter (U), and how fresh the check is, with
    // Check now (c).
    Item {
      objectName: "updatesHeader"
      width: parent.width
      height: Math.max(updateAllButton.visible ? updateAllButton.implicitHeight : 0, checkNowButton.implicitHeight)

      Button {
        id: updateAllButton
        objectName: "updateAll"
        visible: root.count > 0
        anchors.left: parent.left
        anchors.verticalCenter: parent.verticalCenter
        text: (root.filter ? "Update " + Model.heroTitle(root.statusTargets, root.filter) : "Update all") + " (" + root.count + ")"
        tooltipText: Model.keyTip("update_all", "updates")
        Accessible.role: Accessible.Button
        Accessible.name: text
        enabled: root.canChange
        opacity: enabled ? 1.0 : 0.4
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.frame.updateAll()
      }

      PlainLabel {
        objectName: "checkedLabel"
        anchors.right: checkNowButton.left
        anchors.rightMargin: Style.spacing.md
        anchors.verticalCenter: parent.verticalCenter
        width: Math.min(implicitWidth, parent.width - checkNowButton.width - Style.spacing.md
          - (updateAllButton.visible ? updateAllButton.width + Style.spacing.md : 0))
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: Model.updatesChecked(root.statusTargets, root.filter, root.nowMs)
      }

      Button {
        id: checkNowButton
        objectName: "checkNow"
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        text: Model.copy("updates.check_now")
        tooltipText: Model.keyTip("check", "updates")
        Accessible.role: Accessible.Button
        Accessible.name: text
        enabled: !!root.frame && root.frame.canAct && !(root.svc && root.svc.checking === true)
        opacity: enabled ? 1.0 : 0.4
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.caption
        onClicked: root.frame.checkNow()
      }
    }

    // `/`: the filter, "2 of 5" beside it (urgent at none).
    Item {
      objectName: "filterRow"
      visible: root.filterShown
      width: parent.width
      height: visible ? filterField.implicitHeight : 0

      TextField {
        id: filterField
        objectName: "filterField"
        // Hidden until `/` (an Item that is not visible cannot take focus: it is kept in
        // the tree at no width instead).
        width: root.filterShown ? parent.width - countLabel.width - Style.spacing.lg : 0
        opacity: root.filterShown ? 1.0 : 0.0
        placeholderText: Model.copy("updates.filter")
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
        anchors.right: parent.right
        anchors.verticalCenter: parent.verticalCenter
        width: visible ? implicitWidth : 0
        elide: Text.ElideNone
        // "0 of 5" stands out in bold foreground, never urgent text (§5.1, 1n2p.40).
        color: root.rows.length === 0 && root.scopeRows.length > 0 ? root.foreground : root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        font.bold: root.rows.length === 0 && root.scopeRows.length > 0
        text: Model.copy("updates.count", { "shown": root.rows.length, "total": root.scopeRows.length })
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

  // One row, as both lists build it: under All, its client's header above the client's
  // first row; the AddonRow; its release notes when opened.
  Component {
    id: rowDelegate

      Column {
        id: entry
        required property var modelData
        required property int index
        objectName: "updateRow_" + index
        width: column.width
        spacing: Style.spacing.sm
        readonly property bool open: root.expanded === modelData.key
        readonly property var notes: open ? Model.releaseNotes(root.svc ? root.svc.changelog : null, modelData, root.nowMs) : null
        // While a change runs on it, its phase; just after, what happened (1n2p.18).
        readonly property var rowState: Model.activityRow(root.svc ? root.svc.activity : null, modelData.key, root.done)

        // Under All, the client's header above its first row: its player name, whether it
        // runs, its updates (design doc §2.2), and Update {client}. The rows under it name
        // no client.
        Item {
          visible: entry.modelData.group !== ""
          width: parent.width
          height: visible ? Math.max(clientHeader.implicitHeight, clientUpdate.visible ? clientUpdate.implicitHeight : 0) : 0

          SectionHeader {
            id: clientHeader
            objectName: "clientHeader"
            anchors.left: parent.left
            anchors.right: clientUpdate.visible ? clientUpdate.left : parent.right
            anchors.rightMargin: clientUpdate.visible ? Style.spacing.md : 0
            anchors.verticalCenter: parent.verticalCenter
            title: entry.modelData.group
            count: entry.modelData.groupCount
            foreground: root.foreground
            dim: root.dim
            fontFamily: root.fontFamily
          }

          Button {
            id: clientUpdate
            objectName: "updateClient"
            visible: entry.modelData.groupUpdates > 0
            anchors.right: parent.right
            anchors.verticalCenter: parent.verticalCenter
            text: Model.copy("updates.update_client", { "client": entry.modelData.group })
            Accessible.role: Accessible.Button
            Accessible.name: text
            enabled: root.canChange
            opacity: enabled ? 1.0 : 0.4
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.caption
            onClicked: root.updateClient(entry.modelData.groupTarget)
          }
        }

        AddonRow {
          objectName: "addonRow"
          width: parent.width
          title: entry.modelData.name
          fromVersion: entry.modelData.fromVersion
          toVersion: entry.modelData.toVersion
          chips: entry.modelData.chips
          meta: entry.rowState && !entry.rowState.pending ? entry.rowState.text : entry.modelData.meta
          pending: !!entry.rowState && entry.rowState.pending
          pendingText: entry.rowState ? entry.rowState.text : ""
          band: entry.modelData.band
          glyph: entry.modelData.glyph ? String.fromCodePoint(entry.modelData.glyph) : ""
          actions: entry.modelData.actions
          cursored: !!root.frame && root.frame.cursorActive && root.frame.cursor === entry.index
          // Another build: visible, inert. A change running keeps the rows readable (their
          // own phase shows); canChange keeps them from acting.
          inert: !!root.frame && root.frame.readOnly === true
          foreground: root.foreground
          dim: root.dim
          urgent: root.urgent
          accent: root.accent
          fontFamily: root.fontFamily
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

        // `e`: every release note since the installed version, newest first, plain text,
        // with a link only to a safe host.
        Column {
          objectName: "notes"
          visible: entry.open
          width: parent.width
          leftPadding: Style.spacing.xl
          spacing: Style.spacing.sm

          // A failed read beside the alert glyph; otherwise dim (1n2p.40).
          AlertLine {
            objectName: "notesStatus"
            width: parent.width - Style.spacing.xl
            marked: !!entry.notes && entry.notes.state === "failed"
            foreground: root.foreground
            dim: root.dim
            urgent: root.urgent
            fontFamily: root.fontFamily
            text: !entry.notes || entry.notes.state === "reading" ? Model.copy("updates.notes.reading")
              : (entry.notes.state === "failed" ? entry.notes.failed
                : (entry.notes.entries.length === 0 ? Model.copy("updates.notes.none") : entry.notes.behind))
          }

          Repeater {
            objectName: "noteEntries"
            model: entry.notes ? entry.notes.entries : []

            Column {
              required property var modelData
              required property int index
              objectName: "noteEntry_" + index
              width: parent.width - Style.spacing.xl
              spacing: Style.spacing.xs

              PlainLabel {
                objectName: "noteVersion"
                width: parent.width
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                font.bold: true
                text: modelData.version + (modelData.date ? " · " + modelData.date : "")
              }

              PlainLabel {
                objectName: "noteText"
                width: parent.width
                wrapMode: Text.WordWrap
                elide: Text.ElideNone
                color: root.foreground
                font.family: root.fontFamily
                font.pixelSize: Style.font.caption
                text: modelData.text
              }
            }
          }

          Button {
            objectName: "notesLink"
            visible: !!entry.notes && entry.notes.url !== ""
            text: entry.notes ? entry.notes.link : ""
            Accessible.role: Accessible.Link
            Accessible.name: text
            bordered: true
            foreground: root.foreground
            fontFamily: root.fontFamily
            fontSize: Style.font.caption
            onClicked: if (Model.isSafeUrl(entry.notes.url)) Quickshell.execDetached(["omarchy-launch-browser", entry.notes.url])
          }
        }
      }
  }
}
