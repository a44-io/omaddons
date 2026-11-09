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
  width: 620
  height: 1100

  // The panel as the bar's KeyboardPanel sizes it (BarWidget.qml): 540 wide by default
  // (a scene may set the minimum, 400, before it opens), at most 720 tall, inside the
  // popup card.
  property int panelWidth: 540
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
  function words(command) {
    var i = 6
    while (i < command.length) {
      if (command[i] === "--progress" || command[i] === "--if-match") i += 2
      else if (command[i] === "--dry-run") i += 1
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
    if (w[0] === "search") return scene.search
    if (w[0] === "info") return scene.changelog
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

  // A fresh Service on a fresh mock; `missing` makes the binary absent.
  function start(missing) {
    if (service) service.destroy()
    service = null
    wait(0)
    ProcessRegistry.reset()
    ProcessRegistry.missingBinary = missing === true
    Quickshell.resetDetachedCommands()
    service = serviceComponent.createObject(testCase, { "bellDebounceMs": 10, "localDeadlineMs": 600000,
      "networkDeadlineMs": 600000, "killGraceMs": 60, "streamGraceMs": 30 })
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
    panelWidth = 540
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
  function save(item, name, blank) {
    wait(60)
    var image = grabImage(item)
    verify(image.width > 0 && image.height > 0, name + ": an empty grab")
    if (!blank) verify(drawn(image), name + ": nothing drawn")
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
      "backupsDetail": the("backups-detail"), "changelog": the("changelog") }
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
    shoot("phase-unreachable")
  }

  function test_phase_incompatible() {
    var scene = everyday()
    open(scene, "updates")
    service.refresh()
    settle({ "status": statusFrom("status-updates", function(env) { env.tool_version = "0.2.0" }) })
    compare(service.phase, "incompatible")
    shoot("phase-incompatible")
  }

  // The binary sets `summary.widget_outdated` only with `widget.outdated` (an outdated
  // widget serving this state), which Model.compatible reads as another build: the
  // panel goes read-only, and the frame's own "from another build" card never shows.
  function test_phase_widget_outdated() {
    var scene = everyday()
    scene.status = statusFrom("status-updates", function(env) {
      env.data.summary.widget_outdated = true
      env.data.widget = { "omarchy": true, "installed": true, "enabled": true, "version": Build.TOOL_VERSION,
        "outdated": true, "context_matches": true }
    })
    open(scene, "updates")
    compare(service.phase, "incompatible")
    shoot("phase-widget-outdated")
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

  // One client in scope: its name as the title, its version and family, its counts.
  function test_frame_one_client() {
    open(everyday(), "updates", "retail")
    compare(findChild(frame, "hero").title, "Retail")
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
      "search": { "rows": [], "selection": { "query": "dbm", "target": "" },
        "skipped": [{ "source": "github", "reason": "offline" }, { "source": "tukui", "reason": "disabled" }] } }
    var nowMs = Date.parse("2100-01-01T00:25:00Z")
    var service = function(fields) {
      var s = { "targets": targets, "reading": [], "outcomes": {}, "listsAt": nowMs - 600000, "errorsAt": nowMs - 600000, "backupsAt": nowMs - 600000 }
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

  function test_bar_idle() {
    var scene = everyday()
    scene.status = the("status-idle")
    start(false)
    settle(scene)
    widgetOn(service)
    saveBar("bar-idle")
  }

  function test_bar_badge() {
    start(false)
    settle(everyday())
    widgetOn(service)
    verify(service.bar.badge > 0, "updates make a badge")
    saveBar("bar-badge")
  }

  function test_bar_urgent() {
    var scene = everyday()
    scene.status = the("status-errors")
    start(false)
    settle(scene)
    widgetOn(service)
    verify(service.bar.urgent === true, "new Lua errors are urgent")
    saveBar("bar-urgent")
  }

  function test_bar_dimmed() {
    start(true)
    wait(30)
    widgetOn(service)
    verify(service.bar.dimmed === true, "no binary dims the bar")
    saveBar("bar-dimmed")
  }
}
