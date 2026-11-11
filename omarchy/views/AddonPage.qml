import QtQuick
import Quickshell
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import ".."
import "../ui"
import "../Model.js" as Model

// An addon's page (1n2p.23, design doc §2.4), pushed on the navigation stack by
// frame.openAddon from a list (Updates, Addons, Search) or from another page (a
// dependency): what it is (its TOC's title, what its source says of it, its web page),
// one line per client (the version installed there and what is waiting, else whether
// the newest release loads there), its release notes, the folders it owns, what it
// needs and what needs it (each a page of its own), and its Lua errors (each client's
// error page). The local facts (`list -v`) show at once; the page's own `info
// --changelog` read, on the network lane and view-bound to this addon, adds the rest
// (Model.addonPage). The cursor walks the client lines; the keys act on the line under
// it, as the Addons view's do on a row: Enter repairs, updates or installs, `u`, `i`,
// `x`, `p`, `r`, `o`; `w` opens its web page, `y` copies its source. `v` opens the
// line's client's Versions (1n2p.24, its own `info --releases` read): the undo of its
// last update, the channel to follow and each release listed, lines the cursor walks
// after the client lines, each previewed in the plan sheet on Enter. Its release notes
// are lines too, after those (omad-z3ur): each folded to its first line but the newest,
// Enter or a click shows all of it. A dependency no client has offers Install it, on the
// cursored client line's client; its Folders say which are gone from disk and which
// clients share them. The keys are Model.KEYMAP's page rows.
Item {
  id: root

  property var frame: null
  // The addon: its name as the list named it, and its ref when the list knew one; or,
  // from Search's pasted ref or link (1n2p.25), only that text, `link`: the page's read
  // resolves it, and its answer names the addon and the ref an install takes (M3-D2).
  property string name: ""
  property string ref: ""
  property string link: ""

  readonly property var svc: frame ? frame.svc : null
  readonly property var statusTargets: svc && Array.isArray(svc.targets) ? svc.targets : []
  readonly property double nowMs: frame ? frame.nowMs : Date.now()
  // What the page reads it by: its ref, else its name, else the pasted text.
  readonly property string query: ref !== "" ? ref : (name !== "" ? name : link)
  // This page's own read, once it answers for this addon.
  readonly property var info: svc && svc.addonInfo && svc.addonInfo.selection && svc.addonInfo.selection.ref === query
    ? svc.addonInfo : null
  // The client whose Versions `v` opened (1n2p.24), "" when they are closed.
  property string versionsFor: ""
  // The release notes Enter or a click folded or unfolded, by version (omad-z3ur): the
  // newest starts unfolded, every other folded to its first line.
  property var notesOpen: ({})
  readonly property var page: Model.addonPage({ "name": name, "ref": ref, "link": link, "lists": svc ? svc.lists : null,
    "info": info ? info.data : null, "reading": !!info && !info.data && !info.failed, "failed": info ? info.failed : "",
    "errors": svc ? svc.errors : null, "history": svc ? svc.history : null, "statusTargets": statusTargets, "nowMs": nowMs,
    "versionsFor": versionsFor, "releasesRead": svc ? svc.addonReleases : null, "notesOpen": notesOpen,
    "statusWarnings": svc && Array.isArray(svc.statusWarnings) ? svc.statusWarnings : [] })
  // The lines the cursor walks: the client lines (the frame's `o` opens the one under
  // it), then the open Versions' lines, then the release notes.
  readonly property var rows: page.lines
  readonly property int rowCount: rows.length
  readonly property bool typing: false
  // What `y` copied, said for a moment.
  property string copied: ""
  property string askedFor: ""
  // The line under the cursor, null when the cursor is off: what Install it acts on.
  readonly property var cursoredLine: !!frame && frame.cursorActive ? lineAt(frame.cursor) : null

  readonly property color foreground: frame ? frame.foreground : Commons.Color.foreground
  readonly property color dim: frame ? frame.dim : Qt.darker(foreground, 1.55)
  readonly property color urgent: frame ? frame.urgent : Commons.Color.urgent
  readonly property color accent: frame ? frame.accent : Commons.Color.accent
  readonly property string fontFamily: frame ? frame.fontFamily : Style.font.family
  readonly property bool canChange: !!frame && frame.canAct && !frame.busy

  implicitHeight: column.implicitHeight

  function rowItem(i) {
    var clients = page.clients.length
    var versions = page.versions.lines.length
    if (i < clients) return lineRepeater.itemAt(i)
    return i < clients + versions ? versionRepeater.itemAt(i - clients) : noteRepeater.itemAt(i - clients - versions)
  }

  // The read, once the frame has handed the page its addon (its props come after the
  // page is made, one by one: callLater asks once for the last of them).
  onQueryChanged: Qt.callLater(root.ask)
  onSvcChanged: Qt.callLater(root.ask)
  Component.onCompleted: Qt.callLater(root.ask)
  Component.onDestruction: if (svc) svc.clearAddonInfo()
  function ask() {
    if (!svc || query === "" || askedFor === query) return
    askedFor = query
    copied = ""
    svc.readAddonInfo(query)
    // Its changes come from History's read, made once if nothing read it yet (local).
    if (!svc.history && typeof svc.readHistory === "function") svc.readHistory()
  }

  function lineAt(cursor) {
    return cursor >= 0 && cursor < rows.length ? rows[cursor] : null
  }

  function handleKey(name, cursor) {
    var line = lineAt(cursor)
    // A release note (omad-z3ur): Enter shows all of it or folds it. The line keys below
    // find no client on it and do nothing; `w` and `y` act on the page as anywhere.
    if (name === "enter" && line && line.kind === "note") return toggleNote(line)
    if (name === "enter") {
      // A Versions line: its undo, channel or version, previewed in the plan sheet.
      if (line && line.kind) run(line.request)
      else if (line && line.primary === "install") install(line)
      else if (line && line.primary !== "") run(Model.rowAction(line.row, line.primary))
      return true
    }
    if (name === "versions") return toggleVersions(line)
    if (name === "update") {
      if (line && line.row && line.row.updateAvailable) run(Model.rowAction(line.row, "update"))
      return true
    }
    if (name === "install") {
      if (line && line.primary === "install") install(line)
      return true
    }
    if (name === "remove") {
      if (line && line.row) run(Model.rowAction(line.row, "remove"))
      return true
    }
    if (name === "pin") {
      if (line && line.row) run(Model.rowAction(line.row, "pin"))
      return true
    }
    if (name === "restore") {
      if (line && line.installed) frame.openChooser({ "target": line.target, "addon": page.name })
      return true
    }
    if (name === "web") {
      openWeb(page.homepage)
      return true
    }
    if (name === "copy") {
      copyRef()
      return true
    }
    return false
  }

  function run(action) {
    if (!canChange || !action) return false
    return frame.report(svc.run(action.template, action.slots, action.label, action.shown))
  }

  // `v` (1n2p.24): the client line's Versions, its releases read once for this ref while
  // the page is open; `v` again on that client's line or one of its Versions' lines
  // closes them, the cursor back on the client's line.
  function toggleVersions(line) {
    if (!line || line.kind === "note" || !svc || page.ref === "" || page.name === "") return false
    if (versionsFor !== "" && (line.kind || line.target === versionsFor)) {
      var at = page.clients.map(function(c) { return c.target }).indexOf(versionsFor)
      versionsFor = ""
      if (at >= 0) select(at)
      return true
    }
    versionsFor = line.target
    svc.readAddonReleases(page.ref)
    // Brought into view at once, laid out first: the cursor on their first line (as a
    // key's move brings it), else their heading while the releases are read.
    column.forceLayout()
    frame.layoutContent()
    if (page.versions.lines.length > 0) frame.moveCursor(page.versions.lines[0].index)
    else frame.showItem(versionsState.visible ? versionsState : versionsHeader)
    return true
  }

  // Install here: the client line's client, previewed as Search's install is. The ref is
  // the list's or the read's, never a pasted link's text.
  function install(line) {
    if (!canChange || page.ref === "") return false
    return frame.report(svc.run("install", { "target": line.target, "ref": page.ref }, "Install " + page.name, []))
  }

  // Install it, beside a missing dependency (omad-z3ur): on the cursored client line's
  // client only, previewed in the plan sheet; with no such line nothing runs, and the
  // panel says to choose one.
  function installDependency(dependency) {
    var request = Model.dependencyInstall(cursoredLine, dependency)
    if (!request.action) {
      if (frame) frame.keyNote = request.note
      return false
    }
    return run(request.action)
  }

  // A release note folded to its first line, or shown whole (omad-z3ur).
  function toggleNote(line) {
    if (!line.foldable) return false
    var open = Object.assign({}, notesOpen)
    open[line.version] = line.folded
    notesOpen = open
    return true
  }

  function openWeb(url) {
    if (Model.isSafeUrl(url)) Quickshell.execDetached(["omarchy-launch-browser", url])
  }

  // Whether there was a source to copy; the flash follows once wl-copy answers.
  function copyRef() {
    if (!svc || page.ref === "") return false
    var ref = page.ref
    return svc.copyCommand(ref, function(ok) {
      root.copied = Model.copyFlash(ok, Model.copy("page.copied", { "ref": ref }))
      copiedTimer.restart()
    })
  }

  function select(index) {
    frame.cursor = index
    frame.cursorActive = true
  }

  // A Lua error line: that client's error page, as the Errors view opens it.
  function openErrors(entry) {
    var rows = Model.errorRows(Model.errorSections(svc ? svc.errors : null, statusTargets, "", nowMs))
    var row = rows.filter(function(r) { return r.key === entry.key })[0] || null
    if (!row) return false
    return frame.push({ "title": page.name, "component": errorPageComponent, "props": { "rowKey": row.key, "initial": row } })
  }

  Timer {
    id: copiedTimer
    interval: 1500
    onTriggered: root.copied = ""
  }

  Component {
    id: errorPageComponent
    ErrorPage {}
  }

  Column {
    id: column
    width: root.width
    spacing: Style.spacing.md

    // ---- what it is
    Column {
      width: parent.width
      spacing: Style.spacing.xs

      PlainLabel {
        objectName: "pageTitle"
        width: parent.width
        wrapMode: Text.WordWrap
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.body
        font.bold: true
        text: root.page.title
      }

      // Its name, when its TOC's title is another.
      PlainLabel {
        objectName: "pageName"
        visible: root.page.title !== root.page.name
        width: parent.width
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: root.page.name
      }

      PlainLabel {
        objectName: "pageSummary"
        visible: root.page.summary !== ""
        width: parent.width
        wrapMode: Text.WordWrap
        color: root.foreground
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: root.page.summary
      }

      PlainLabel {
        objectName: "pageFacts"
        visible: text !== ""
        width: parent.width
        wrapMode: Text.WordWrap
        color: root.dim
        font.family: root.fontFamily
        font.pixelSize: Style.font.caption
        text: [root.page.source, root.page.facts].filter(function(p) { return p !== "" }).join(" · ")
      }

      // Reading about it, why that failed, or why its source says nothing more.
      // A failed read beside the alert glyph; reading, or why nothing more, dim.
      AlertLine {
        objectName: "pageState"
        visible: text !== ""
        width: parent.width
        marked: root.page.state === "failed"
        foreground: root.foreground
        dim: root.dim
        urgent: root.urgent
        fontFamily: root.fontFamily
        text: root.page.state === "reading" ? Model.copy("page.reading")
          : (root.page.state === "failed" ? Model.copy("page.failed", { "why": root.page.failed }) : root.page.why)
      }
    }

    Flow {
      objectName: "pageButtons"
      width: parent.width
      spacing: Style.spacing.sm

      Button {
        objectName: "pageWeb"
        visible: root.page.homepage !== ""
        text: Model.withKey(Model.copy("page.web"), "web", "page")
        Accessible.role: Accessible.Link
        Accessible.name: text
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.openWeb(root.page.homepage)
      }

      Button {
        objectName: "pageCopy"
        visible: root.page.ref !== ""
        text: root.copied !== "" ? root.copied : Model.withKey(Model.copy("page.copy_ref"), "copy", "page")
        Accessible.role: Accessible.Button
        Accessible.name: text
        bordered: true
        foreground: root.foreground
        fontFamily: root.fontFamily
        fontSize: Style.font.bodySmall
        onClicked: root.copyRef()
      }
    }

    // ---- one line per client: the cursor's rows
    SectionHeader {
      objectName: "clientsHeader"
      width: parent.width
      title: Model.copy("page.section.clients")
      foreground: root.foreground
      dim: root.dim
      fontFamily: root.fontFamily
    }

    Repeater {
      id: lineRepeater
      objectName: "clientLines"
      // The client lines alone: the Versions' lines have a Repeater of their own below.
      model: root.page.clients

      AddonRow {
        required property var modelData
        required property int index
        objectName: "clientLine_" + modelData.target
        width: column.width
        title: modelData.client
        meta: modelData.meta
        chips: modelData.chips
        band: modelData.band
        glyph: modelData.glyph ? String.fromCodePoint(modelData.glyph) : ""
        actions: modelData.actions
        cursored: !!root.frame && root.frame.cursorActive && root.frame.cursor === index
        inert: !root.canChange
        foreground: root.foreground
        dim: root.dim
        urgent: root.urgent
        accent: root.accent
        fontFamily: root.fontFamily
        pointerGate: root.frame ? root.frame.pointerGate : null
        onHovered: root.select(index)
        // One click is Enter (§4, 1n2p.15): the line's change, previewed first.
        onActivated: {
          root.select(index)
          root.handleKey("enter", index)
        }
        onAction: function(name) {
          root.select(index)
          root.handleKey(name, index)
        }
      }
    }

    // ---- its Versions on one client (1n2p.24): `v` on a client line
    SectionHeader {
      id: versionsHeader
      objectName: "versionsHeader"
      visible: root.page.versions.open
      width: parent.width
      title: Model.copy("page.section.versions", { "client": root.page.versions.client })
      foreground: root.foreground
      dim: root.dim
      fontFamily: root.fontFamily
    }

    // Listing them, why that failed, or why its source lists none.
    AlertLine {
      id: versionsState
      objectName: "versionsState"
      visible: root.page.versions.open && text !== ""
      width: parent.width
      marked: root.page.versions.state === "failed"
      foreground: root.foreground
      dim: root.dim
      urgent: root.urgent
      fontFamily: root.fontFamily
      text: root.page.versions.state === "reading" ? Model.copy("page.versions.reading")
        : (root.page.versions.state === "failed" ? Model.copy("page.versions.failed", { "why": root.page.versions.failed })
          : root.page.versions.why)
    }

    Repeater {
      id: versionRepeater
      objectName: "versionLines"
      model: root.page.versions.lines

      AddonRow {
        required property var modelData
        objectName: "versionLine_" + modelData.key
        width: column.width
        title: modelData.title
        meta: modelData.meta
        chips: modelData.chips
        band: modelData.band
        glyph: modelData.glyph ? String.fromCodePoint(modelData.glyph) : ""
        actions: modelData.actions
        cursored: !!root.frame && root.frame.cursorActive && root.frame.cursor === modelData.index
        inert: !root.canChange
        foreground: root.foreground
        dim: root.dim
        urgent: root.urgent
        accent: root.accent
        fontFamily: root.fontFamily
        pointerGate: root.frame ? root.frame.pointerGate : null
        onHovered: root.select(modelData.index)
        // One click is Enter: the line's undo, channel or version, previewed first.
        onActivated: {
          root.select(modelData.index)
          root.handleKey("enter", modelData.index)
        }
        onAction: function(name) {
          root.select(modelData.index)
          root.handleKey(name, modelData.index)
        }
      }
    }

    // ---- its release notes, newest first
    SectionHeader {
      objectName: "notesHeader"
      visible: root.page.notes.length > 0 || root.page.state === ""
      width: parent.width
      title: Model.copy("page.section.notes")
      foreground: root.foreground
      dim: root.dim
      fontFamily: root.fontFamily
    }

    PlainLabel {
      objectName: "notesNone"
      visible: root.page.notes.length === 0 && root.page.state === "" && !!root.info
      width: parent.width
      color: root.dim
      font.family: root.fontFamily
      font.pixelSize: Style.font.caption
      text: Model.copy("page.notes.none")
    }

    // Each release a line the cursor walks (omad-z3ur): folded to its first line but the
    // newest; Enter or a click shows all of it, or folds it again.
    Repeater {
      id: noteRepeater
      objectName: "notes"
      model: root.page.notes

      CursorSurface {
        id: note
        required property var modelData
        readonly property bool cursored: !!root.frame && root.frame.cursorActive && root.frame.cursor === modelData.index
        objectName: "note_" + modelData.key
        width: column.width
        implicitHeight: noteColumn.implicitHeight + Style.spacing.sm * 2
        hasCursor: cursored
        foreground: root.foreground
        accent: root.accent
        Accessible.role: Accessible.ListItem
        Accessible.name: modelData.title + ", " + modelData.shown
        Accessible.description: modelData.hint

        Column {
          id: noteColumn
          anchors.left: parent.left
          anchors.right: parent.right
          anchors.leftMargin: Style.spacing.lg
          anchors.rightMargin: Style.spacing.lg
          anchors.verticalCenter: parent.verticalCenter
          spacing: Style.spacing.xs

          Item {
            width: parent.width
            implicitHeight: Math.max(noteTitle.implicitHeight, noteHint.implicitHeight)

            PlainLabel {
              id: noteTitle
              objectName: "noteTitle"
              anchors.left: parent.left
              anchors.right: noteHint.visible ? noteHint.left : parent.right
              anchors.rightMargin: Style.spacing.md
              color: root.foreground
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              font.bold: true
              text: note.modelData.title
            }

            // Its fold, on the cursored line only: what Enter does to it.
            PlainLabel {
              id: noteHint
              objectName: "noteHint"
              visible: note.cursored && note.modelData.hint !== ""
              anchors.right: parent.right
              width: visible ? implicitWidth : 0
              elide: Text.ElideNone
              color: root.dim
              font.family: root.fontFamily
              font.pixelSize: Style.font.caption
              text: note.modelData.hint
            }
          }

          PlainLabel {
            objectName: "noteText"
            width: parent.width
            wrapMode: Text.WordWrap
            color: root.dim
            font.family: root.fontFamily
            font.pixelSize: Style.font.caption
            text: note.modelData.shown
          }
        }

        // Hover moves the cursor (through the frame's PointerMoveGate); one click folds or
        // unfolds it, as Enter does.
        MouseArea {
          id: noteArea
          anchors.fill: parent
          hoverEnabled: true
          onPositionChanged: function(mouse) {
            if (!root.frame || root.frame.pointerGate.moved(noteArea, mouse)) root.select(note.modelData.index)
          }
          onClicked: {
            root.select(note.modelData.index)
            root.handleKey("enter", note.modelData.index)
          }
        }
      }
    }

    // ---- what it owns, needs and throws
    SectionHeader {
      objectName: "foldersHeader"
      visible: root.page.folders.length > 0
      width: parent.width
      title: Model.copy("page.section.folders")
      foreground: root.foreground
      dim: root.dim
      fontFamily: root.fontFamily
    }

    Repeater {
      objectName: "folders"
      model: root.page.folders

      Column {
        required property var modelData
        width: column.width
        spacing: Style.spacing.xs

        PlainLabel {
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.foreground
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: modelData.client + ": " + modelData.folders.join(", ")
        }

        PlainLabel {
          visible: modelData.leftOut.length > 0
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: Model.copy("page.folders.left_out", { "client": modelData.client, "folders": modelData.leftOut.join(", ") })
        }

        // Its folders gone from disk (omad-z3ur): words in the foreground beside the
        // alert glyph, never in the urgent colour (§5.1).
        AlertLine {
          objectName: "foldersMissing"
          visible: modelData.missingText !== ""
          width: parent.width
          marked: true
          foreground: root.foreground
          dim: root.dim
          urgent: root.urgent
          fontFamily: root.fontFamily
          text: modelData.missingText
        }

        // The clients that see these folders too: they share this client's AddOns folder.
        PlainLabel {
          objectName: "foldersShared"
          visible: modelData.sharedText !== ""
          width: parent.width
          wrapMode: Text.WordWrap
          color: root.dim
          font.family: root.fontFamily
          font.pixelSize: Style.font.caption
          text: modelData.sharedText
        }
      }
    }

    // A dependency, and what needs it: each a page of its own.
    Repeater {
      objectName: "links"
      model: [{ "id": "needs", "names": root.page.needs }, { "id": "needed_by", "names": root.page.neededBy.map(function(n) { return { "name": n, "installed": true } }) }]

      Column {
        id: linkGroup
        required property var modelData
        visible: modelData.names.length > 0
        width: column.width
        spacing: Style.spacing.xs

        SectionHeader {
          width: parent.width
          title: Model.copy("page.section." + linkGroup.modelData.id)
          foreground: root.foreground
          dim: root.dim
          fontFamily: root.fontFamily
        }

        Flow {
          objectName: "links_" + linkGroup.modelData.id
          width: parent.width
          spacing: Style.spacing.sm

          Repeater {
            model: linkGroup.modelData.names

            Row {
              id: entry
              required property var modelData
              // A dependency no client has (omad-z3ur): Install it beside its link.
              readonly property bool installable: linkGroup.modelData.id === "needs" && !modelData.installed
              spacing: Style.spacing.xs

              Button {
                objectName: "link_" + entry.modelData.name
                text: entry.modelData.installed ? entry.modelData.name : Model.copy("page.needs.missing", { "name": entry.modelData.name })
                // Each opens its own page. A missing one says so in words, beside the
                // alert glyph, in an urgent border: urgent is a mark's colour, never
                // text's (§5.1, 1n2p.40).
                iconText: entry.modelData.installed ? "" : String.fromCodePoint(0xF0028)
                Accessible.role: Accessible.Link
                Accessible.name: text
                foreground: root.foreground
                bordered: true
                fontFamily: root.fontFamily
                fontSize: Style.font.bodySmall
                onClicked: root.frame.openAddon(entry.modelData.name, "")

                Rectangle {
                  objectName: "missingBorder"
                  visible: !entry.modelData.installed
                  anchors.fill: parent
                  color: "transparent"
                  radius: Style.cornerRadius
                  border.width: 1
                  border.color: root.urgent
                }
              }

              // On the cursored client line's client, named on the button; with no such
              // line the panel says to choose one, and nothing runs.
              Button {
                objectName: "installDependency_" + entry.modelData.name
                visible: entry.installable
                readonly property var request: Model.dependencyInstall(root.cursoredLine, entry.modelData.name)
                text: request.text
                opacity: request.action && root.canChange ? 1.0 : 0.45
                Accessible.role: Accessible.Button
                Accessible.name: text
                foreground: root.foreground
                bordered: true
                fontFamily: root.fontFamily
                fontSize: Style.font.bodySmall
                onClicked: root.installDependency(entry.modelData.name)
              }
            }
          }
        }
      }
    }

    SectionHeader {
      objectName: "errorsHeader"
      visible: root.page.errors.length > 0
      width: parent.width
      title: Model.copy("page.section.errors")
      foreground: root.foreground
      dim: root.dim
      fontFamily: root.fontFamily
    }

    Flow {
      objectName: "errors"
      visible: root.page.errors.length > 0
      width: parent.width
      spacing: Style.spacing.sm

      Repeater {
        model: root.page.errors

        Button {
          required property var modelData
          objectName: "errorsOn_" + modelData.target
          text: modelData.text
          // U+F00E4 nf-md-bug (§5.4), its words in the foreground, in an urgent border
          // (§5.1, 1n2p.40).
          iconText: String.fromCodePoint(0xF00E4)
          Accessible.role: Accessible.Link
          Accessible.name: text
          foreground: root.foreground
          bordered: true
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          onClicked: root.openErrors(modelData)

          Rectangle {
            objectName: "errorsBorder"
            anchors.fill: parent
            color: "transparent"
            radius: Style.cornerRadius
            border.width: 1
            border.color: root.urgent
          }
        }
      }
    }

    // Its newest changes, from History's read (1n2p.30): each opens its change's page,
    // with that change's undo.
    SectionHeader {
      objectName: "changesHeader"
      visible: root.page.changes.length > 0
      width: parent.width
      title: Model.copy("page.section.changes")
      foreground: root.foreground
      dim: root.dim
      fontFamily: root.fontFamily
    }

    Flow {
      objectName: "changes"
      visible: root.page.changes.length > 0
      width: parent.width
      spacing: Style.spacing.sm

      Repeater {
        model: root.page.changes

        Button {
          required property var modelData
          required property int index
          objectName: "change_" + index
          text: modelData.text
          Accessible.role: Accessible.Link
          Accessible.name: text
          foreground: modelData.state === "undone" ? root.dim : root.foreground
          bordered: true
          fontFamily: root.fontFamily
          fontSize: Style.font.bodySmall
          onClicked: root.frame.openChange(modelData.key, modelData.title)
        }
      }
    }
  }
}
