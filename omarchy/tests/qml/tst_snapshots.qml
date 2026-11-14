import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import qs.Commons
import qs.Commons as Commons
import qs.Ui
import "../.."
import "../../ui"
import "../../Model.js" as Model
import "../../Build.js" as Build
import "Fixtures.js" as Fixtures

// Offscreen renders of every state a player can meet (the UX audit's harness, kept for
// every later widget bead). The real Service runs on the mock Process and is answered
// with the envelopes the real binary wrote in pipeline step 1 (Fixtures.js), edited
// where a state needs it (150 addons, a long name); the panel and the bar button are
// grabbed and saved as <Fixtures.SNAPSHOT_DIR>/<name>.png. In the gate the qs.Ui stubs
// draw them; `argc widget-snapshots` draws them with Omarchy's own shell kit and theme.
// The assertions are a smoke test: each PNG is written, non-empty, at the panel's size.
// Pixels are reviewed, never diffed.
TestCase {
  id: testCase
  name: "Snapshots"
  when: windowShown
  visible: true
  width: 680
  height: 1100

  // The panel as the bar's KeyboardPanel sizes it (BarWidget.qml): 600 wide by default
  // (a scene may set the minimum, 400, before it opens), at most 720 tall, inside the
  // popup card.
  property int panelWidth: 600
  readonly property int panelMaxHeight: 720
  readonly property int cardPadding: 12

  property var service: null
  property var card: null
  property var frame: null
  property var widget: null
  property var shots: []

  Component {
    id: serviceComponent
    Service {}
  }

  // The popup card the KeyboardPanel draws, with the frame inside.
  Component {
    id: cardComponent
    BorderSurface {
      id: surface
      property alias view: inner
      x: 10
      y: 10
      width: testCase.panelWidth + testCase.cardPadding * 2
      height: Math.min(inner.contentImplicitHeight, testCase.panelMaxHeight) + testCase.cardPadding * 2
      color: Commons.Color.popups.background
      borderSpec: Border.flat(Commons.Color.popups.border, 2)
      radius: Style.cornerRadius

      PanelView {
        id: inner
        x: testCase.cardPadding
        y: testCase.cardPadding
        width: testCase.panelWidth
        height: Math.min(contentImplicitHeight, testCase.panelMaxHeight)
        // As BarWidget hands down the bar's cap (1n2p.38: a long list fills it).
        heightCap: testCase.panelMaxHeight
      }
    }
  }

  // The shared components of omarchy/ui/ (§5), each in its states, in the popup card:
  // a row default, cursored, pending, inert and with a long name; the header, chips,
  // the empty state, the three notices, progress, a badge and a key hint.
  Component {
    id: galleryComponent
    BorderSurface {
      id: gallery
      property int panelWidth: 540
      readonly property var rowActions: [
        { "name": "update", "glyph": String.fromCodePoint(0xF0CE1), "tooltip": "Update (u)" },
        { "name": "notes", "glyph": String.fromCodePoint(0xF0DC9), "tooltip": "Notes (e)" },
        { "name": "pin", "glyph": String.fromCodePoint(0xF0403), "tooltip": "Pin (p)" }
      ]
      x: 10
      y: 10
      width: panelWidth + testCase.cardPadding * 2
      height: galleryColumn.implicitHeight + testCase.cardPadding * 2
      color: Commons.Color.popups.background
      borderSpec: Border.flat(Commons.Color.popups.border, 2)
      radius: Style.cornerRadius

      Column {
        id: galleryColumn
        x: testCase.cardPadding
        y: testCase.cardPadding
        width: gallery.panelWidth
        spacing: Style.spacing.md

        SectionHeader { width: parent.width; title: "Retail · running"; count: "3 updates"; buttonText: "Update Retail (3)" }
        AddonRow {
          width: parent.width; title: "DBM"; fromVersion: "12.1.14"; toVersion: "12.1.15"; band: "accent"
          glyph: String.fromCodePoint(0xF0CE1); meta: "2 days ago · then /reload in game"; chips: [{ "text": "Retail" }]
        }
        AddonRow {
          width: parent.width; title: "Plater"; fromVersion: "Plater-v661"; toVersion: "Plater-v662"; band: "accent"
          glyph: String.fromCodePoint(0xF0CE1); meta: "5 h ago"; cursored: true; actions: gallery.rowActions
          chips: [{ "text": "Retail" }, { "text": "beta" }]
        }
        AddonRow {
          width: parent.width; title: "WeakAuras"; fromVersion: "5.19.2"; toVersion: "5.20.0"; pending: true
          pendingText: "Updating… downloading (2 of 3)"; glyph: String.fromCodePoint(0xF0CE1)
        }
        AddonRow {
          width: parent.width; title: "Details_DamageMeter_StreamerModeAndPluginsCollection_Classic"; toVersion: "v662.20251109"
          band: "urgent"; glyph: String.fromCodePoint(0xF0DC9); meta: "WoWInterface · files changed on disk (3)"
          chips: [{ "text": "changed on disk", "urgent": true }, { "text": "Retail" }]
        }
        AddonRow {
          width: parent.width; title: "Questie"; toVersion: "12.1.1"; inert: true; glyph: String.fromCodePoint(0xF05E0)
          meta: "inert: the panel is from another version"
        }
        Separator { width: parent.width }
        Row {
          spacing: Style.spacing.md
          Chip { text: "Classic Era" }
          Chip { text: "files changed"; urgent: true }
          KeyHint { key: "u" }
          KeyHint { key: "Ctrl+Z" }
          Badge { count: 3 }
          Badge { count: 2; urgent: true }
        }
        ProgressBar { width: parent.width; fraction: 0.4 }
        ProgressBar { width: parent.width }
        EmptyState {
          width: parent.width; glyph: String.fromCodePoint(0xF05E0); sentence: "All up to date. Checked 25 min ago."
          actionText: "Check now"; actionKey: "c"
        }
        NoticeCard {
          width: parent.width; kind: "failure"; title: "Couldn't reach GitHub"; body: "Check the connection."
          buttons: [{ "name": "retry", "text": "Try again" }]; dismissible: true
        }
        NoticeCard { width: parent.width; kind: "info"; title: "Some errors couldn't be read yet: the game was writing." }
        NoticeCard { width: parent.width; kind: "success"; title: "Updated 3 addons on Retail"; buttons: [{ "name": "undo", "text": "Undo" }] }
      }
    }
  }

  // An open sheet over a panel-sized area: the plan sheet's shape.
  Component {
    id: sheetGalleryComponent
    Rectangle {
      property int panelWidth: 540
      x: 10
      y: 10
      width: panelWidth + testCase.cardPadding * 2
      height: 360
      color: Commons.Color.popups.background

      Sheet {
        anchors.fill: parent
        opened: true
        title: "Remove DBM from Retail?"
        confirmText: "Remove"
        destructive: true
        confirmGlyph: String.fromCodePoint(0xF01B4)
        selectedIndex: 0

        PlainLabel { width: parent.width; text: "DBM 12.1.15 (1 folder)" }
        PlainLabel { width: parent.width; text: "Removes the folder DBM-Core. Undo brings it back."; color: Model.dimColor(Commons.Color.foreground, Commons.Color.popups.background) }
      }
    }
  }

  // Every cell of one view's state matrix (design doc §7, 1n2p.19), each as ViewState
  // draws Model.viewState's answer, under the cell's name.
  Component {
    id: statesGalleryComponent
    Rectangle {
      // Not `states`: that is every Item's own list of States.
      id: matrix
      property int panelWidth: 540
      property string view: "updates"
      readonly property var cells: testCase.stateCells(view)
      x: 10
      y: 10
      width: panelWidth + testCase.cardPadding * 2
      height: statesColumn.implicitHeight + testCase.cardPadding * 2
      color: Commons.Color.popups.background

      Column {
        id: statesColumn
        x: testCase.cardPadding
        y: testCase.cardPadding
        width: matrix.panelWidth
        spacing: Style.spacing.md

        // By index: each cell's state stays the JavaScript object Model.viewState made.
        Repeater {
          model: matrix.cells.length

          Column {
            required property int index
            readonly property var cell: matrix.cells[index]
            width: statesColumn.width
            spacing: Style.spacing.xs

            SectionHeader { width: parent.width; title: cell.title }
            ViewState { width: parent.width; viewState: cell.state }
          }
        }
      }
    }
  }

  // The bar's PluginBarApi facade, as much of it as the bar button reads.
  Component {
    id: barApiComponent
    QtObject {
      property var shell: null
      property color foreground: Commons.Color.foreground
      property color barForeground: Commons.Color.foreground
      property color urgent: Commons.Color.urgent
      property string fontFamily: Style.font.family
      property bool vertical: false
      property int barSize: 26
      property bool foregroundAnimationEnabled: false
      function showTooltip(item, text) {}
      function hideTooltip(item) {}
    }
  }

  Component {
    id: barComponent
    BarWidget {}
  }

  // The bar as it sits on screen: a strip in the bar's background with the button,
  // drawn four times larger so the glyph and the badge can be read.
  Component {
    id: stripComponent
    Item {
      property alias strip: strip
      x: 10
      y: 10
      width: strip.width * strip.scale
      height: strip.height * strip.scale

      Rectangle {
        id: strip
        width: 120
        height: 30
        scale: 4
        transformOrigin: Item.TopLeft
        color: Commons.Color.background
      }
    }
  }

  // ---------------------------------------------------------------- the binary

  // The command words and arguments after the standard flags (tst_plans.qml's).
  // The command words after the standard flags, by name: History's read names no actor
  // (1n2p.30).
  function words(command) {
    var i = 1
    while (i < command.length) {
      if (command[i] === "--json" || command[i] === "--dry-run") i += 1
      else if (["--actor", "--reason", "--progress", "--if-match"].indexOf(command[i]) !== -1) i += 2
      else break
    }
    return command.slice(i)
  }

  function pending() {
    var out = []
    for (var i = 0; i < ProcessRegistry.launches.length; i++) {
      var launch = ProcessRegistry.launches[i]
      if (launch.command[1] !== "--json") continue
      var p = launch.process
      if (!p || p.running !== true) continue
      out.push(launch)
    }
    return out
  }

  // A fixture envelope as an object, to edit; its text, to answer with.
  function fixture(name) { return JSON.parse(Fixtures.text(name)) }
  function text(env) { return JSON.stringify(env) }
  function ok(command, data) {
    return text({ "ok": true, "schema_version": "1.1", "tool_version": Build.TOOL_VERSION, "command": command,
      "exit_code": 0, "partial": false, "data": data, "warnings": [], "commands": [] })
  }

  // What the binary answers, by the request's words: a scene maps a resource to an
  // envelope (`{text, code}`), and a resource the scene leaves out stays running.
  function answerOf(scene, w) {
    var dry = false
    if (w[0] === "status") return scene.status
    if (w[0] === "list") return scene.list
    if (w[0] === "errors" && w[1] === "dismiss") return scene.dismiss
    if (w[0] === "errors" && w.indexOf("--raw-details") !== -1) return scene.errorsRaw
    if (w[0] === "errors") return scene.errors
    if (w[0] === "backup" && w[1] === "list" && w.indexOf("-v") !== -1) return scene.backupsDetail
    if (w[0] === "backup" && w[1] === "list") return scene.backups
    if (w[0] === "backup" && w[1] === "create" && w.indexOf("--auto") !== -1)
      return { "text": ok("backup create", { "dry_run": false, "run": null, "targets": [] }) }
    if (w[0] === "search" && w.indexOf("--catalog") !== -1) return scene.catalog
    if (w[0] === "search") return scene.search
    // An addon page's Versions (1n2p.24) read its releases.
    if (w[0] === "info" && w.indexOf("--releases") !== -1) return scene.releases
    if (w[0] === "info") return scene.changelog
    // omaddons' settings, for the settings sheet (1n2p.31).
    if (w[0] === "config" && w.length === 1) return scene.config
    if (w[0] === "outdated") return scene.check
    return scene[w[0]]
  }

  // Answers every running request the scene has an answer for, as the lanes start
  // them, until none is left to answer.
  function settle(scene) {
    for (var round = 0; round < 60; round++) {
      var answered = 0
      var running = pending()
      for (var i = 0; i < running.length; i++) {
        var answer = answerOf(scene, words(running[i].command))
        if (!answer) continue
        if (answer.stderr) for (var j = 0; j < answer.stderr.length; j++) running[i].process.emitStderr(answer.stderr[j])
        running[i].process.finish(answer.code || 0, answer.text, "streamFirst")
        answered++
      }
      wait(answered ? 5 : 20)
      if (!answered && round > 2) break
    }
  }

  function the(name, code) { return { "text": Fixtures.text(name), "code": code || 0 } }

  // ---------------------------------------------------------------- the scenes

  // A fresh Service on a fresh mock; `missing` makes the binary absent, and `extra`
  // overrides its timings.
  function start(missing, extra) {
    if (service) service.destroy()
    service = null
    wait(0)
    ProcessRegistry.reset()
    ProcessRegistry.missingBinary = missing === true
    Quickshell.resetDetachedCommands()
    var props = { "bellDebounceMs": 10, "localDeadlineMs": 600000, "networkDeadlineMs": 600000, "killGraceMs": 60, "streamGraceMs": 30 }
    if (extra) for (var key in extra) props[key] = extra[key]
    service = serviceComponent.createObject(testCase, props)
    verify(service !== null)
  }

  // The service answered with `scene`, the panel open on `view` (and `filter`).
  function open(scene, view, filter) {
    start(false)
    settle(scene)
    show(scene, view, filter)
  }

  function show(scene, view, filter) {
    if (!card) {
      card = cardComponent.createObject(testCase, { })
      verify(card !== null)
      frame = card.view
    }
    frame.svc = service
    service.setPanelOpen("snapshots", true)
    frame.opened = true
    if (view && view !== frame.view) frame.showView(view)
    frame.filter = filter || ""
    settle(scene)
  }

  function closePanel() {
    if (card) card.destroy()
    card = null
    frame = null
    if (widget) widget.destroy()
    widget = null
    if (stripHolder) stripHolder.destroy()
    stripHolder = null
  }

  function init() {
    closePanel()
  }

  function cleanup() {
    closePanel()
    panelWidth = 600
    if (service) service.destroy()
    service = null
    ProcessRegistry.reset()
    wait(0)
  }

  // ---------------------------------------------------------------- the renders

  // The bytes of a file the test wrote (QML_XHR_ALLOW_FILE_READ=1 in the pipeline).
  function fileSize(path) {
    var xhr = new XMLHttpRequest()
    xhr.open("GET", "file://" + path, false)
    xhr.responseType = "arraybuffer"
    try {
      xhr.send()
    } catch (e) {
      return -1
    }
    return xhr.response ? xhr.response.byteLength : -1
  }

  // More than one colour among a grid of samples: something was drawn.
  function drawn(image) {
    var first = String(image.pixel(0, 0))
    for (var x = 0; x < image.width; x += Math.max(1, Math.floor(image.width / 24)))
      for (var y = 0; y < image.height; y += Math.max(1, Math.floor(image.height / 24)))
        if (String(image.pixel(x, y)) !== first) return true
    return false
  }

  // `blank` allows a render with nothing drawn: the stub kit's bar button has no glyph.
  // Every render is also held to the accessibility walk (1n2p.40).
  function save(item, name, blank) {
    wait(60)
    var image = grabImage(item)
    verify(image.width > 0 && image.height > 0, name + ": an empty grab")
    if (!blank) verify(drawn(image), name + ": nothing drawn")
    compare(unnamedControls(item).join("; "), "", name + ": a control a screen reader cannot name")
    if (Fixtures.SNAPSHOT_DIR === "") return image
    var path = Fixtures.SNAPSHOT_DIR + "/" + name + ".png"
    image.save(path)
    verify(fileSize(path) > 0, name + ": " + path + " was not written")
    shots = shots.concat([name])
    return image
  }

  // The panel at the size the bar gives it.
  function shoot(name) {
    wait(30)
    var image = save(card, name)
    compare(image.width, panelWidth + cardPadding * 2, name + ": the panel's width")
    verify(image.height <= panelMaxHeight + cardPadding * 2, name + ": taller than the panel's cap")
    compare(overflowing(frame).join("; "), "", name + ": drawn past the panel's edge or a clip")
  }

  // The visible items drawn past the panel's left or right edge, or past the sides of a
  // clipping ancestor (where their text would be cut) (1n2p.39). Vertical overflow is a
  // scroll area's to show.
  function overflowing(root) {
    var out = []
    var walk = function(item, left, right) {
      for (var i = 0; i < item.children.length; i++) {
        var c = item.children[i]
        if (!c.visible || c.opacity === 0 || c.width <= 0 || c.height <= 0) continue
        // Both edges through the transforms between them (the text zoom scales a canvas).
        var x = c.mapToItem(root, 0, 0).x
        var end = c.mapToItem(root, c.width, 0).x
        if (x < left - 1 || end > right + 1) {
          out.push((c.objectName || String(c).split("(")[0]) + " at " + Math.round(x) + ".." + Math.round(end)
            + " in " + Math.round(left) + ".." + Math.round(right))
        }
        walk(c, c.clip ? Math.max(left, x) : left, c.clip ? Math.min(right, end) : right)
      }
    }
    walk(root, 0, root.width)
    return out
  }

  // The roles a control a player acts on takes: what a screen reader announces as
  // something to press, pick, type into or open (1n2p.40).
  readonly property var controlRoles: [Accessible.Button, Accessible.CheckBox, Accessible.RadioButton,
    Accessible.ComboBox, Accessible.EditableText, Accessible.ListItem, Accessible.PageTab, Accessible.MenuItem,
    Accessible.Link, Accessible.Slider, Accessible.SpinBox, Accessible.ButtonMenu, Accessible.ButtonDropDown]

  // A pointer target: a MouseArea that takes a button (a hover-only one takes none).
  function clickable(item) {
    return item.pressAndHoldInterval !== undefined && item.acceptedButtons !== undefined
      && item.acceptedButtons !== Qt.NoButton
  }

  // A box a player types into.
  function typable(item) {
    return item.cursorPosition !== undefined && item.selectByMouse !== undefined && item.readOnly === false
  }

  // The visible controls of `root` a screen reader cannot announce (1n2p.40). A control
  // is a pointer target or a text box; it is named by the nearest item at or above it
  // that sets an Accessible role, which must be a control's role with a name. An item
  // marked `Accessible.ignored` (a sheet's backdrop, whose way out is Esc) is skipped.
  function unnamedControls(root) {
    var out = []
    var describe = function(item, why) {
      var path = []
      for (var p = item; p && p !== root && path.length < 4; p = p.parent)
        if (p.objectName) path.unshift(p.objectName)
      return (path.join(" > ") || "(unnamed)") + " " + String(item).split("(")[0] + ": " + why
    }
    var check = function(item) {
      for (var p = item; p; p = p.parent) {
        var a = p.Accessible
        if (a.ignored === true) return
        if (a.role === Accessible.NoRole) continue
        if (controlRoles.indexOf(a.role) === -1) out.push(describe(item, "inside a " + a.role + " role, none of its own"))
        else if (String(a.name || "") === "") out.push(describe(item, "role " + a.role + " has no name"))
        return
      }
      out.push(describe(item, "no role"))
    }
    var walk = function(item) {
      for (var i = 0; i < item.children.length; i++) {
        var c = item.children[i]
        if (!c.visible || c.opacity === 0) continue
        if (clickable(c) || typable(c)) check(c)
        walk(c)
      }
    }
    walk(root)
    return out
  }

  // ---------------------------------------------------------------- envelopes

  function statusFrom(name, edit) {
    var env = fixture(name)
    if (edit) edit(env)
    return { "text": text(env) }
  }

  // The `list -v` fixture's first addon as the template of `count` addons over its
  // clients: one in three behind, one pinned, one modified, one a dependency.
  function bigList(count, longName) {
    var env = fixture("list")
    var template = env.data.targets[1].addons[0]
    var lists = []
    for (var t = 0; t < env.data.targets.length; t++) lists.push([])
    for (var i = 0; i < count; i++) {
      var a = JSON.parse(JSON.stringify(template))
      a.name = i === 7 && longName ? longName : "Addon" + ("00" + (i + 1)).slice(-3)
      a.folders = [a.name]
      a.version = "v1." + i
      var behind = i % 3 === 0
      a.latest = { "version": behind ? "v1." + (i + 1) : a.version, "status": behind ? "update_available" : "current",
        "source": "github", "release_id": "gh:o/" + a.name.toLowerCase() + "@v1." + (i + 1), "asset": a.name + ".zip" }
      if (i % 17 === 5) {
        a.pinned = true
        a.latest.status = "pinned"
      }
      if (i % 23 === 4) a.drift = { "state": "modified", "files": 3, "folders": [] }
      if (i % 11 === 2) {
        a.reason = "dependency"
        a.required_by = ["Addon001"]
      }
      lists[i % lists.length].push(a)
    }
    for (var k = 0; k < lists.length; k++) env.data.targets[k].addons = lists[k]
    return { "text": text(env) }
  }

  function longNameList() {
    var env = fixture("list")
    var a = env.data.targets[1].addons[0]
    a.name = "Details_DamageMeter_StreamerModeAndPluginsCollection_Classic"
    a.folders = [a.name]
    return { "text": text(env) }
  }

  // The `list` fixture with every waiting update's versions as long as real release tags
  // (omad-39he), lengthened in place as longNameList lengthens a name: Plater's, and
  // Details', wider than a row at 400 wide with the text zoomed to 2.
  function longVersionsList() {
    var env = fixture("list")
    var tags = [["Plater-v661", "Plater-v662"], ["Details.20251102.13711.165", "Details.20251109.13800.170"]]
    var n = 0
    for (var t = 0; t < env.data.targets.length; t++)
      for (var i = 0; i < env.data.targets[t].addons.length; i++) {
        var a = env.data.targets[t].addons[i]
        if (!a.latest || a.latest.status !== "update_available") continue
        a.version = tags[n % tags.length][0]
        a.latest.version = tags[n % tags.length][1]
        n++
      }
    verify(n >= 2, "the list fixture has " + n + " waiting updates; the scene needs two")
    return { "text": text(env) }
  }

  function upToDate() {
    var env = fixture("list")
    for (var t = 0; t < env.data.targets.length; t++)
      for (var i = 0; i < env.data.targets[t].addons.length; i++) {
        var a = env.data.targets[t].addons[i]
        a.latest = { "version": a.version, "status": "current", "source": a.latest ? a.latest.source : null,
          "release_id": null, "asset": null }
      }
    return { "text": text(env) }
  }

  function emptyList() {
    var env = fixture("list")
    for (var t = 0; t < env.data.targets.length; t++) env.data.targets[t].addons = []
    return { "text": text(env) }
  }

  function manyBackups(count) {
    var env = fixture("backups")
    var template = env.data.targets[0].backups[0]
    var list = []
    for (var i = 0; i < count; i++) {
      var b = JSON.parse(JSON.stringify(template))
      b.id = template.id.slice(0, 20) + ("00000" + i).slice(-6)
      b.kind = i % 4 === 0 ? "manual" : "auto"
      b.trigger = b.kind === "auto" ? (i % 2 ? "post_session" : "pre_change") : null
      b.consistency = i % 7 === 3 ? "best_effort" : "stable"
      list.push(b)
    }
    env.data.targets[0].backups = list
    env.data.targets[0].shown = count
    env.data.targets[0].total = count
    return { "text": text(env) }
  }

  function noBackups() {
    var env = fixture("backups")
    env.data.targets[0].backups = []
    env.data.targets[0].shown = 0
    env.data.targets[0].total = 0
    return { "text": text(env) }
  }

  function manyResults(count) {
    var env = fixture("search")
    var list = []
    for (var i = 0; i < count; i++) {
      var r = JSON.parse(JSON.stringify(env.data.results[i % env.data.results.length]))
      r.name = r.name + (i >= env.data.results.length ? " " + (i + 1) : "")
      list.push(r)
    }
    env.data.results = list
    return { "text": text(env) }
  }

  function noResults() {
    var env = fixture("search")
    env.data.results = []
    return { "text": text(env) }
  }

  function noErrors() {
    var env = fixture("errors")
    for (var t = 0; t < env.data.targets.length; t++) {
      env.data.targets[t].addons = []
      if (env.data.targets[t].summary) {
        env.data.targets[t].summary.new = 0
        env.data.targets[t].summary.entries = 0
      }
    }
    return { "text": text(env) }
  }

  // Updates behind on two clients, Lua errors on retail: the everyday scene.
  function everyday() {
    return { "status": the("status-updates"), "list": the("list"), "errors": the("errors"), "backups": the("backups"),
      "backupsDetail": the("backups-detail"), "changelog": the("changelog"), "history": the("history") }
  }

  // ---------------------------------------------------------------- phases

  function test_phase_starting() {
    start(false)
    show({ }, "updates")
    compare(service.phase, "starting")
    shoot("phase-starting")
  }

  function test_phase_missing_binary() {
    start(true)
    wait(30)
    show({ }, "updates")
    compare(service.phase, "missing")
    shoot("phase-missing")
  }

  function test_phase_unreachable() {
    start(false)
    settle({ "status": { "text": "", "code": 101,
      "stderr": ["thread 'main' panicked at src/app/status.rs:120:5:", "called `Option::unwrap()` on a `None` value"] } })
    show({ }, "updates")
    compare(service.phase, "unreachable")
    compare(service.phaseCard.kind, "malformed")
    shoot("phase-unreachable")
  }

  // A status read past its deadline (1n2p.34): stopped, said in words, retried.
  function test_phase_timeout() {
    start(false, { "localDeadlineMs": 40 })
    tryVerify(function() { return service.phase === "unreachable" }, 2000, "the status read times out")
    show({ }, "updates")
    compare(service.phaseCard.kind, "timeout")
    shoot("phase-timeout")
  }

  // A child that never ends, even after SIGKILL (1n2p.34): let go, the lane freed.
  function test_phase_stuck() {
    start(false, { "localDeadlineMs": 40, "killGraceMs": 30 })
    var launches = pending()
    for (var i = 0; i < launches.length; i++) {
      launches[i].process.ignoreTerm = true
      launches[i].process.ignoreKill = true
    }
    tryVerify(function() { return service.phase === "unreachable" && !!service.phaseCard && service.phaseCard.kind === "stuck" }, 2000,
      "the watchdog lets the status read go")
    // The read the opened panel asks for runs under the usual deadline, unanswered, so
    // the card stays the one the watchdog left.
    service.localDeadlineMs = 600000
    show({ }, "updates")
    compare(service.phaseCard.kind, "stuck")
    shoot("phase-stuck")
  }

  function test_phase_incompatible() {
    var scene = everyday()
    open(scene, "updates")
    service.refresh()
    settle({ "status": statusFrom("status-updates", function(env) { env.tool_version = "0.2.0" }) })
    compare(service.phase, "incompatible")
    compare(service.phaseCard.kind, "version")
    shoot("phase-incompatible")
  }

  // The binary sets `summary.widget_outdated` only with `widget.outdated` (an outdated
  // widget serving this state), which Model.compatible reads as another build: the
  // panel goes read-only, and its card says the panel is out of date.
  function test_phase_widget_outdated() {
    var scene = everyday()
    scene.status = statusFrom("status-updates", function(env) {
      env.data.summary.widget_outdated = true
      env.data.widget = { "omarchy": true, "installed": true, "enabled": true, "version": Build.TOOL_VERSION,
        "outdated": true, "context_matches": true }
    })
    open(scene, "updates")
    compare(service.phase, "incompatible")
    compare(service.phaseCard.kind, "outdated")
    service.reinstallWidget()
    shoot("phase-widget-outdated")
  }

  // A change a crash cut off (1n2p.34): announced above the view, from `status` alone.
  function test_interrupted_change() {
    var scene = everyday()
    scene.status = statusFrom("status-updates", function(env) {
      env.data.journal = { "pending": 1, "in_progress": false }
      env.data.targets[1].attention = ["journal_pending"]
    })
    open(scene, "updates")
    verify(service.interrupted !== null, "the change is announced")
    shoot("interrupted-change")
  }

  // A read whose child the watchdog let go (1n2p.34): the failure card, with Copy details.
  function test_failure_stuck() {
    open(everyday(), "updates")
    service.lastError = { "message": Model.failureWords("Check now", "stuck"), "code": "stuck", "fixes": [], "fixText": "",
      "details": Model.failureDetails({ "label": "Check now", "outcome": "stuck", "argv": ["omaddons", "outdated"], "tail": [] }, {}, "0.1.0") }
    shoot("failure-stuck")
  }

  function test_phase_no_clients() {
    var scene = { "status": statusFrom("status-idle", function(env) {
      env.data.targets = []
      env.data.summary.targets = 0
      env.data.last_change = null
    }), "list": { "text": ok("list", { "targets": [] }) }, "errors": { "text": ok("errors", { "targets": [] }) },
      "backups": { "text": ok("backup list", { "targets": [] }) } }
    open(scene, "updates")
    compare(service.targets.length, 0)
    shoot("phase-no-clients")
  }

  // The welcome card's Add a client folder (1n2p.32): the page with its path box, a
  // relative path refused in words.
  function test_first_run_add_client() {
    var scene = { "status": the("status-none"), "list": { "text": ok("list", { "targets": [] }) },
      "errors": { "text": ok("errors", { "targets": [] }) }, "backups": { "text": ok("backup list", { "targets": [] }) } }
    open(scene, "updates")
    verify(frame.stateAction("add"), "the page opens")
    var field = findChild(frame.current, "pathField")
    verify(field !== null)
    field.text = "Games/World of Warcraft"
    frame.current.add()
    shoot("first-run-add-client")
  }

  // ---------------------------------------------------------------- Updates

  function test_updates_loading() {
    var scene = everyday()
    delete scene.list
    open(scene, "updates")
    shoot("updates-loading")
  }

  function test_updates_empty() {
    var scene = everyday()
    scene.status = the("status-idle")
    scene.list = upToDate()
    open(scene, "updates")
    shoot("updates-empty")
  }

  function test_updates_short() {
    open(everyday(), "updates")
    shoot("updates-short")
  }

  // After an undo (RG-36): the release it left, skipped, its row cursored with Offer again.
  function test_updates_skipped() {
    var scene = { "status": the("skip-undone-status"), "list": the("skip-undone-list"), "errors": the("errors"),
      "backups": the("backups"), "history": the("skip-history") }
    open(scene, "updates")
    compare(frame.currentView.rows.map(function(r) { return r.kind }), ["skipped"])
    frame.cursor = 0
    frame.cursorActive = true
    shoot("updates-skipped")
  }

  // Offer again's plan sheet (RG-36): the release offered again, named.
  function test_confirm_offer_again() {
    var scene = { "status": the("skip-undone-status"), "list": the("skip-undone-list"), "errors": the("errors"),
      "backups": the("backups"), "history": the("skip-history") }
    open(scene, "updates")
    scene.unskip = the("skip-unskip-preview")
    var offer = Model.rowAction(frame.currentView.rows[0], "skip")
    compare(service.run(offer.template, offer.slots, offer.label), "")
    settle(scene)
    verify(service.confirm !== null, "Offer again's plan sheet")
    shoot("confirm-offer-again")
  }

  // The frame (design doc §2.1) at the minimum width: glyph tabs, the view named in the
  // content, one part of the meta line, no detail.
  function test_frame_at_400() {
    panelWidth = 400
    open(everyday(), "updates")
    verify(frame.narrow)
    shoot("frame-400-updates")
  }

  function test_frame_issues_at_400() {
    panelWidth = 400
    var scene = everyday()
    scene.status = the("status-attention")
    open(scene, "issues")
    shoot("frame-400-issues")
  }

  // One client in scope: its name in the scope Dropdown, its own header with Update
  // Retail where All shows it; the hero says "omaddons" alone (omad-224l).
  function test_frame_one_client() {
    open(everyday(), "updates", "retail")
    compare(findChild(frame, "hero").title, "omaddons")
    compare(findChild(frame, "scope").currentLabel(), "Retail")
    shoot("frame-540-one-client")
  }

  // The text zoom (Ctrl+=, three steps): every glyph and gap 1.3 times, in the same panel.
  function test_frame_text_zoom() {
    open(everyday(), "updates")
    frame.uiScale = 1.3
    settle(everyday())
    shoot("frame-540-text-zoom")
  }

  function test_updates_one_client_running() {
    var scene = everyday()
    scene.status = the("status-running")
    open(scene, "updates", "retail")
    shoot("updates-filter-running")
  }

  // The names of a view's rows of one client and kind, in the order shown.
  function namesOf(rows, target, kind) {
    return rows.filter(function(r) { return r.target === target && (!kind || r.kind === kind) })
      .map(function(r) { return r.name })
  }

  function test_updates_long() {
    var scene = everyday()
    scene.list = bigList(150, "")
    open(scene, "updates")
    // Under Qt's own sort (omad-1tnk): each client's updates in the order list -v gave.
    var era = namesOf(frame.current.rows, "era", "update")
    verify(era.length > 20, "era's updates: " + era.length)
    compare(era, era.slice().sort(), "era's updates in name order")
    shoot("updates-long")
  }

  function test_updates_long_name() {
    var scene = everyday()
    scene.list = longNameList()
    open(scene, "updates")
    shoot("updates-long-name")
  }

  function test_updates_release_notes() {
    open(everyday(), "updates")
    frame.cursor = 0
    frame.cursorActive = true
    frame.textKey("e")
    settle(everyday())
    shoot("updates-release-notes")
  }

  // Check now (UX-01, 1n2p.18): the activity bar says so at once, then counts what the
  // CLI reports, with Cancel.
  function test_updates_checking() {
    open(everyday(), "updates")
    frame.textKey("c")
    settle({ })
    shoot("updates-checking")
    var check = null
    var running = pending()
    for (var i = 0; i < running.length; i++) if (words(running[i].command)[0] === "outdated") check = running[i]
    verify(check !== null, "the check runs")
    check.process.emitStderr(JSON.stringify({ "event": "progress", "phase": "fetch", "message": "gh:o/foo", "current": 12,
      "total": 42, "target": null, "ref": "gh:o/foo" }))
    wait(20)
    shoot("updates-checking-counted")
  }

  // The key reference (`?`) over Updates.
  function test_help_reference() {
    open(everyday(), "updates")
    frame.textKey("?")
    settle(everyday())
    shoot("help-updates")
  }

  // ---------------------------------------------------------------- Installed

  function test_installed_loading() {
    var scene = everyday()
    delete scene.list
    open(scene, "installed")
    shoot("installed-loading")
  }

  function test_installed_empty() {
    var scene = everyday()
    scene.list = emptyList()
    open(scene, "installed")
    shoot("installed-empty")
  }

  function test_installed_short() {
    var scene = everyday()
    scene.list = the("list-markers")
    open(scene, "installed")
    shoot("installed-short")
  }

  function test_installed_long() {
    var scene = everyday()
    scene.list = bigList(150, "Details_DamageMeter_StreamerModeAndPluginsCollection_Classic")
    open(scene, "installed")
    shoot("installed-long")
  }

  function test_installed_long_name() {
    var scene = everyday()
    scene.list = longNameList()
    open(scene, "installed")
    shoot("installed-long-name")
  }

  // The rebuilt Addons view (1n2p.22) at the minimum width: a row keeps its first chip
  // and its cursored row one action (design doc §5.3).
  function test_installed_long_at_400() {
    panelWidth = 400
    var scene = everyday()
    scene.list = bigList(150, "Details_DamageMeter_StreamerModeAndPluginsCollection_Classic")
    open(scene, "installed")
    frame.cursor = 0
    frame.cursorActive = true
    shoot("installed-long-400")
  }

  // One client: problems first, then waiting updates, a dependency under the addon that
  // needs it, the cursored row's buttons.
  function test_installed_one_client() {
    var scene = everyday()
    scene.list = bigList(150, "")
    open(scene, "installed", "retail")
    frame.cursor = 0
    frame.cursorActive = true
    shoot("installed-one-client")
  }

  // `/` with a word typed: the box, "n of m", what it matches.
  function test_installed_filter() {
    var scene = everyday()
    scene.list = bigList(150, "")
    open(scene, "installed")
    var installed = frame.currentView
    installed.handleKey("filter", 0)
    findChild(installed, "filterField").text = "addon01"
    compare(findChild(installed, "filterCount").text, installed.rows.length + " of 150")
    shoot("installed-filter")
    findChild(installed, "filterField").text = "nothing like it"
    shoot("installed-filter-none")
    installed.escapeFilter()
  }

  // ---------------------------------------------------------------- Issues

  function test_issues_loading() {
    var scene = everyday()
    delete scene.errors
    open(scene, "issues")
    shoot("issues-loading")
  }

  function test_issues_empty() {
    var scene = everyday()
    scene.status = the("status-idle")
    scene.errors = noErrors()
    open(scene, "issues")
    shoot("issues-empty")
  }

  function test_issues_problem() {
    var scene = everyday()
    scene.status = the("status-attention")
    open(scene, "issues")
    shoot("issues-problem")
  }

  // ---------------------------------------------------------------- Errors (1n2p.27)

  function test_errors_lua_errors() {
    var scene = everyday()
    scene.status = the("status-errors")
    open(scene, "errors")
    frame.cursor = 0
    frame.cursorActive = true
    shoot("errors-lua-errors")
  }

  // `y` on a row: the copy's flash in the corner toast, on the footer row (omad-92d5).
  function test_errors_copy_flash() {
    var scene = everyday()
    scene.status = the("status-errors")
    open(scene, "errors")
    frame.cursor = 0
    frame.cursorActive = true
    verify(frame.flash("Copied the report on Foo", true))
    tryCompare(findChild(frame, "toast"), "opacity", 1, 1000)
    shoot("errors-copy-flash")
  }

  function test_errors_partial_read() {
    var scene = everyday()
    scene.status = the("status-errors-partial")
    scene.errors = the("errors-partial")
    open(scene, "errors")
    shoot("errors-partial-read")
  }

  // Enter on Foo: its page, every error and its stack (§2.4).
  function test_errors_page() {
    var scene = everyday()
    scene.status = the("status-errors")
    open(scene, "errors")
    var errors = frame.currentView
    verify(errors.rows.length > 0)
    errors.openPage(errors.rows[0])
    shoot("errors-page")
  }

  // RG-65, after an update broke something (journey J7's scene, from the real binary):
  // Foo's row says what it came after; its page says what changed before the error, how
  // strongly, and offers Go back; Go back's plan sheet.
  function rollbackScene() {
    var scene = everyday()
    scene.status = the("rollback-status")
    scene.errors = the("rollback-errors")
    scene.list = the("rollback-list")
    return scene
  }

  function test_errors_after_change() {
    open(rollbackScene(), "errors")
    verify(frame.currentView.rows[0].chips.some(function(c) { return c.text === Model.copy("errors.tag.update") }), "the row's tag")
    frame.cursor = 0
    frame.cursorActive = true
    shoot("errors-after-change")
  }

  // Foo's page, the linked error's section in view, its Go back cursored.
  function whatChanged(scene) {
    open(scene, "errors")
    var errors = frame.currentView
    errors.openPage(errors.rows[0])
    var page = findChild(frame, "page").item
    var list = findChild(page, "errors")
    for (var i = 0; i < list.count; i++) {
      var section = findChild(list.itemAt(i), "change")
      if (section && section.visible) return page
    }
    fail("no linked error's section")
  }

  function test_errors_page_what_changed() {
    whatChanged(rollbackScene())
    shoot("errors-page-what-changed")
  }

  function test_errors_page_what_changed_at_400() {
    panelWidth = 400
    whatChanged(rollbackScene())
    shoot("errors-page-what-changed-400")
  }

  function test_confirm_go_back() {
    var scene = rollbackScene()
    var page = whatChanged(scene)
    scene.skip = the("rollback-preview")
    verify(page.handleKey("enter", 0), "Enter runs the first way back")
    settle(scene)
    verify(service.confirm !== null, "Go back's plan sheet")
    compare(service.confirm.sheet.confirmLabel, Model.copy("sheet.confirm.rollback.one"))
    shoot("confirm-go-back")
  }

  function test_issues_long() {
    var scene = everyday()
    scene.status = statusFrom("status-attention", function(env) {
      var target = env.data.targets[0]
      var issue = target.issues[0]
      var many = []
      for (var i = 0; i < 24; i++) {
        var copy = JSON.parse(JSON.stringify(issue))
        copy.addon = "Addon" + ("00" + (i + 1)).slice(-3)
        copy.title = copy.addon + "'s files changed on disk (" + (i + 1) + " files)"
        many.push(copy)
      }
      target.issues = many
      env.data.summary.issues.p2 = many.length
    })
    open(scene, "issues")
    // Under Qt's own sort (omad-1tnk): the problems in the order status ranked them.
    var titles = Model.issueRows(Model.issueSections(service.targets, [], "", service.machineIssues))
      .filter(function(r) { return r.kind === "problem" }).map(function(r) { return r.title })
    compare(titles.length, 24)
    for (var i = 0; i < titles.length; i++)
      verify(titles[i].indexOf("Addon" + ("00" + (i + 1)).slice(-3) + "'s") === 0, "problem " + i + ": " + titles[i])
    shoot("issues-long")
  }

  // A long problem and a long client name (1n2p.28): the sentence wraps, never elides.
  function test_issues_long_name() {
    var scene = everyday()
    scene.status = statusFrom("status-attention", function(env) {
      var target = env.data.targets[0]
      target.display_name = "Details_DamageMeter_StreamerModeAndPluginsCollection_Classic's own client"
      target.issues[0].addon = "Details_DamageMeter_StreamerModeAndPluginsCollection_Classic"
      target.issues[0].title = target.issues[0].addon + "'s folder is gone, and so are the 14 folders of its plugins"
      // Its fix names it too (the button cuts it), as does status's recommendation of
      // the same reinstall, which the row's fix stands for.
      var argv = target.issues[0].remediation.argv
      argv[argv.indexOf("Baz")] = target.issues[0].addon
      target.issues[0].remediation.command = argv.join(" ")
      env.commands[0].argv = argv.slice()
      env.commands[0].command = argv.join(" ")
    })
    open(scene, "issues")
    shoot("issues-long-name")
  }

  // Nothing needs fixing, and what `status` suggests (1n2p.28): take over, check, back up.
  function test_issues_suggestions() {
    var scene = everyday()
    scene.status = the("status-check-due")
    open(scene, "issues")
    shoot("issues-suggestions")
  }

  // A forced repair (1n2p.28, design doc §6.5.4): Explain and Copy command, never a fix.
  function test_issues_terminal_only() {
    var scene = everyday()
    scene.status = the("setaside-status")
    open(scene, "issues")
    frame.cursor = 0
    frame.cursorActive = true
    shoot("issues-terminal-only")
  }

  // The problem's page: the doctor's explanation, then the command for a terminal.
  function test_issues_page() {
    var scene = everyday()
    scene.status = the("setaside-status")
    scene.doctor = the("setaside-explain")
    open(scene, "issues")
    var issues = frame.currentView
    verify(issues.rows.length > 0)
    issues.openPage(issues.rows[0])
    settle(scene)
    shoot("issues-page")
  }

  // A machine problem the doctor repairs, under "This computer".
  function test_issues_repair() {
    var scene = everyday()
    scene.status = the("repair-status")
    open(scene, "issues")
    shoot("issues-repair")
  }

  // ---------------------------------------------------------------- the addon page (1n2p.23)

  // Foo's page from Addons: its two clients, what its read brought (its notes, why its
  // source said nothing more), its folders and its Lua errors on Retail.
  function test_addon_page() {
    var scene = everyday()
    open(scene, "installed")
    frame.openAddon("Foo", "gh:o/foo")
    settle(scene)
    var page = findChild(frame, "page").item
    compare(page.page.state, "")
    verify(page.page.notes.length > 0, "its release notes")
    frame.cursor = 1
    frame.cursorActive = true
    shoot("addon-page")
  }

  // Its Folders (omad-z3ur): Foo_Config gone from Retail's disk, beside the alert glyph,
  // and the two clients sharing one AddOns folder, as status's typed warning names them;
  // the older release note folded under the newest; and LibNowhere, which no client has,
  // with Install it on the cursored Retail line's client.
  function test_addon_page_folders() {
    var scene = everyday()
    var list = fixture("list")
    list.data.targets.forEach(function(t) {
      t.addons.forEach(function(a) {
        if (a.name !== "Foo" || t.id !== "retail") return
        a.folders = ["Foo", "Foo_Config"]
        a.missing_folders = ["Foo_Config"]
      })
    })
    scene.list = { "text": text(list), "code": 0 }
    var status = fixture("status-updates")
    status.warnings = [{ "code": "target_addons_shared", "message": "era and retail share one AddOns directory",
      "fix": "manage these addons from one client", "fix_argv": null, "targets": ["era", "retail"] }]
    scene.status = { "text": text(status), "code": 0 }
    var changelog = fixture("changelog")
    changelog.data.changelog.entries.push({ "version": "v1.1", "published_at": changelog.data.changelog.entries[0].published_at,
      "text": "Settings\n- a panel for the pull timer\n- fewer chat messages" })
    changelog.data.installed.forEach(function(r) { if (r.toc) r.toc.dependencies = ["LibNowhere"] })
    scene.changelog = { "text": text(changelog), "code": 0 }
    open(scene, "installed")
    frame.openAddon("Foo", "gh:o/foo")
    settle(scene)
    var page = findChild(frame, "page").item
    verify(page.page.folders.some(function(f) { return f.missingText !== "" }), "a folder gone from disk")
    verify(page.page.folders.some(function(f) { return f.sharedText !== "" }), "a shared AddOns folder")
    compare(JSON.stringify(page.page.notes.map(function(n) { return n.folded })), JSON.stringify([false, true]))
    frame.cursor = page.rows.map(function(l) { return l.target }).indexOf("retail")
    frame.cursorActive = true
    compare(findChild(page, "installDependency_LibNowhere").text, "Install it on Retail")
    frame.showItem(findChild(page, "installDependency_LibNowhere"))
    shoot("addon-page-folders")
  }

  // Its Versions on Retail (1n2p.24), over the binary's Versions scene: following beta,
  // the channels with their newest, and each release with its date, channel and how
  // likely it loads, the installed one marked; the cursor on the older stable release.
  function test_addon_page_versions() {
    var scene = { "status": the("versions-status"), "list": the("versions-list-beta"), "errors": { "text": ok("errors", { "targets": [] }) },
      "backups": { "text": ok("backup list", { "targets": [] }) }, "changelog": the("changelog"), "history": the("history-empty"),
      "releases": the("versions-releases") }
    open(scene, "installed")
    frame.openAddon("Foo", "gh:o/foo")
    settle(scene)
    var page = findChild(frame, "page").item
    frame.cursor = 0
    frame.cursorActive = true
    verify(page.handleKey("versions", 0))
    settle(scene)
    compare(page.page.versions.state, "")
    compare(page.page.versions.lines.length, 6)
    frame.cursor = 5
    shoot("addon-page-versions")
  }

  // A source that serves only its newest release says so; the channels stay.
  function test_addon_page_versions_latest_only() {
    var releases = fixture("versions-releases")
    releases.data.releases = null
    releases.data.releases_unavailable = { "reason": "source_lists_latest_only" }
    var scene = { "status": the("versions-status"), "list": the("versions-list-beta"), "errors": { "text": ok("errors", { "targets": [] }) },
      "backups": { "text": ok("backup list", { "targets": [] }) }, "changelog": the("changelog"), "history": the("history-empty"),
      "releases": { "text": text(releases) } }
    open(scene, "installed")
    frame.openAddon("Foo", "gh:o/foo")
    settle(scene)
    var page = findChild(frame, "page").item
    frame.cursor = 0
    frame.cursorActive = true
    verify(page.handleKey("versions", 0))
    settle(scene)
    compare(findChild(page, "versionsState").text, "Its source offers only its newest release.")
    shoot("addon-page-versions-latest")
  }

  // Its read still running: the local facts, and "Reading about it…".
  function test_addon_page_reading() {
    var scene = everyday()
    delete scene.changelog
    open(scene, "installed")
    frame.openAddon("Foo", "gh:o/foo")
    settle(scene)
    compare(findChild(frame, "page").item.page.state, "reading")
    shoot("addon-page-reading")
  }

  // ---------------------------------------------------------------- Backups

  function test_backups_loading() {
    var scene = everyday()
    delete scene.backups
    open(scene, "backups")
    shoot("backups-loading")
  }

  function test_backups_empty() {
    var scene = everyday()
    scene.backups = noBackups()
    open(scene, "backups")
    shoot("backups-empty")
  }

  function test_backups_short() {
    open(everyday(), "backups")
    shoot("backups-short")
  }

  function test_backups_long() {
    var scene = everyday()
    scene.backups = manyBackups(50)
    open(scene, "backups")
    shoot("backups-long")
  }

  // A backup's page (1n2p.29): when, size, the addons whose settings it holds, the cursor
  // on the first with its Enter hint.
  function test_backup_page() {
    var scene = everyday()
    open(scene, "backups")
    var b = fixture("backups").data.targets[0].backups[0]
    verify(frame.openBackup("retail", b.id, "backup"))
    settle(scene)
    frame.moveCursor(0)
    shoot("backup-page")
  }

  // 1n2p.40: no keyboard trap. Esc leaves every sheet and every page the panel opens on
  // the real service: in at most two presses (a box with the keys gives them back
  // first), never closing the panel while a layer is still open, and the keys come back
  // to the panel's own catcher.
  function test_esc_leaves_every_sheet_and_page() {
    var pagesGone = function() { return frame.pages.length === 0 }
    var layers = [
      { "name": "key reference", "view": "updates", "open": function() { frame.textKey("?") },
        "gone": function() { return !frame.helpOpen } },
      { "name": "settings", "view": "updates", "open": function() { frame.settingsOpen = true },
        "gone": function() { return !frame.settingsOpen } },
      { "name": "restore chooser", "view": "installed",
        "open": function() { verify(frame.openChooser({ "target": "retail", "addon": "Foo" })) },
        "gone": function() { return !findChild(frame, "chooser").opened } },
      { "name": "addon page", "view": "installed", "open": function() { frame.openAddon("Foo", "gh:o/foo") }, "gone": pagesGone },
      { "name": "backup page", "view": "backups",
        "open": function() { verify(frame.openBackup("retail", fixture("backups").data.targets[0].backups[0].id, "backup")) },
        "gone": pagesGone },
      { "name": "change page", "view": "history", "open": function() {
          var rows = frame.currentView.rows.filter(function(r) { return r.kind === "update" })
          frame.openChange(rows[0].key, rows[0].title)
        }, "gone": pagesGone },
      { "name": "errors page", "view": "errors", "status": "status-errors",
        "open": function() { frame.currentView.openPage(frame.currentView.rows[0]) }, "gone": pagesGone },
      { "name": "problem page", "view": "issues", "status": "setaside-status",
        "open": function() { frame.currentView.openPage(frame.currentView.rows[0]) }, "gone": pagesGone },
      // No client yet (with one, the page goes back as the scan lists it). Its path box
      // has the keys: the first Esc leaves the box, the second the page.
      { "name": "add a client", "view": "updates", "first": true, "open": function() { verify(frame.stateAction("add")) },
        "gone": pagesGone },
      { "name": "plan sheet", "view": "installed", "list": "plan-list", "open": function(scene) {
          scene.remove = the("plan-remove")
          compare(service.run("remove", { "target": "retail", "addon": "Bar" }, "Remove Bar"), "")
        }, "gone": function() { return service.confirm === null } }
    ]
    for (var i = 0; i < layers.length; i++) {
      var layer = layers[i]
      cleanup()
      var scene = everyday()
      scene.config = the("config")
      scene.doctor = the("setaside-explain")
      if (layer.status) scene.status = the(layer.status)
      if (layer.list) scene.list = the(layer.list)
      if (layer.first) {
        scene = { "status": the("status-none"), "list": { "text": ok("list", { "targets": [] }) },
          "errors": { "text": ok("errors", { "targets": [] }) }, "backups": { "text": ok("backup list", { "targets": [] }) } }
      }
      open(scene, layer.view)
      var closed = 0
      frame.closeRequested.connect(function() { closed++ })
      layer.open(scene)
      settle(scene)
      verify(!layer.gone(), layer.name + ": open")
      var presses = 0
      while (!layer.gone() && presses < 2) {
        frame.focusItem.forceActiveFocus()
        keyClick(Qt.Key_Escape)
        settle(scene)
        presses++
      }
      verify(layer.gone(), layer.name + ": Esc leaves it")
      compare(closed, 0, layer.name + ": the panel stays open")
      tryVerify(function() { return frame.focusItem.activeFocus }, 1000, layer.name + ": the keys come back")
    }
  }

  // 1n2p.39: every view and the settings sheet at both widths and the text zooms,
  // nothing drawn past the panel's edge or a clip (shoot's check), on whichever kit and
  // theme the run draws with; 1n2p.40 adds the range's ends, 0.6 and 2 (§5.6), where
  // every control must still be reachable (and named: save's walk). The long-versions
  // states are their view with release tags as long as real ones (omad-39he).
  function test_matrix_widths_and_text_zooms() {
    var states = ["updates", "installed", "search", "errors", "issues", "backups", "history", "settings",
      "updates-long-versions", "installed-long-versions"]
    // A state's view, where its name is not one.
    var viewOf = { "settings": "updates", "updates-long-versions": "updates", "installed-long-versions": "installed" }
    for (var w of [600, 400]) {
      for (var z of [0.6, 0.8, 1.0, 1.5, 2.0]) {
        for (var s of states) {
          cleanup()
          panelWidth = w
          var scene = everyday()
          scene.config = the("config")
          if (s.endsWith("-long-versions")) scene.list = longVersionsList()
          open(scene, viewOf[s] || s)
          frame.uiScale = z
          if (s === "settings") {
            frame.settingsOpen = true
            settle(scene)
          }
          shoot("matrix-" + s + "-" + w + "-x" + Math.round(z * 10))
        }
      }
    }
  }

  // The settings sheet (1n2p.31), the cursor on a switch with what it does, and
  // omaddons' settings as the binary printed them.
  function test_settings_sheet() {
    var scene = everyday()
    scene.config = the("config")
    open(scene, "updates")
    frame.settingsOpen = true
    settle(scene)
    var sheet = findChild(frame, "settings")
    sheet.choice = 2
    shoot("settings")
  }

  // The cursor on an omaddons setting.
  function test_settings_sheet_omaddons() {
    var scene = everyday()
    scene.config = the("config")
    open(scene, "updates")
    frame.settingsOpen = true
    settle(scene)
    var sheet = findChild(frame, "settings")
    sheet.choice = 9
    shoot("settings-omaddons")
  }

  // The chooser over a backup's page: one addon's settings or all of them.
  function test_restore_chooser_over_a_backup_page() {
    var scene = everyday()
    open(scene, "backups")
    var b = fixture("backups").data.targets[0].backups[0]
    verify(frame.openBackup("retail", b.id, "backup"))
    settle(scene)
    verify(frame.openChooser({ "target": "retail", "backup": b.id, "keepDetail": true }))
    shoot("chooser-backup-page")
  }

  // ---------------------------------------------------------------- History (1n2p.30)

  function test_history_loading() {
    var scene = everyday()
    delete scene.history
    open(scene, "history")
    shoot("history-loading")
  }

  function test_history_empty() {
    var scene = everyday()
    scene.history = the("history-empty")
    open(scene, "history")
    shoot("history-empty")
  }

  // Every kind of row the scene has: a restore, removals (one undone), an undo, a
  // backup, a settings change, an update of two clients, installs; the cursor on one.
  function test_history_short() {
    open(everyday(), "history")
    var history = frame.currentView
    verify(history.rows.length > 5, "the scene's changes are listed")
    history.select(1)
    shoot("history-short")
  }

  // Enter on the update of two clients: its page, each client's addons, and its undos.
  function test_history_change_page() {
    open(everyday(), "history")
    var history = frame.currentView
    var rows = history.rows.filter(function(r) { return r.kind === "update" })
    verify(rows.length === 1)
    frame.openChange(rows[0].key, rows[0].title)
    shoot("history-change-page")
  }

  // ---------------------------------------------------------------- Search

  function test_search_empty_box() {
    open(everyday(), "search")
    shoot("search-empty-box")
  }

  // The query typed into the view's field and searched at once, as Enter does (the
  // pause would search it too).
  function searchFor(query, scene) {
    var view = frame.current
    var field = findChild(view, "searchField")
    verify(field !== null, "the search field")
    field.text = query
    view.searchQuery(query)
    settle(scene)
  }

  function test_search_searching() {
    open(everyday(), "search")
    searchFor("weakauras", { })
    shoot("search-searching")
  }

  function test_search_results() {
    var scene = everyday()
    open(scene, "search")
    scene.search = the("search")
    searchFor("weakauras", scene)
    shoot("search-results")
  }

  function test_search_long() {
    var scene = everyday()
    open(scene, "search")
    scene.search = manyResults(20)
    searchFor("bars", scene)
    shoot("search-long")
  }

  function test_search_long_name() {
    var scene = everyday()
    open(scene, "search")
    var env = fixture("search")
    env.data.results[0].name = "Details! Damage Meter: Streamer Mode and Plugins Collection (Classic)"
    scene.search = { "text": text(env) }
    searchFor("details", scene)
    shoot("search-long-name")
  }

  // Enter on a result under All: the client chooser, none chosen yet.
  function test_search_install_chooser() {
    var scene = everyday()
    open(scene, "search")
    scene.search = the("search")
    searchFor("weakauras", scene)
    verify(frame.current.rows.length > 0, "results to choose from")
    frame.current.install(frame.current.rows[0])
    shoot("search-install-chooser")
  }

  // Under All, two clients: what fits each in words, the catalog's mark, and a hit no
  // client has evidence for (1n2p.25).
  function test_search_all_clients() {
    var scene = everyday()
    open(scene, "search")
    scene.search = the("search-all")
    searchFor("questie", scene)
    verify(frame.current.rows.length === 2, "Questie and QuestieExtras")
    shoot("search-all")
  }

  // An empty box: the built-in catalog for the scope, what fits first (1n2p.26).
  function test_search_browse() {
    var scene = everyday()
    scene.catalog = the("search-catalog")
    open(scene, "search")
    settle(scene)
    verify(frame.current.rows.length > 10, "the catalog's rows")
    shoot("search-browse")
  }

  // A pasted link: one row, no search (1n2p.25).
  function test_search_link() {
    open(everyday(), "search")
    findChild(frame.current, "searchField").text = "https://github.com/WeakAuras/WeakAuras2"
    verify(frame.current.rows.length === 1, "the link's row")
    shoot("search-link")
  }

  function test_search_no_results() {
    var scene = everyday()
    open(scene, "search")
    scene.search = noResults()
    searchFor("zzzz", scene)
    shoot("search-no-results")
  }

  // ---------------------------------------------------------------- dialogs

  // `x` on Bar: the removal's preview (a stored plan), then the dialog over the panel.
  function test_confirm_remove() {
    var scene = everyday()
    scene.list = the("plan-list")
    open(scene, "installed")
    scene.remove = the("plan-remove")
    compare(service.run("remove", { "target": "retail", "addon": "Bar" }, "Remove Bar"), "")
    settle(scene)
    verify(service.confirm !== null, "the removal's dialog")
    shoot("confirm-remove")
  }

  // The same plan sheet at the panel's narrowest.
  function test_confirm_remove_at_400() {
    panelWidth = 400
    var scene = everyday()
    scene.list = the("plan-list")
    open(scene, "installed")
    scene.remove = the("plan-remove")
    compare(service.run("remove", { "target": "retail", "addon": "Bar" }, "Remove Bar"), "")
    settle(scene)
    verify(service.confirm !== null, "the removal's plan sheet")
    shoot("confirm-remove-400")
  }

  // Update all whose rows the view did not show: the plan sheet lists every update,
  // each client's under its name, at 540 and at 400.
  function updateOnTwoClients(width, name) {
    panelWidth = width
    var scene = everyday()
    scene.list = the("plan-list-after")
    open(scene, "updates")
    scene.update = the("plan-update-all")
    compare(service.run("update_all", {}, "Update all", []), "")
    settle(scene)
    verify(service.confirm !== null, "the update's plan sheet")
    shoot(name)
  }

  function test_confirm_update_on_two_clients() { updateOnTwoClients(540, "confirm-update-two-clients") }
  function test_confirm_update_on_two_clients_at_400() { updateOnTwoClients(400, "confirm-update-two-clients-400") }

  // A pasted addon list (1n2p.37) on every client, from Addons: its plan sheet groups
  // each client's Conflicts, New and Unchanged under the client's name, at 540 and 400.
  function pastedList(width, name) {
    panelWidth = width
    var scene = everyday()
    scene.status = the("share-status")
    scene.list = the("share-list")
    open(scene, "installed")
    scene.sync = the("share-sync-preview")
    var list = JSON.parse(Fixtures.text("share-export-retail")).data.content
    compare(service.run("sync_all", {}, "Use a pasted addon list", [], list), "")
    settle(scene)
    verify(service.confirm !== null, "the pasted list's plan sheet")
    shoot(name)
  }

  function test_confirm_pasted_list() { pastedList(540, "confirm-pasted-list") }
  function test_confirm_pasted_list_at_400() { pastedList(400, "confirm-pasted-list-400") }

  // `u` on Foo's update whose release moved since the list was read: the dialog says
  // what changed instead of applying.
  function test_confirm_changed_since_you_looked() {
    var scene = everyday()
    scene.list = the("plan-moved-list")
    open(scene, "updates", "retail")
    var rows = Model.updateRows(service.lists, "retail", service.targets)
    var at = -1
    for (var i = 0; i < rows.length; i++) if (rows[i].name === "Foo") at = i
    verify(at >= 0, "Foo's row on retail")
    frame.cursor = at
    frame.cursorActive = true
    scene.update = the("plan-update-moved")
    frame.act("u")
    settle(scene)
    verify(service.confirm !== null, "the changed-since dialog")
    shoot("confirm-changed")
  }

  function test_confirm_restore() {
    var scene = everyday()
    open(scene, "backups")
    var preview = fixture("restore-preview")
    scene.backup = { "text": text(preview) }
    var id = preview.data.backup ? preview.data.backup.id : "01M6Q2VA00000000000000000A"
    compare(service.run("restore", { "target": "retail", "backup": id }, "Restore WTF/"), "")
    settle(scene)
    verify(service.confirm !== null, "the restore's dialog")
    shoot("confirm-restore")
  }

  function test_restore_chooser_for_an_addon() {
    var scene = everyday()
    open(scene, "installed")
    verify(frame.openChooser({ "target": "retail", "addon": "Foo" }))
    settle(scene)
    shoot("chooser-addon")
  }

  function test_restore_chooser_for_a_backup() {
    var scene = everyday()
    open(scene, "backups")
    var backups = fixture("backups").data.targets[0].backups
    verify(frame.openChooser({ "target": "retail", "backup": backups[0].id }))
    settle(scene)
    shoot("chooser-backup")
  }

  // ---------------------------------------------------------------- a change

  // Update all, previewed and applied (every row matches, so no dialog): the apply left
  // running.
  function runningUpdate(scene) {
    scene.update = the("plan-update-all")
    frame.textKey("U")
    settle(scene)
    var apply = null
    var running = pending()
    for (var i = 0; i < running.length; i++) if (words(running[i].command)[0] === "apply") apply = running[i]
    return apply
  }

  function test_action_running_with_progress() {
    var scene = everyday()
    scene.list = the("plan-list-after")
    open(scene, "updates")
    var apply = runningUpdate(scene)
    verify(apply !== null, "the update's apply runs")
    // As the binary writes it: what the event is about, typed (1n2p.18), so Bar on
    // Classic Era shows its own phase.
    apply.process.emitStderr(JSON.stringify({ "event": "progress", "phase": "download", "message": "gh:o/bar@v2 on era",
      "current": 1, "total": 2, "target": "era", "ref": "gh:o/bar" }))
    wait(20)
    shoot("action-progress")
    widgetOn(service)
    saveBar("bar-spinning")
  }

  // Update all over 150 addons: the progress line is at the end of the one scrolling
  // column, below the fold.
  function test_action_progress_below_a_long_list() {
    var scene = everyday()
    scene.list = bigList(150, "")
    open(scene, "updates")
    scene.update = { "text": text(fixture("plan-update-all")) }
    frame.textKey("U")
    settle(scene)
    if (service.confirm) service.confirmYes()
    settle(scene)
    verify(service.action !== null, "a change runs: " + JSON.stringify(service.confirm))
    shoot("action-progress-long")
  }

  function test_action_result_with_undo() {
    var scene = everyday()
    scene.list = the("plan-list-after")
    open(scene, "updates")
    var apply = runningUpdate(scene)
    verify(apply !== null, "the update's apply runs")
    scene.apply = the("plan-update-all-apply")
    scene.status = the("status-after-update")
    var after = fixture("plan-list-after")
    for (var t = 0; t < after.data.targets.length; t++)
      for (var i = 0; i < after.data.targets[t].addons.length; i++) {
        var a = after.data.targets[t].addons[i]
        if (a.latest && a.latest.status === "update_available") {
          a.version = a.latest.version
          a.latest.status = "current"
        }
      }
    scene.list = { "text": text(after) }
    settle(scene)
    verify(service.lastResult !== null, "a result")
    // The result in the corner toast, faded in (omad-92d5).
    tryCompare(findChild(frame, "toast"), "opacity", 1, 1000)
    shoot("action-result-undo")
  }

  function test_action_error_with_a_fix() {
    var scene = everyday()
    open(scene, "updates")
    scene.update = the("failure", 3)
    compare(service.runCliArgv(["omaddons", "update", "-t", "nosuch"], "Update"), "")
    settle(scene)
    verify(service.lastError !== null, "an error card")
    shoot("action-error-fix")
  }

  // ---------------------------------------------------------------- the components (§5)

  function shootGallery(component, width, name) {
    var item = component.createObject(testCase, { "panelWidth": width })
    verify(item !== null)
    var image = save(item, name)
    compare(image.width, width + cardPadding * 2, name + ": the gallery's width")
    item.destroy()
  }

  // One view's cells of the state matrix (design doc §7): first load, failed, empty,
  // filtered empty, refreshing, and each notice the view can show, from synthetic service
  // fields as the service sets them.
  function stateCells(view) {
    var resource = Model.VIEW_RESOURCES[view]
    var targets = [
      { "id": "retail", "key": "k-retail", "displayName": "Retail", "updates": { "checkedAt": "2100-01-01T00:00:00Z", "stale": true },
        "backups": { "stale": true } },
      { "id": "era", "key": "k-era", "displayName": "Classic Era", "updates": { "checkedAt": "2100-01-01T00:00:00Z", "stale": false },
        "backups": { "stale": false } }]
    var data = { "lists": { "targets": [] }, "errors": { "targets": [{ "id": "retail", "partial": true }] }, "backups": { "targets": [] },
      "history": { "failed": false, "entries": [], "truncated": false, "skipped": false },
      "search": { "rows": [], "selection": { "query": "dbm", "target": "" },
        "skipped": [{ "source": "github", "reason": "offline" }, { "source": "tukui", "reason": "disabled" }] } }
    var nowMs = Date.parse("2100-01-01T00:25:00Z")
    var service = function(fields) {
      var s = { "targets": targets, "reading": [], "outcomes": {}, "listsAt": nowMs - 600000, "errorsAt": nowMs - 600000, "backupsAt": nowMs - 600000,
        "historyAt": nowMs - 600000 }
      for (var key in fields) s[key] = fields[key]
      return s
    }
    var withData = {}
    // Issues' data is `status`'s answer (1n2p.27); every other view's its own read.
    if (view === "issues") withData.summary = {}
    else withData[view === "updates" || view === "installed" ? "lists" : view] = data[view === "updates" || view === "installed" ? "lists" : view]
    var cell = function(name, fields, input) {
      var i = { "service": service(fields), "filter": "", "query": view === "search" ? "dbm" : "", "rows": 0, "allRows": 0, "nowMs": nowMs }
      for (var key in input || {}) i[key] = input[key]
      return { "title": view + " · " + name, "state": Model.viewState(view, i) }
    }
    var failed = {}
    failed[resource] = "failed"
    var reading = [resource]
    var cells = [
      cell("first load", {}),
      cell("failed", { "outcomes": failed }),
      cell("empty", withData),
      cell("refreshing", Object.assign({ "reading": reading }, withData), { "rows": 3, "allRows": 3 }),
      cell("failed with data", Object.assign({ "outcomes": failed }, withData), { "rows": 3, "allRows": 3 })
    ]
    // A search answers for its own client: it has no filtered empty.
    if (view !== "search") cells.splice(3, 0, cell("filtered empty", withData, { "filter": "era", "allRows": 2 }))
    if (view === "search") cells.unshift(cell("no query", {}, { "query": "" }))
    if (view === "updates") {
      cells.push(cell("offline", Object.assign({ "lastCheck": { "trouble": true, "failed": 0 } }, withData), { "rows": 3, "allRows": 3 }))
      cells.push(cell("offline, items failed", Object.assign({ "lastCheck": { "trouble": true, "failed": 2 } }, withData), { "rows": 3, "allRows": 3 }))
      cells.push(cell("stale", withData, { "rows": 3, "allRows": 3, "filter": "retail" }))
    }
    if (view === "errors") cells.push(cell("partial", withData, { "rows": 3, "allRows": 3 }))
    if (view === "backups") cells.push(cell("stale", withData, { "rows": 2, "allRows": 2, "filter": "retail" }))
    if (view === "search") cells.push(cell("skipped sources", withData, { "rows": 2, "allRows": 2 }))
    // Lines `history` could not read, and a list cut at its limit (1n2p.30).
    if (view === "history") {
      cells.push(cell("skipped and cut", { "history": { "failed": false, "entries": [], "truncated": true, "skipped": true } },
        { "rows": 3, "allRows": 3 }))
    }
    return cells
  }

  function shootStates(view) {
    var item = statesGalleryComponent.createObject(testCase, { "panelWidth": 540, "view": view })
    verify(item !== null)
    verify(item.cells.length >= 6, view + ": every cell")
    // Its failed-with-data cell draws its notice (the states reach ViewState intact).
    verify(findChild(item, "stateNotice") !== null, view + ": a notice is drawn")
    save(item, "states-" + view)
    item.destroy()
  }

  function test_states_updates() { shootStates("updates") }
  function test_states_installed() { shootStates("installed") }
  function test_states_search() { shootStates("search") }
  function test_states_errors() { shootStates("errors") }
  function test_states_issues() { shootStates("issues") }
  function test_states_backups() { shootStates("backups") }
  function test_states_history() { shootStates("history") }

  function test_components_at_540() { shootGallery(galleryComponent, 540, "components-540") }
  function test_components_at_400() { shootGallery(galleryComponent, 400, "components-400") }
  function test_sheet_at_540() { shootGallery(sheetGalleryComponent, 540, "components-sheet-540") }
  function test_sheet_at_400() { shootGallery(sheetGalleryComponent, 400, "components-sheet-400") }

  // ---------------------------------------------------------------- the bar button

  property var stripHolder: null

  function widgetOn(svc) {
    if (widget) widget.destroy()
    if (stripHolder) stripHolder.destroy()
    stripHolder = stripComponent.createObject(testCase)
    var api = barApiComponent.createObject(stripHolder.strip)
    widget = barComponent.createObject(stripHolder.strip, { "bar": api, "svc": svc })
    verify(widget !== null)
    widget.anchors.centerIn = stripHolder.strip
    return stripHolder
  }

  function saveBar(name) {
    wait(30)
    var image = save(stripHolder, name, true)
    compare(image.width, stripHolder.width, name + ": the bar strip's width")
  }

  // The bar button's states (1n2p.16, design doc §2.5), each on the real service over
  // envelopes the binary wrote: the glyph, the badge beside it, the spinner, the mark.
  // `status` names the status envelope (`everyday()`'s updates when empty).
  function barScene(status) {
    var scene = everyday()
    if (status) scene.status = status
    start(false)
    settle(scene)
    widgetOn(service)
  }

  function test_bar_idle() {
    barScene(the("status-idle"))
    compare([service.bar.state, service.bar.badge, service.bar.mark], ["idle", 0, ""])
    saveBar("bar-idle")
  }

  function test_bar_updates() {
    barScene(null)
    compare([service.bar.state, service.bar.badgeUrgent, service.bar.mark], ["updates", false, ""])
    verify(service.bar.badge > 0, "updates make a badge")
    saveBar("bar-updates")
  }

  function test_bar_attention() {
    barScene(the("status-attention"))
    compare([service.bar.state, service.bar.badge, service.bar.badgeUrgent, service.bar.mark], ["attention", 1, true, "attention"])
    saveBar("bar-attention")
  }

  function test_bar_attention_with_updates() {
    barScene(the("status-errors"))
    // The badge counts the 2 new Lua errors, never the update (UX-30).
    compare([service.bar.state, service.bar.badge, service.bar.badgeUrgent], ["attention", 2, true])
    saveBar("bar-attention-updates")
  }

  function test_bar_playing() {
    barScene(the("status-running"))
    compare([service.bar.state, service.bar.playing, service.bar.mark], ["updates", true, "running"])
    saveBar("bar-playing")
  }

  function test_bar_working() {
    barScene(null)
    verify(service.check())
    verify(service.bar.spinning, "a check spins the bar")
    saveBar("bar-working")
  }

  function test_bar_stale() {
    barScene(null)
    service.lastRefreshMs = Date.now() - Model.barStaleAfterMs(service.settings) - 60000
    service.refreshBar()
    compare([service.bar.state, service.bar.dimmed, service.bar.mark], ["stale", true, "stale"])
    saveBar("bar-stale")
  }

  function test_bar_broken() {
    start(true)
    wait(30)
    widgetOn(service)
    compare([service.bar.state, service.bar.dimmed, service.bar.mark], ["broken", true, "broken"])
    saveBar("bar-broken")
  }

  function test_bar_empty() {
    var env = JSON.parse(Fixtures.text("status-idle"))
    env.data.targets = []
    env.data.summary.targets = 0
    barScene({ "text": text(env), "code": 0 })
    compare([service.bar.state, service.bar.dimmed, service.bar.mark], ["empty", true, ""])
    saveBar("bar-empty")
  }
}
