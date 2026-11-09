import QtQuick
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// The Search view (F6): a query (Enter in the field searches on the network lane; the
// All filter searches every client in one request), results view-bound to the query
// and filter they were asked for, each with the clients' compatibility hints. Enter on
// a result installs it into the filtered client when its hint allows; in the All
// filter it opens a chooser of the clients, preselecting none, so a hint is never
// promoted to a destination (review round 3, revision 20).
Item {
  id: root

  property var frame: null
  readonly property var svc: frame ? frame.svc : null
  readonly property string filter: frame ? frame.filter : ""
  // The query the shown results answer.
  property string asked: ""
  readonly property var results: svc && svc.search && svc.search.selection
    && svc.search.selection.query === asked && svc.search.selection.target === filter
    ? svc.search.rows : []
  // What the view says around its results (design doc §7): what to type, searching,
  // nothing found, the sources that could not be searched, a search that failed.
  readonly property var viewState: Model.viewState("search", { "service": svc, "filter": filter, "query": asked,
    "rows": results.length, "nowMs": frame ? frame.nowMs : Date.now() })
  readonly property int rowCount: chooserOpen ? 0 : results.length
  // The field, or the chooser, has the keyboard.
  readonly property bool typing: field.activeFocus || chooserOpen
  property bool chooserOpen: false
  property var chooserRow: null
  property int chooserCursor: -1
  readonly property var chooserTargets: chooserRow ? Model.installTargets(chooserRow, svc && Array.isArray(svc.targets) ? svc.targets : []) : []

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function focusSearch() { field.forceActiveFocus() }
  function rowItem(i) { return rowRepeater.itemAt(i) }

  // KEYMAP's search rows: Enter and `i` install the cursored result.
  function handleKey(name, cursor) {
    if (name === "enter" || name === "install") {
      var row = cursor >= 0 && cursor < results.length ? results[cursor] : null
      if (row) install(row)
      return true
    }
    return false
  }

  // The search box's keys (KEYMAP's field rows): Enter searches, Esc leaves the box with
  // its text kept; every other key is typed.
  function fieldKey(code, text, modifiers) {
    var row = Model.keyRow(Model.keyName(code, text, modifiers), "field")
    if (!row) return false
    if (row.action === "search") search()
    else if (row.action === "leave") frame.leaveField()
    return true
  }

  function search() {
    var query = field.text.trim()
    if (query === "") return
    asked = query
    svc.searchFor(query, filter)
    frame.leaveField()
  }

  // Try again (a search that failed): the shown query, asked again.
  function searchAgain() {
    if (asked === "" || !svc) return false
    svc.searchFor(asked, filter)
    return true
  }

  // Enter on a result: into the filtered client when its hint allows, else nothing;
  // under All, the chooser.
  function install(row) {
    if (filter) {
      var target = Model.installTargets(row, [{ "id": filter }])[0]
      if (target.eligible) installInto(filter, row)
      return
    }
    chooserRow = row
    chooserCursor = -1
    chooserOpen = true
    chooserKeys.forceActiveFocus()
  }

  function installInto(target, row) {
    if (!canChange) return
    // The view shows no release for a search result: the dialog always opens (M4-D28).
    frame.report(svc.run("install", { "target": target, "ref": row.ref }, "Install " + row.name, []))
  }

  // The chooser's cursor moves among the eligible clients only; it starts on none.
  function moveChooser(step) {
    var eligible = []
    for (var i = 0; i < chooserTargets.length; i++) if (chooserTargets[i].eligible) eligible.push(i)
    if (eligible.length === 0) return
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
      var chosen = chooserCursor >= 0 ? chooserTargets[chooserCursor] : null
      if (chosen && chosen.eligible) {
        installInto(chosen.id, chooserRow)
        closeChooser()
      }
    }
    return true
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
      placeholderText: "Search addons (" + Model.keyHint("search", "view") + " types here, "
        + Model.keyHint("search", "field") + " searches)"
      foreground: root.foreground
      Keys.onPressed: function(event) { event.accepted = root.fieldKey(event.key, event.text, event.modifiers) }
    }

    // In the All filter: which client to install into, none chosen yet.
    Column {
      objectName: "chooser"
      visible: root.chooserOpen
      width: parent.width
      spacing: Style.spacing.sm

      PlainLabel {
        width: parent.width
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
        text: root.chooserRow ? "Install " + root.chooserRow.name + " into: (j/k, Enter, Esc)" : ""
      }

      Repeater {
        objectName: "chooserTargets"
        model: root.chooserTargets

        CursorSurface {
          required property var modelData
          required property int index
          width: parent.width
          implicitHeight: choice.implicitHeight + Style.spacing.md * 2
          hasCursor: root.chooserCursor === index
          opacity: modelData.eligible ? 1.0 : 0.4
          foreground: root.foreground

          PlainLabel {
            id: choice
            anchors.left: parent.left
            anchors.leftMargin: Style.spacing.lg
            anchors.verticalCenter: parent.verticalCenter
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            text: modelData.id + " · " + modelData.hint
          }

          MouseArea {
            anchors.fill: parent
            enabled: modelData.eligible
            onClicked: {
              root.chooserCursor = index
              root.chooserKey(Qt.Key_Return, "")
            }
          }
        }
      }
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

    Repeater {
      id: rowRepeater
      model: root.chooserOpen ? [] : root.results

      CursorSurface {
        id: entry
        required property var modelData
        required property int index
        objectName: "searchRow_" + index
        width: column.width
        implicitHeight: line.implicitHeight + Style.spacing.md * 2
        hasCursor: !!root.frame && root.frame.cursorActive && root.frame.cursor === index
        foreground: root.foreground

        Row {
          id: line
          anchors.left: parent.left
          anchors.leftMargin: Style.spacing.lg
          anchors.verticalCenter: parent.verticalCenter
          spacing: Style.spacing.lg

          PlainLabel {
            color: root.foreground
            font.family: root.fontFamily
            font.pixelSize: Style.font.body
            font.bold: true
            text: entry.modelData.name
          }

          PlainLabel {
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: entry.modelData.source + (Model.popularityText(entry.modelData) ? " · " + Model.popularityText(entry.modelData) : "")
          }

          // The filtered client's hint, or every client's under All.
          PlainLabel {
            objectName: "hints"
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: Model.installTargets(entry.modelData, root.svc && Array.isArray(root.svc.targets) ? root.svc.targets : [])
              .filter(function(t) { return !root.filter || t.id === root.filter })
              .map(function(t) { return t.id + " " + t.hint }).join(" · ")
          }
        }

        MouseArea {
          anchors.fill: parent
          onClicked: {
            root.frame.cursor = entry.index
            root.frame.cursorActive = true
          }
          onDoubleClicked: root.install(entry.modelData)
        }
      }
    }
  }
}
