import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// The Search view (1n2p.25, design doc §2.3): the box at the top, searched as it is typed
// (after Model.SEARCH_DEBOUNCE_MS of quiet, from Model.SEARCH_MIN_CHARS characters; Enter
// searches at once), on the network lane, each search view-bound to its query and the
// client scope (All searches every client in one request). The last query's results stay,
// under "Searching…", until the next one's land, and a newer query stops an older search
// (Service.searchFor). A pasted ref or link is no search: it is one row, "Install from
// GitHub", whose page resolves it (the text only ever reaches the read-only `info`, M3-D2).
// Each result is an AddonRow (Model.searchRows): its name, its summary, its verdict in
// words with the source, popularity, age and author, the installed marker. Enter opens a
// result's page; `i` installs it into the scope's client where its hint allows, and under
// All opens a chooser of the clients, preselecting none and saying why each other one is
// not offered, so a hint is never promoted to a destination; where nothing can be
// installed, `i` says why.
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  readonly property var statusTargets: svc && Array.isArray(svc.targets) ? svc.targets : []
  // The query the shown results answer (or will: the search it asked for runs).
  property string asked: ""
  // The query the pause will search when it ends (typing, or a new client scope).
  property string pending: ""
  // The box's text, when it is a ref or a link (Model.searchLink), else null.
  readonly property var link: Model.searchLink(field.text)
  readonly property var answer: svc && svc.search && svc.search.selection ? svc.search : null
  readonly property bool searching: !!svc && Array.isArray(svc.reading) && svc.reading.indexOf("search") !== -1
  readonly property bool answered: answers(asked, filter)
  // The results shown: this query's; while the next query runs, the last one's on the
  // same client; none for a pasted link (its row is the view) or an empty query.
  readonly property var results: link || asked === "" || !answer ? []
    : answered ? answer.rows
    : (searching && answer.selection.target === filter ? answer.rows : [])
  // An empty box browses the built-in catalog for the scope (1n2p.26): one local read per
  // scope, kept by the service; until it answers (or when it fails) the view says what
  // to type.
  readonly property bool browsing: field.text.trim() === ""
  readonly property var catalog: svc && svc.catalogs && svc.catalogs.hasOwnProperty(filter) ? svc.catalogs[filter] : null
  readonly property var rows: browsing
    ? Model.catalogRows({ "rows": catalog ? catalog.rows : [], "filter": filter, "statusTargets": statusTargets,
        "lists": svc ? svc.lists : null, "nowMs": frame ? frame.nowMs : Date.now() })
    : Model.searchRows({ "rows": results, "link": link, "filter": filter, "statusTargets": statusTargets,
        "lists": svc ? svc.lists : null, "nowMs": frame ? frame.nowMs : Date.now() })
  // What the view says around its results (design doc §7): what to type, searching,
  // nothing found, the sources that could not be searched, a search that failed.
  // A query the pause is about to search says "Searching…" already.
  readonly property string shownQuery: link ? link.text : browsing ? ""
    : (asked !== "" ? asked : (Model.searchesLive(field.text) ? field.text.trim() : ""))
  readonly property var viewState: Model.viewState("search", { "service": svc, "filter": filter,
    "query": shownQuery, "rows": rows.length, "nowMs": frame ? frame.nowMs : Date.now() })
  readonly property int rowCount: chooserOpen ? 0 : rows.length
  // Past LONG_LIST_ROWS (the catalog browse, a long search) the rows are a VirtualList
  // that builds only those in view and scrolls itself (1n2p.38).
  readonly property bool scrollsItself: !chooserOpen && rows.length > Model.LONG_LIST_ROWS
  // The field, or the chooser, has the keyboard.
  readonly property bool typing: field.activeFocus || chooserOpen
  property bool chooserOpen: false
  property var chooserRow: null
  property int chooserCursor: -1
  readonly property var chooserTargets: chooserRow ? chooserRow.targets : []
  readonly property var chooserEligible: chooserTargets.filter(function(t) { return t.eligible })
  // The chooser's lines: with two or more eligible clients, "Every client it works on
  // (n)" first (omad-3rre), never preselected; then each client with its verdict.
  readonly property var chooserChoices: (chooserEligible.length >= 2
    ? [{ "id": "", "every": true, "eligible": true, "words": Model.copy("search.chooser.every", { "n": chooserEligible.length }) }]
    : []).concat(chooserTargets)
  // Whether the panel shows this view: a new scope searches the shown query again only
  // then, and on coming back.
  readonly property bool current: !!frame && frame.view === "search"

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property color accent: frame ? frame.accent : Commons.Color.accent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function focusSearch() { field.forceActiveFocus() }
  function rowItem(i) { return scrollsItself ? rowList.rowItem(i) : rowRepeater.itemAt(i) }

  function positionCursor() { rowList.positionCursor() }

  onFilterChanged: followScope()
  onCurrentChanged: followScope()
  Component.onCompleted: Qt.callLater(root.followScope)

  // The catalog for the scope, read once (the service keeps it).
  function askCatalog() {
    if (svc && typeof svc.readCatalog === "function") svc.readCatalog(filter)
  }

  // Whether the service's answer is for `query` on `target`. The change handlers below
  // ask this instead of reading `answered`, a binding that may not have caught up with
  // the change yet.
  function answers(query, target) {
    return !!answer && answer.selection.query === query && answer.selection.target === target
  }

  // The shown query, searched again for the scope now chosen (after the pause, so
  // stepping through clients with `t` searches once).
  function followScope() {
    if (!current) return
    if (field.text.trim() === "") {
      askCatalog()
      return
    }
    if (asked === "" || Model.searchLink(field.text) || answers(asked, filter) || searching) return
    pending = asked
    debounce.restart()
  }

  // The box changed: an empty box clears the search and goes back to the catalog, the
  // cursor at its top; a query long enough is searched after the pause; a link is no
  // search.
  function typed() {
    var text = field.text.trim()
    if (text === "") {
      debounce.stop()
      pending = ""
      asked = ""
      if (frame) frame.cursor = 0
      if (current) askCatalog()
      return
    }
    if (Model.searchesLive(text)) {
      pending = text
      debounce.restart()
    } else {
      debounce.stop()
    }
  }

  // `text` in the box and searched at once, as typing it and Enter would (omad-z5il: a
  // failure's Search for it). The box keeps the keys, as after `s`.
  function searchFor(text) {
    field.text = String(text || "")
    focusSearch()
    return searchQuery(field.text)
  }

  // `query` searched now, unless it is the one the shown answer (or the running search)
  // is for already.
  function searchQuery(query) {
    debounce.stop()
    var q = String(query || "").trim()
    if (q === "" || Model.searchLink(q) || !svc) return false
    if (q === asked && (answers(q, filter) || searching)) return false
    asked = q
    svc.searchFor(q, filter)
    return true
  }

  // KEYMAP's search rows: Enter opens the result's page (a link's page resolves it); `i`
  // installs it where its hint allows, else says why.
  function handleKey(name, cursor) {
    var row = cursor >= 0 && cursor < rows.length ? rows[cursor] : null
    if (name === "enter") {
      if (row) open(row)
      return true
    }
    if (name === "install") {
      if (row) install(row)
      return true
    }
    return false
  }

  function open(row) {
    if (row.kind === "link") return frame.openLink(row.link)
    return frame.openAddon(row.name, row.ref)
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  // The search box's keys (KEYMAP's field rows): Enter searches now and goes to the
  // results, Down and Up move the results' cursor while the box keeps the keys, Esc
  // leaves the box with its text kept; every other key is typed.
  function fieldKey(code, text, modifiers) {
    var name = Model.keyName(code, text, modifiers)
    var row = Model.keyRow(name, "field")
    if (!row || !frame) return false
    if (row.action === "search") {
      searchQuery(field.text)
      if (rows.length > 0) select(0)
      frame.leaveField()
    } else if (row.action === "move") {
      frame.moveCursor(frame.cursorActive ? frame.cursor + Model.keyStep(name) : 0)
    } else if (row.action === "leave") {
      frame.leaveField()
    }
    return true
  }

  // Esc where the view has the keys (design doc §2.4): the search goes (the box, its
  // results); false when there is none, so Esc closes the panel.
  function escapeFilter() {
    if (field.text === "" && asked === "") return false
    field.text = ""
    debounce.stop()
    pending = ""
    asked = ""
    if (frame) {
      frame.cursor = 0
      frame.cursorActive = false
    }
    return true
  }

  // Try again (a search that failed, or a source that did not answer): the shown query,
  // asked again.
  function searchAgain() {
    if (asked === "" || !svc) return false
    debounce.stop()
    svc.searchFor(asked, filter)
    return true
  }

  // `i` on a row: a link's page; a result into the scope's client where its hint allows,
  // under All the chooser; else what stands in the way, in words.
  function install(row) {
    if (row.kind === "link") return open(row)
    if (row.action !== "install") {
      frame.keyNote = row.note
      return false
    }
    if (filter) return installInto(filter, row)
    frame.pointerGate.reset()
    chooserRow = row
    chooserCursor = -1
    chooserOpen = true
    chooserKeys.forceActiveFocus()
    return true
  }

  function installInto(target, row) {
    if (!canChange) return false
    // The view shows no release for a search result: the dialog always opens (M4-D28).
    return frame.report(svc.run("install", { "target": target, "ref": row.ref }, "Install " + row.name, []))
  }

  // Every eligible client at once (omad-3rre): each client's preview in turn, one sheet
  // for all of them, then each plan applied in turn.
  function installEverywhere(row) {
    if (!canChange) return false
    return frame.report(svc.installEach(row.ref, row.name, chooserEligible.map(function(t) { return t.id })))
  }

  // The chooser's cursor moves among the eligible lines only; it starts on none.
  function moveChooser(step) {
    var eligible = []
    for (var i = 0; i < chooserChoices.length; i++) if (chooserChoices[i].eligible) eligible.push(i)
    if (eligible.length === 0) return
    if (frame) frame.pointerGate.reset()
    var at = eligible.indexOf(chooserCursor)
    at = at < 0 ? (step > 0 ? 0 : eligible.length - 1) : Math.max(0, Math.min(eligible.length - 1, at + step))
    chooserCursor = eligible[at]
  }

  function closeChooser() {
    chooserOpen = false
    chooserRow = null
    chooserCursor = -1
  }

  // A dialog's keys, as KEYMAP's sheet rows say: Esc and n close the chooser, j/k move
  // among the eligible clients, Enter and Space install into the cursored one (never
  // from auto-repeat, M3-D46). Every other key is the chooser's.
  function chooserKey(key, text, autoRepeat, modifiers) {
    var name = Model.keyName(key, text, modifiers)
    var action = Model.dialogKey(name, autoRepeat === true)
    if (action === "cancel") closeChooser()
    else if (action === "line") moveChooser(Model.keyStep(name))
    else if (action === "press") {
      var chosen = chooserCursor >= 0 ? chooserChoices[chooserCursor] : null
      if (chosen && chosen.eligible) {
        if (chosen.every) installEverywhere(chooserRow)
        else installInto(chosen.id, chooserRow)
        closeChooser()
      }
    }
    return true
  }

  Timer {
    id: debounce
    objectName: "searchDebounce"
    interval: Model.SEARCH_DEBOUNCE_MS
    onTriggered: root.searchQuery(root.pending)
  }

  Item {
    id: chooserKeys
    objectName: "chooserKeys"
    Keys.onPressed: function(event) {
      event.accepted = root.chooserKey(event.key, event.text, event.isAutoRepeat, event.modifiers)
    }
  }

  Column {
    id: column
    width: root.width
    spacing: Style.spacing.sm

    TextField {
      id: field
      objectName: "searchField"
      width: parent.width
      placeholderText: Model.copy("search.box")
      Accessible.role: Accessible.EditableText
      Accessible.name: placeholderText
      foreground: root.foreground
      onTextChanged: root.typed()
      Keys.onPressed: function(event) { event.accepted = root.fieldKey(event.key, event.text, event.modifiers) }
    }

    // In the All filter: which client to install into, none chosen yet; a client it is
    // not offered on says why (its verdict in words).
    Column {
      objectName: "chooser"
      visible: root.chooserOpen
      width: parent.width
      spacing: Style.spacing.sm

      PlainLabel {
        objectName: "chooserTitle"
        width: parent.width
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
        text: root.chooserRow ? Model.copy("search.chooser.title", { "addon": root.chooserRow.name }) + " (j/k, Enter, Esc)" : ""
      }

      Repeater {
        objectName: "chooserTargets"
        model: root.chooserChoices

        CursorSurface {
          required property var modelData
          required property int index
          width: parent.width
          implicitHeight: choice.implicitHeight + Style.spacing.md * 2
          hasCursor: root.chooserCursor === index
          opacity: modelData.eligible ? 1.0 : 0.4
          foreground: root.foreground
          // The client and its verdict in words, which say why one is not offered.
          Accessible.role: Accessible.ListItem
          Accessible.name: String(modelData.words || "")

          PlainLabel {
            id: choice
            objectName: "choice"
            anchors.left: parent.left
            anchors.leftMargin: Style.spacing.lg
            anchors.right: parent.right
            anchors.rightMargin: Style.spacing.lg
            anchors.verticalCenter: parent.verticalCenter
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            text: modelData.words
          }

          // Hover moves the chooser's cursor on real pointer motion; one click installs
          // (§4, 1n2p.15).
          MouseArea {
            id: choiceArea
            anchors.fill: parent
            enabled: modelData.eligible
            hoverEnabled: true
            onPositionChanged: function(mouse) {
              if (!root.frame || root.frame.pointerGate.moved(choiceArea, mouse)) root.chooserCursor = index
            }
            onClicked: {
              root.chooserCursor = index
              root.chooserKey(Qt.Key_Return, "")
            }
          }
        }
      }
    }

    // An empty box: what the list is (1n2p.26).
    PlainLabel {
      objectName: "browseHeader"
      visible: root.browsing && root.rows.length > 0 && !root.chooserOpen
      width: parent.width
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: root.filter ? Model.copy("search.browse.client", { "client": Model.heroTitle(root.statusTargets, root.filter) })
        : Model.copy("search.browse.all")
    }

    ViewState {
      objectName: "viewState"
      visible: !root.chooserOpen
      width: parent.width
      viewState: root.viewState
      foreground: root.foreground
      fontFamily: root.fontFamily
      onAction: function(name) { if (root.frame) root.frame.stateAction(name) }
    }

    // Short lists: every row built, in the content the frame scrolls.
    Repeater {
      id: rowRepeater
      model: root.chooserOpen || root.scrollsItself ? [] : root.rows
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

  // One result, as both lists build it.
  Component {
    id: rowDelegate

      Item {
        id: entry
        required property var modelData
        required property int index
        objectName: "searchRow_" + index
        width: column.width
        implicitHeight: addonRow.implicitHeight

        AddonRow {
          id: addonRow
          objectName: "addonRow"
          width: parent.width
          title: entry.modelData.title
          detail: entry.modelData.detail
          meta: entry.modelData.meta
          chips: entry.modelData.chips
          glyph: entry.modelData.glyph
          actions: entry.modelData.actions
          cursored: !!root.frame && root.frame.cursorActive && root.frame.cursor === entry.index
          // Another build: visible, inert (as the Updates view's rows).
          inert: !!root.frame && root.frame.readOnly === true
          foreground: root.foreground
          dim: root.dim
          urgent: root.urgent
          accent: root.accent
          fontFamily: root.fontFamily
          // A click puts the cursor on it; its buttons act (design doc §4).
          pointerGate: root.frame ? root.frame.pointerGate : null
          onHovered: root.select(entry.index)
          // One click opens, as Enter does (§4, 1n2p.15).
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
