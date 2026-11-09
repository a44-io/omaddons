import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "../../Store.js" as Store

// The Updates and Installed views inside the frame (pipeline step 4). Against a stub
// service: rows, groups and markers, what Enter, `u`, `U`, `p`, `e` and `x` run (template and
// slots, and the argv Model.buildArgv makes of them), view-bound release notes and their
// links. Against the real Service on the mock Process: `x` previews the removal, Yes
// runs it bound to that preview, and a stale preview comes back instead of running.
TestCase {
  id: testCase
  name: "Views"
  when: windowShown
  visible: true
  width: 600
  height: 720

  property var svc: null
  property var view: null
  property var made: []

  // `list -v` as the binary prints it: two clients; retail runs.
  function addon(name, version, latest, status, extra) {
    var a = { "name": name, "version": version, "version_confidence": "exact", "ref": "gh:o/" + name.toLowerCase(),
      "channel": "stable", "pinned": false, "state": "ok", "reason": "explicit", "required_by": [], "orphan": false,
      "latest": { "version": latest, "status": status }, "folders": [name], "missing_folders": [],
      "drift": { "state": "clean", "files": 0, "folders": [] } }
    if (extra) for (var key in extra) a[key] = extra[key]
    return a
  }

  function listEnvelope(targets) {
    return { "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": "list", "exit_code": 0,
      "partial": false, "data": { "targets": targets }, "warnings": [], "commands": [] }
  }

  function listing() {
    return listEnvelope([
      { "id": "era", "key": "k-era", "label": "era", "counts": { "managed": 2, "unmanaged": 0 }, "addons": [
        addon("Bar", "v1", "v2", "update_available"),
        addon("Plater", "1.0", "1.1", "failed")
      ] },
      { "id": "retail", "key": "k-retail", "label": "retail", "counts": { "managed": 4, "unmanaged": 2 }, "addons": [
        addon("BigWigs", "v300", "v300", "current", { "reason": "dependency", "required_by": ["LittleWigs"] }),
        addon("DBM", "12.1.14", "12.1.15", "update_available"),
        addon("Foo", "v1", "v1", "current", { "drift": { "state": "modified", "files": 1, "folders": [] } }),
        addon("WeakAuras", "5.19.2", "5.20.0", "pinned", { "pinned": true })
      ] }
    ])
  }

  Component {
    id: serviceComponent
    QtObject {
      property string phase: "ok"
      readonly property bool canAct: phase === "ok"
      property var summary: null
      property var targets: [
        { "id": "era", "running": false, "runningUnknown": false, "clientDir": "/games/era", "updates": { "checkedAt": "2100-01-01T00:00:00Z" }, "issues": [] },
        { "id": "retail", "running": true, "runningUnknown": false, "clientDir": "/games/retail", "updates": { "checkedAt": "2100-01-01T00:00:00Z" }, "issues": [] }
      ]
      property var lastChange: null
      property var action: null
      property string queuedAction: ""
      property var lastResult: null
      property var lastError: null
      property var confirm: null
      property string incompatibleText: ""
      property string unreachableText: ""
      property string binary: "/opt/omaddons-fixture/omaddons"
      property string requestedView: ""
      property var lists: null
      property var changelog: null
      property var errors: null
      property var statusWarnings: []
      property var machineIssues: []
      property var rawErrors: null
      // The doctor's explanation a problem row asked for, and every readExplanation call.
      property var explanation: null
      property var explains: []
      property int explainClears: 0
      property var search: null
      property var backups: null
      property var backupsDetail: null
      property var detailReads: []
      property int detailClears: 0
      property var folders: []

      function readBackupsDetail(target) { detailReads = detailReads.concat([target]) }
      function clearBackupsDetail() { detailClears++; backupsDetail = null }
      function restoreFolder(addon) { folders = folders.concat([addon]); return "" }

      property var runs: []
      property var cliRuns: []
      property var notes: []
      property var reads: []
      property var rawReads: []
      property int rawClears: 0
      property var searches: []
      property var dismissals: []

      // `shown` (what the view showed, for a previewed update) is recorded when given.
      function run(name, slots, label, shown) { runs = runs.concat([shown === undefined ? [name, slots, label] : [name, slots, label, shown]]); return "" }
      function runCliArgv(argv, label) { cliRuns = cliRuns.concat([[argv, label]]); return "" }
      function readView(view) { reads = reads.concat([view]) }
      function readChangelog(target, addon) { notes = notes.concat([[target, addon]]) }
      function readRawErrors(target, addon) { rawReads = rawReads.concat([[target, addon]]) }
      function clearRawErrors() { rawClears++; rawErrors = null }
      function readExplanation(key, argv) { explains = explains.concat([[key, argv]]); return true }
      function clearExplanation() { explainClears++; explanation = null }
      function searchFor(query, target) { searches = searches.concat([[query, target]]) }
      function dismissErrors(plan) { dismissals = dismissals.concat([JSON.parse(JSON.stringify(plan))]); return "" }
      // The real copyCommand runs Model.copyArgv(text); this records the argv instead.
      property var copies: []
      function copyCommand(text) {
        if (!Model.copyArgv(text)) return false
        copies = copies.concat([Model.copyArgv(text)])
        return true
      }
      // What Model.viewState reads (1n2p.19), and the views' Try again and Check now.
      property var reading: []
      property var outcomes: ({})
      property var lastCheck: null
      property bool checking: false
      property int checks: 0
      property var readAgains: []
      function readAgain(view) { readAgains = readAgains.concat([view]); return true }
      function check() { checks++ }
      function refresh() {}
      function reinstallWidget() { return "ok" }
      function confirmYes() { confirm = null; return "" }
      function confirmNo() { confirm = null }
    }
  }

  Component {
    id: frameComponent
    PanelView {
      width: 540
      height: 700
    }
  }

  Component {
    id: realService
    Service {}
  }

  function make(service) {
    var v = frameComponent.createObject(testCase, { "svc": service })
    verify(v !== null)
    made = made.concat([v])
    v.opened = true
    v.focusItem.forceActiveFocus()
    tryVerify(function() { return v.focusItem.activeFocus })
    return v
  }

  function init() {
    Quickshell.resetDetachedCommands()
    svc = serviceComponent.createObject(testCase)
    svc.lists = Model.reduceList(listing())
    view = make(svc)
  }

  function cleanup() {
    for (var i = 0; i < made.length; i++) made[i].destroy()
    made = []
    if (svc) svc.destroy()
    svc = null
    view = null
    ProcessRegistry.reset()
    wait(0)
  }

  function json(value) {
    return JSON.stringify(value)
  }

  function content() {
    var item = findChild(view, "content").item
    verify(item !== null, "a loaded view")
    return item
  }

  function row(name, index) {
    var found = findChild(content(), name + "_" + index)
    verify(found !== null, "no " + name + "_" + index)
    return found
  }

  // What a row of `listing()` showed, in Model.shownOf's fields and order (the listing
  // names no release: its source, release and file are unknown).
  function shownRow(key, target, addon, from, to) {
    return { "target_key": key, "target_id": target, "addon": addon, "source": null, "channel": "stable",
      "release_id": null, "asset": null, "from_version": from, "to_version": to }
  }

  // The command words of the argv Model.buildArgv makes of a recorded run.
  function words(run) {
    // A previewed template builds as its preview (the dry run before its plan's apply).
    var built = Model.buildArgv(run[0], run[1], { "binary": "/opt/omaddons", "label": run[2], "context": Build.CONTEXT,
      "preview": !!Model.COMMANDS[run[0]].preview })
    var argv = built.argv
    var at = argv.indexOf("--reason") + 2
    while (argv[at] === "--progress" || argv[at] === "--dry-run" || argv[at] === "--if-match") at += argv[at] === "--dry-run" ? 1 : 2
    return argv.slice(at)
  }

  // ------------------------------------------------------------- Updates

  function test_updates_are_grouped_by_client_with_the_count_U_runs() {
    var updates = content()
    compare(json(updates.rows.map(function(r) { return [r.group, r.name, r.kind] })),
      json([["era", "Bar", "update"], ["", "Plater", "failed"], ["retail", "DBM", "update"], ["", "WeakAuras", "pinned"]]))
    compare(findChild(updates, "updateAll").text, "Update all (2)")
    // A running client's update loads after a /reload.
    verify(findChild(row("updateRow", 2), "reloadChip").visible)
    verify(!findChild(row("updateRow", 0), "reloadChip").visible)
    keyClick("t")
    keyClick("t")
    compare(view.filter, "retail")
    compare(findChild(content(), "updateAll").text, "Update retail (1)")
    findChild(content(), "updateAll").clicked()
    // The update previews with what the rows showed: retail's one update.
    compare(json(svc.runs[0]), json(["update_target", { "target": "retail" }, "Update retail",
      [shownRow("k-retail", "retail", "DBM", "12.1.14", "12.1.15")]]))
    // `U` is the same button, and its tooltip says so (from KEYMAP).
    keyClick("U")
    compare(json(svc.runs[1]), json(svc.runs[0]))
    compare(findChild(content(), "updateAll").tooltipText, Model.keyTip("update_all", "updates"))
    verify(findChild(content(), "updateAll").tooltipText.endsWith("(U)"))
  }

  function test_u_updates_the_cursored_addon_and_says_so_where_nothing_waits() {
    // Updates: era's Bar, under the cursor.
    keyClick("u")
    compare(json(svc.runs[0]), json(["update_addon", { "target": "era", "addon": "Bar" }, "Update Bar",
      [shownRow("k-era", "era", "Bar", "v1", "v2")]]))
    // Backups binds no `u`: the key says why, and runs nothing.
    keyClick("5")
    keyClick("u")
    compare(svc.runs.length, 1)
    compare(findChild(view, "keyNote").text, "Nothing to update here")
    // The next key clears it.
    keyClick("j")
    compare(findChild(view, "keyNote").text, "")
  }

  function test_enter_updates_one_addon_and_p_pins_by_its_state() {
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["update_addon", { "target": "era", "addon": "Bar" }, "Update Bar",
      [shownRow("k-era", "era", "Bar", "v1", "v2")]]))
    compare(json(words(svc.runs[0])), json(["update", "-t", "era", "--", "Bar"]))
    // A failed check has nothing to update.
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 1)
    keyClick("j")
    keyClick("j")
    keyClick("p")
    compare(json(svc.runs[1]), json(["unpin", { "target": "retail", "addon": "WeakAuras" }, "Unpin WeakAuras"]))
    keyClick("k")
    keyClick("p")
    compare(json(svc.runs[2]), json(["pin", { "target": "retail", "addon": "DBM" }, "Pin DBM"]))
    // While a change runs, the rows run nothing.
    svc.action = { "label": "Update Bar", "current": 0, "total": 0, "message": "" }
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 3)
  }

  function test_e_shows_the_notes_of_its_own_row_and_safe_links_only() {
    keyClick("j")
    keyClick("j")
    keyClick("e")
    compare(json(svc.notes), json([["retail", "DBM"]]))
    var entry = row("updateRow", 2)
    verify(findChild(entry, "notes").visible)
    compare(findChild(entry, "notesText").text, "Reading the release notes…")
    // A late answer for another row is never shown here.
    svc.changelog = { "selection": { "target": "era", "addon": "Bar" }, "data": { "changelog": { "entries": [{ "version": "v2", "text": "Bar notes" }], "more_url": "https://github.com/o/bar/releases" } } }
    compare(findChild(entry, "notesText").text, "Reading the release notes…")
    svc.changelog = { "selection": { "target": "retail", "addon": "DBM" }, "data": { "changelog": { "entries": [{ "version": "12.1.15", "text": "Fixes the pull timer." }], "more_url": "https://github.com/o/dbm/releases" } } }
    compare(findChild(entry, "notesText").text, "Fixes the pull timer.")
    var link = findChild(entry, "notesLink")
    verify(link.visible)
    compare(link.text, "more on GitHub")
    link.clicked()
    compare(json(Quickshell.detachedCommands[0]), json(["omarchy-launch-browser", "https://github.com/o/dbm/releases"]))
    svc.changelog = { "selection": { "target": "retail", "addon": "DBM" }, "data": { "changelog": { "entries": [], "more_url": "http://evil.example/x" } } }
    verify(!findChild(entry, "notesLink").visible)
    keyClick("e")
    verify(!findChild(entry, "notes").visible)
  }

  function test_nothing_to_update_says_when_it_was_checked() {
    svc.lists = Model.reduceList(listEnvelope([{ "id": "era", "key": "k", "label": "era", "counts": { "unmanaged": 0 },
      "addons": [addon("Bar", "v2", "v2", "current")] }]))
    view.nowMs = Date.parse("2100-01-01T00:25:00Z")
    var empty = findChild(content(), "stateEmpty")
    verify(empty.visible)
    compare(empty.sentence, "All up to date. Checked 25 min ago.")
    verify(!findChild(content(), "updateAll").visible)
  }

  // ------------------------------------------------------------- the views' states (1n2p.19)

  // The rows stay while the list is read again, with a quiet line saying so.
  function test_a_refresh_never_blanks_the_updates() {
    svc.reading = ["list"]
    var state = findChild(content(), "viewState")
    verify(findChild(state, "stateRefreshing").visible, "refreshing says so")
    verify(!findChild(state, "stateEmpty").visible)
    compare(content().rows.length, 4, "the rows stay")
    verify(findChild(content(), "updateRow_0").visible)
    svc.reading = []
    verify(!findChild(state, "stateRefreshing").visible)
  }

  // A check that could not reach a source keeps the rows, says so, and Check now checks.
  function test_a_check_whose_source_did_not_answer_keeps_the_rows_with_check_now() {
    svc.lastCheck = { "at": "2100-01-01T00:00:00Z", "trouble": true, "failed": 0, "outcome": "ok" }
    var notices = findChild(findChild(content(), "viewState"), "stateNotices")
    compare(notices.count, 1)
    var notice = notices.itemAt(0)
    compare(notice.title, "Some sources didn't answer the last check; what they said before is shown.")
    verify(findChild(content(), "updateRow_0").visible, "the rows stay")
    notice.clicked("check")
    compare(svc.checks, 1, "Check now checks")
  }

  // No list yet: loading; a list that could not be read: the sentence and Try again,
  // which reads it again.
  function test_a_first_load_then_a_failed_read_offers_try_again() {
    svc.lists = null
    var state = findChild(content(), "viewState")
    var empty = findChild(state, "stateEmpty")
    verify(empty.visible)
    compare(empty.sentence, "Reading your addons…")
    compare(empty.actionText, "")
    compare(content().rows.length, 0)
    svc.outcomes = { "list": "failed" }
    compare(empty.sentence, "Couldn't read your addons.")
    compare(empty.actionText, "Try again")
    empty.action()
    compare(json(svc.readAgains), json(["updates"]), "Try again reads the view again")
  }

  // Search: the empty box says what to type; a search that failed asks again; skipped
  // sources say why, over the results that came.
  function test_search_says_what_to_type_and_why_sources_were_skipped() {
    keyClick("3")
    var search = content()
    var state = findChild(search, "viewState")
    compare(findChild(state, "stateEmpty").sentence, "Type a name, or paste a link from GitHub, WoWInterface, Tukui or WowAce.")
    search.asked = "dbm"
    compare(findChild(state, "stateEmpty").sentence, "Searching…")
    svc.outcomes = { "search": "failed" }
    compare(findChild(state, "stateEmpty").sentence, "The search didn't finish.")
    findChild(state, "stateEmpty").action()
    compare(json(svc.searches), json([["dbm", ""]]), "Try again searches again")
    svc.outcomes = { "search": "ok" }
    svc.search = { "query": "dbm", "rows": [{ "name": "DBM", "ref": "dbm", "source": "catalog", "compat": "", "hints": {} }],
      "skipped": [{ "source": "github", "reason": "offline" }, { "source": "wowi", "reason": "offline" }],
      "selection": { "query": "dbm", "target": "" } }
    verify(!findChild(state, "stateEmpty").visible, "results: no empty state")
    compare(findChild(state, "stateNotices").count, 1)
    compare(findChild(state, "stateNotices").itemAt(0).title, "GitHub and WoWInterface couldn't be searched: offline.")
    compare(search.rowCount, 1)
  }

  // ------------------------------------------------------------- Installed

  function test_installed_rows_carry_their_markers_and_enter_repairs_or_updates() {
    keyClick("2")
    var installed = content()
    compare(json(installed.rows.map(function(r) { return [r.group, r.name, r.markers, r.primary] })), json([
      ["era", "Bar", [], "update"], ["", "Plater", [], ""],
      ["retail", "BigWigs", ["dep"], ""], ["", "DBM", [], "update"], ["", "Foo", ["modified"], "reinstall"], ["", "WeakAuras", ["pinned"], ""]]))
    compare(findChild(row("installedRow", 2), "markers").text, "dep")
    // Foo: Enter reinstalls its modified files.
    for (var i = 0; i < 4; i++) keyClick("j")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["reinstall", { "target": "retail", "addon": "Foo" }, "Reinstall Foo"]))
    compare(json(words(svc.runs[0])), json(["install", "-t", "retail", "--reinstall", "--", "Foo"]))
    // BigWigs: nothing waits, so Enter does nothing.
    keyClick("k")
    keyClick("k")
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 1)
    keyClick("x")
    compare(json(svc.runs[1]), json(["remove", { "target": "retail", "addon": "BigWigs" }, "Remove BigWigs"]))
    keyClick("p")
    compare(json(svc.runs[2]), json(["pin", { "target": "retail", "addon": "BigWigs" }, "Pin BigWigs"]))
    // `u` updates only an addon a release waits for: nothing for BigWigs, DBM's for DBM.
    keyClick("u")
    compare(svc.runs.length, 3)
    keyClick("j")
    keyClick("u")
    compare(json(svc.runs[3].slice(0, 3)), json(["update_addon", { "target": "retail", "addon": "DBM" }, "Update DBM"]))
  }

  function test_unmanaged_folders_are_a_hint_and_an_empty_client_says_how_to_add() {
    keyClick("2")
    var lines = findChild(content(), "unmanaged")
    compare(lines.count, 1)
    compare(lines.itemAt(0).text, "2 unmanaged folders on retail: omaddons adopt -t retail")
    svc.lists = Model.reduceList(listEnvelope([{ "id": "era", "key": "k", "label": "era", "counts": { "unmanaged": 0 }, "addons": [] }]))
    var empty = findChild(content(), "stateEmpty")
    verify(empty.visible)
    compare([empty.sentence, empty.actionText], ["No addons yet. Find some in Search.", "Search"])
    empty.action()
    compare(view.view, "search", "Search opens Search")
  }

  function test_an_incompatible_binary_leaves_the_rows_inert() {
    keyClick("2")
    svc.phase = "incompatible"
    keyClick(Qt.Key_Return)
    keyClick("x")
    keyClick("p")
    compare(svc.runs.length, 0)
  }

  // ------------------------------------------------------------- Issues

  // `errors --json` as the binary prints it: era without BugGrabber; on retail, Foo
  // (an update waiting) with a Lua runtime error and a custom message, Bar current, one
  // report received from another player.
  function errorsEnvelope(partial) {
    var error = function(kind, message, chars, location, stack) {
      return { "fingerprint": "e:" + location, "database": "db-1", "origin": "local", "message_kind": kind, "message": message,
        "message_chars": chars, "location": location, "attribution": { "method": "first_addon_frame", "confidence": "high" },
        "type": "error", "count": 2, "sessions": 1, "new": true, "stack": stack }
    }
    return { "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": "errors", "exit_code": 0,
      "partial": false, "warnings": [],
      "commands": [{ "action": "install_buggrabber", "command": "omaddons install buggrabber -t era",
        "argv": ["omaddons", "install", "buggrabber", "-t", "era"], "destructive": false }],
      "data": { "targets": [
        { "target": { "id": "era" }, "capture": "none", "coverage": "complete", "files": { "partial": 0 }, "databases": [],
          "summary": { "new": 0, "received": 0 }, "addons": [], "dismiss": { "precondition": "pc1:" + "1".repeat(64) } },
        { "target": { "id": "retail" }, "capture": "buggrabber", "coverage": "complete", "files": { "partial": partial ? 1 : 0 },
          "databases": [{ "id": "db-1", "state": partial ? "partial" : "complete" }],
          "summary": { "new": 3, "received": 1 }, "dismiss": { "precondition": "pc1:" + "2".repeat(64) },
          "addons": [
            { "addon": "Foo", "managed": true, "kind": "addon", "version": "v1", "latest": { "version": "v2", "status": "update_available" },
              "count": 4, "new": 2, "errors": [error("lua_runtime", "attempt to index a nil value (field 'x')", null, "Foo/core.lua:12", ["Foo/core.lua:12: in function 'Refresh'"]),
                error("custom", null, 25, "Foo/boss.lua:3", [])] },
            { "addon": "Bar", "managed": true, "kind": "addon", "version": "v1", "latest": { "version": "v1", "status": "current" },
              "count": 1, "new": 1, "errors": [error("lua_runtime", "attempt to call a nil value", null, "Bar/x.lua:1", ["Bar/x.lua:1: in main chunk"])] }
          ] }
      ] } }
  }

  function issuesReady(partial) {
    svc.targets = [
      { "id": "era", "running": false, "runningUnknown": false, "clientDir": "/games/era", "updates": null, "issues": [
        { "id": "fm-1", "severity": "P2", "urgent": false, "title": "era: Plater's files changed", "addon": "Plater",
          "remediation": { "command": "omaddons install Plater -t era --reinstall", "argv": ["omaddons", "install", "Plater", "-t", "era", "--reinstall"], "destructive": false, "runnable": true } },
        { "id": "fm-2", "severity": "P0", "urgent": true, "title": "a journal waits for repair", "addon": null,
          "remediation": { "command": "see README: Troubleshooting", "argv": null, "destructive": false, "runnable": false } }
      ] },
      { "id": "retail", "running": false, "runningUnknown": false, "clientDir": "/games/retail", "updates": null, "issues": [] }
    ]
    svc.errors = Model.reduceErrors(errorsEnvelope(partial))
    keyClick("4")
    compare(view.view, "issues")
  }

  function test_issues_lists_problems_by_severity_then_errors_then_warnings() {
    svc.statusWarnings = [{ "code": "journal_pending", "message": "unfinished transactions wait", "fix": null, "fixText": "quit the game, then run any change" }]
    issuesReady(false)
    var issues = content()
    compare(json(issues.rows.map(function(r) { return [r.index, r.kind] })),
      json([[0, "problem"], [1, "problem"], [2, "capture"], [3, "addon"], [4, "addon"], [5, "warning"]]))
    // P0 first; its prose remediation is text, never a button.
    compare(findChild(row("issueRow", 0), "commandText").text, "see README: Troubleshooting")
    verify(!findChild(row("issueRow", 0), "enterHint").visible)
    keyClick(Qt.Key_Return)
    compare(svc.cliRuns.length, 0)
    // The P2's reinstall runs through the classifier's path.
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(json(svc.cliRuns[0]), json([["omaddons", "install", "Plater", "-t", "era", "--reinstall"], "Fix"]))
    // era: BugGrabber missing, Enter installs it.
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(json(svc.cliRuns[1]), json([["omaddons", "install", "buggrabber", "-t", "era"], "Install BugGrabber"]))
    // The warning's prose fix is text.
    compare(findChild(row("issueRow", 5), "commandText").text, "quit the game, then run any change")
  }

  // A missing client's two remediations (1n2p.43): forgetting it is a button; the purge,
  // which deletes backups and undo snapshots no undo brings back, is text with what it
  // deletes, copied with `y`, never run by Enter.
  function test_a_purge_is_text_to_copy_never_a_button() {
    var purge = ["omaddons", "targets", "rm", "classic", "--purge"]
    var forget = ["omaddons", "targets", "rm", "classic"]
    var reduced = Model.reduceStatus({ "ok": true, "schema_version": "1.1", "tool_version": "0.1.0", "command": "status",
      "exit_code": 0, "data": { "targets": [{ "id": "classic", "issues": [
        { "id": "fm-a", "severity": "P2", "title": "classic is no longer installed", "addon": null,
          "remediation": { "command": forget.join(" "), "argv": forget, "destructive": true } },
        { "id": "fm-b", "severity": "P3", "title": "classic's backups and undo snapshots remain", "addon": null,
          "remediation": { "command": purge.join(" "), "argv": purge, "destructive": true } }
      ] }] } })
    svc.targets = reduced.targets
    keyClick("4")
    compare(view.view, "issues")
    // The forget row: a button (Enter runs the fix), nothing to copy.
    verify(findChild(row("issueRow", 0), "enterHint").visible)
    verify(!findChild(row("issueRow", 0), "copyHint").visible)
    // The purge row: the command as text, what it deletes, and `y` to copy it.
    var purgeRow = row("issueRow", 1)
    compare(findChild(purgeRow, "commandText").text, purge.join(" "))
    compare(findChild(purgeRow, "irreversibleNote").text, Model.IRREVERSIBLE_NOTE)
    verify(!findChild(purgeRow, "enterHint").visible)
    compare(findChild(purgeRow, "copyHint").text, "y copies the command")
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(svc.cliRuns.length, 0)
    keyClick("y")
    compare(json(svc.copies), json([["wl-copy", "--", purge.join(" ")]]))
    compare(findChild(row("issueRow", 1), "copyHint").text, "Copied")
    compare(svc.cliRuns.length, 0)
  }

  // ---- the doctor's repairs (G2): Enter previews a repair through its bound template,
  // a forced one is a terminal command with the doctor's explanation, and a command that
  // needs consent or deletes what repairs keep is text.

  readonly property string credentialFm: "fm-secrets-credential-file-mode"

  // `status`'s issues reduced as the service reduces them: these on retail, `machine` of
  // no client.
  function statusIssues(issues, machine) {
    return Model.reduceStatus({ "ok": true, "schema_version": "1.1", "tool_version": "0.1.0", "command": "status",
      "exit_code": 0, "data": { "targets": [{ "id": "retail", "issues": issues }], "issues": machine || [] } })
  }

  function credentialIssue() {
    return { "id": credentialFm, "severity": "P2", "title": "the GitHub token file is readable by other users", "addon": null,
      "remediation": { "command": "omaddons doctor fix --only " + credentialFm, "argv": ["omaddons", "doctor", "fix", "--only", credentialFm], "destructive": false },
      "auto_fixable": true,
      "explain": { "command": "omaddons doctor explain " + credentialFm, "argv": ["omaddons", "doctor", "explain", credentialFm] } }
  }

  function test_enter_on_an_auto_fixable_problem_previews_its_repair() {
    var reduced = statusIssues([credentialIssue()], [credentialIssue()])
    svc.targets = reduced.targets
    svc.machineIssues = reduced.issues
    keyClick("4")
    compare(view.view, "issues")
    // The machine's row first, then retail's: each previews its repair.
    for (var i = 0; i < 2; i++) {
      var r = row("issueRow", i)
      compare(findChild(r, "enterHint").text, "Enter previews the repair", "row " + i)
      verify(findChild(r, "enterHint").visible, "row " + i)
      verify(!findChild(r, "commandText").visible, "row " + i + ": the preview lists what a repair does")
      verify(!findChild(r, "copyHint").visible, "row " + i)
      verify(!findChild(r, "consentNote").visible, "row " + i)
    }
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["doctor_fix", { "fm": credentialFm }, "Repair: the GitHub token file is readable by other users"]))
    compare(json(words(svc.runs[0])), json(["doctor", "fix", "--only=" + credentialFm]))
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[1]), json(["doctor_fix_target", { "target": "retail", "fm": credentialFm }, "Repair: the GitHub token file is readable by other users"]))
    compare(json(words(svc.runs[1])), json(["doctor", "fix", "-t", "retail", "--only=" + credentialFm]))
    compare(svc.cliRuns.length, 0, "the remediation argv the CLI printed is never run")
    compare(svc.explains.length, 0)
    // A change in progress: no repair starts.
    svc.action = { "label": "Update all", "current": 0, "total": 0, "message": "" }
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 2, "no repair while a change runs")
  }

  function test_a_set_aside_row_has_no_fix_and_enter_shows_why() {
    var fm = "fm-concurrency_primitives-journal-too-new"
    var forced = ["omaddons", "doctor", "fix", "--only", fm, "-t", "retail", "--force", "--yes"]
    var explain = ["omaddons", "doctor", "explain", fm, "-t", "retail"]
    svc.targets = statusIssues([{ "id": fm, "severity": "P0", "title": "An interrupted change was started by a newer omaddons", "addon": null,
      "remediation": { "command": forced.join(" "), "argv": forced, "destructive": true }, "auto_fixable": false,
      "explain": { "command": explain.join(" "), "argv": explain } }]).targets
    keyClick("4")
    var r = row("issueRow", 0)
    verify(findChild(r, "consentNote").visible)
    compare(findChild(r, "consentNote").text, "A deliberate repair: read why (Enter), then run this in a terminal:")
    verify(findChild(r, "commandText").visible)
    compare(findChild(r, "commandText").text, forced.join(" "), "the exact remediation, --force --yes included")
    compare(findChild(r, "enterHint").text, "Enter explains it")
    verify(!findChild(r, "explanation").visible)
    keyClick(Qt.Key_Return)
    compare(json(svc.explains), json([["p/retail/0", explain]]))
    compare(svc.runs.length, 0, "no repair from a forced row")
    compare(svc.cliRuns.length, 0)
    verify(findChild(r, "explanation").visible)
    verify(findChild(r, "explanationReading").visible, "Reading… until the explanation answers")
    // An answer for another row is not shown here.
    svc.explanation = { "selection": { "key": "p/era/0" }, "lines": ["elsewhere"], "failed": "" }
    compare(findChild(r, "explanationLines").count, 0)
    var lines = ["An interrupted change was started by a newer omaddons", "Its repair needs your deliberate consent: read this, then run the command below in a terminal",
      "  In a terminal: " + forced.join(" ")]
    svc.explanation = { "selection": { "key": "p/retail/0" }, "lines": lines, "failed": "" }
    compare(findChild(r, "explanationLines").count, 3)
    compare(findChild(r, "explanationLines").itemAt(2).text, lines[2])
    verify(!findChild(r, "explanationReading").visible)
    // `y` copies the terminal command.
    keyClick("y")
    compare(json(svc.copies), json([["wl-copy", "--", forced.join(" ")]]))
    // Enter again closes it.
    keyClick(Qt.Key_Return)
    compare(svc.explainClears, 1)
    verify(!findChild(r, "explanation").visible)
    compare(svc.explains.length, 1)
    // A failed read says so for its row.
    keyClick(Qt.Key_Return)
    svc.explanation = { "selection": { "key": "p/retail/0" }, "lines": null, "failed": "omaddons doctor explain: timeout" }
    compare(findChild(r, "explanationFailed").text, "Could not read the explanation: omaddons doctor explain: timeout")
    verify(findChild(r, "explanationFailed").visible)
    verify(!findChild(r, "explanationReading").visible)
    compare(svc.runs.length, 0)
    compare(svc.cliRuns.length, 0)
  }

  function test_a_doctor_gc_or_forced_remediation_is_text() {
    var gc = ["omaddons", "doctor", "gc", "--before", "30d", "--dry-run"]
    var forced = ["omaddons", "install", "Foo", "-t", "retail", "--force"]
    svc.targets = statusIssues([
      { "id": "fm-a", "severity": "P2", "title": "doctor runs take space", "addon": null,
        "remediation": { "command": gc.join(" "), "argv": gc, "destructive": true } },
      { "id": "fm-b", "severity": "P3", "title": "Foo's folder belongs to another addon", "addon": "Foo",
        "remediation": { "command": forced.join(" "), "argv": forced, "destructive": false } }
    ]).targets
    keyClick("4")
    var commands = [gc, forced]
    for (var i = 0; i < 2; i++) {
      var r = row("issueRow", i)
      verify(findChild(r, "commandText").visible, "row " + i)
      compare(findChild(r, "commandText").text, commands[i].join(" "))
      verify(!findChild(r, "enterHint").visible, "row " + i + ": " + findChild(r, "enterHint").text)
      compare(findChild(r, "copyHint").text, "y copies the command")
      keyClick(Qt.Key_Return)
      keyClick("j")
    }
    // A forced command with no explanation still says it needs consent.
    compare(findChild(row("issueRow", 1), "consentNote").text, "This needs your deliberate consent: run it in a terminal:")
    verify(!findChild(row("issueRow", 0), "consentNote").visible)
    compare(svc.runs.length, 0)
    compare(svc.cliRuns.length, 0)
    compare(svc.explains.length, 0)
  }

  function test_lua_errors_show_strict_text_and_received_only_as_a_count() {
    issuesReady(false)
    var foo = row("issueRow", 3)
    compare(findChild(foo, "headline").text, "Foo")
    verify(findChild(foo, "counts").text.indexOf("v2 available (may fix it)") > 0)
    var texts = []
    var collect = function(item) {
      for (var i = 0; i < item.children.length; i++) {
        var c = item.children[i]
        if (c.text !== undefined && typeof c.text === "string") texts.push(c.text)
        collect(c)
      }
    }
    collect(content())
    verify(texts.indexOf("Foo/boss.lua:3: custom message (25 characters, hidden)") >= 0, json(texts))
    verify(texts.indexOf("1 error received from another player (not counted)") >= 0, json(texts))
    verify(texts.indexOf("Lua errors (retail)") >= 0)
  }

  function test_enter_updates_the_likely_source_or_shows_its_stack() {
    issuesReady(false)
    for (var i = 0; i < 3; i++) keyClick("j")
    keyClick(Qt.Key_Return)
    // What the Installed rows show of Foo on retail, for the update's preview.
    compare(json(svc.runs[0]), json(["update_addon", { "target": "retail", "addon": "Foo" }, "Update Foo",
      [shownRow("k-retail", "retail", "Foo", "v1", "v1")]]))
    keyClick("j")
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 1)
    compare(findChild(row("issueRow", 4), "stack").count, 1)
  }

  function test_raw_details_are_a_button_on_the_cursored_row_and_show_for_that_row_only() {
    issuesReady(false)
    var foo = row("issueRow", 3)
    verify(!findChild(foo, "rawButton").visible, "no button before the cursor reaches the row")
    for (var i = 0; i < 3; i++) keyClick("j")
    verify(findChild(foo, "rawButton").visible)
    compare(findChild(foo, "rawButton").text, "Raw details…")
    findChild(foo, "rawButton").clicked()
    compare(json(svc.rawReads), json([["retail", "Foo"]]))
    verify(findChild(foo, "raw").visible)
    compare(findChild(foo, "rawLabel").text, "raw details: may contain other players' names and your own code")
    var raw = function(addon) {
      var env = errorsEnvelope(false)
      env.data.raw_details = true
      env.data.targets[1].addons[0].errors[1].raw = { "message": "[Foo/boss.lua]:3: FakeChar pulled", "stack": [], "locals": null, "chunks": [] }
      return { "selection": { "target": "retail", "addon": addon }, "data": Model.reduceErrors(env) }
    }
    svc.rawErrors = raw("Bar")
    compare(findChild(foo, "rawLines").count, 0, "a late read for another row is not shown")
    svc.rawErrors = raw("Foo")
    compare(findChild(foo, "rawLines").count, 1)
    compare(findChild(foo, "rawLines").itemAt(0).text, "[Foo/boss.lua]:3: FakeChar pulled")
    // The row showing them keeps its button, away from the cursor too.
    keyClick("k")
    verify(findChild(foo, "rawButton").visible)
    compare(findChild(foo, "rawButton").text, "Hide raw details")
    findChild(foo, "rawButton").clicked()
    compare(svc.rawClears, 1)
    verify(!findChild(foo, "raw").visible)
    verify(!findChild(foo, "rawButton").visible)
    // `s` is Search now (design doc §3.3), never raw details.
    keyClick("j")
    keyClick("s")
    compare(json(svc.rawReads), json([["retail", "Foo"]]))
    compare(view.view, "search")
  }

  function test_d_dismisses_what_the_panel_showed_and_a_stale_one_says_so() {
    issuesReady(false)
    verify(findChild(content(), "dismiss").visible)
    keyClick("d")
    compare(json(svc.dismissals[0]), json([{ "target": "retail", "token": "pc1:" + "2".repeat(64) }]))
    // era has nothing new to dismiss.
    keyClick("t")
    compare(view.filter, "era")
    verify(!findChild(content(), "dismiss").visible)
    keyClick("d")
    compare(json(svc.dismissals[1]), json([]))
    svc.lastError = { "message": "New errors arrived; review them", "code": "plan_stale", "fixes": [], "fixText": "" }
    compare(findChild(view, "errorMessage").text, "New errors arrived; review them")
  }

  function test_an_incomplete_read_says_so() {
    issuesReady(true)
    var texts = []
    var lines = findChild(content(), "sections")
    verify(lines !== null)
    var all = function(item) {
      for (var i = 0; i < item.children.length; i++) {
        if (typeof item.children[i].text === "string") texts.push(item.children[i].text)
        all(item.children[i])
      }
    }
    all(content())
    verify(texts.indexOf("Incomplete read (the game may be writing)") >= 0, json(texts))
  }

  // ------------------------------------------------------------- Search

  function searchReady(hints) {
    keyClick("3")
    compare(view.view, "search")
    var field = findChild(content(), "searchField")
    keyClick("s")
    tryVerify(function() { return field.activeFocus })
    verify(findChild(view, "keyCatcher").blocked)
    keyClick("u")
    compare(svc.runs.length, 0, "typing is not a command")
    field.text = "damage meter"
    keyClick(Qt.Key_Return)
    compare(json(svc.searches[svc.searches.length - 1]), json(["damage meter", view.filter]))
    tryVerify(function() { return !field.activeFocus })
    svc.search = { "selection": { "query": "damage meter", "target": view.filter }, "skipped": [], "query": "damage meter",
      "rows": [{ "name": "Details!", "ref": "wowi:23056", "source": "wowi", "downloads": 515870, "stars": null, "hints": hints, "compat": "" }] }
  }

  function test_search_installs_into_the_filtered_client_when_its_hint_allows() {
    keyClick("t")
    keyClick("t")
    compare(view.filter, "retail")
    searchReady({ "retail": "likely", "era": "no" })
    compare(findChild(row("searchRow", 0), "hints").text, "retail likely")
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["install", { "target": "retail", "ref": "wowi:23056" }, "Install Details!", []]))
    // A result for another filter is not this view's.
    keyClick("t")
    compare(findChild(content(), "searchRow_0"), null)
  }

  // The view shows no release for a search result, so the install's preview always asks.
  function test_search_install_passes_nothing_shown() {
    keyClick("t")
    keyClick("t")
    searchReady({ "retail": "yes", "era": "no" })
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 1)
    compare(json(svc.runs[0][3]), json([]), "nothing shown")
  }

  function test_a_hint_of_no_offers_no_install() {
    keyClick("t")
    compare(view.filter, "era")
    searchReady({ "retail": "likely", "era": "no" })
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 0)
  }

  function test_under_all_a_chooser_preselects_no_client() {
    searchReady({ "retail": "likely", "era": "no" })
    compare(findChild(row("searchRow", 0), "hints").text, "era no · retail likely")
    keyClick(Qt.Key_Return)
    var search = content()
    verify(findChild(search, "chooser").visible)
    compare(search.chooserCursor, -1)
    tryVerify(function() { return findChild(search, "chooserKeys").activeFocus })
    keyClick(Qt.Key_Return)
    compare(svc.runs.length, 0, "nothing chosen, nothing installed")
    // era (hint no) is greyed and never under the cursor.
    var targets = findChild(search, "chooserTargets")
    compare(targets.itemAt(0).opacity, 0.4)
    keyClick("j")
    compare(search.chooserCursor, 1)
    keyClick("j")
    compare(search.chooserCursor, 1)
    keyClick(Qt.Key_Return)
    compare(json(svc.runs[0]), json(["install", { "target": "retail", "ref": "wowi:23056" }, "Install Details!", []]))
    verify(!findChild(search, "chooser").visible)
    tryVerify(function() { return findChild(view, "keyCatcher").activeFocus })
  }

  // ------------------------------------------------------------- Backups

  // `backup list` as the binary prints it: two backups on retail (the older one in use,
  // the newer verified), one on era.
  function backupsEnvelope(verifiedOld) {
    var backup = function(id, at, extra) {
      var b = { "id": id, "created_at": at, "captured_at": at, "kind": "manual", "trigger": null, "files": 412, "bytes": 15937536,
        "reflinked": "none", "client_running": false, "unsettled": 0, "consistency": "stable", "integrity": "metadata_only",
        "verified_at": null, "baseline": null, "restorable": true, "qualifying": true, "busy": false }
      for (var key in extra) b[key] = extra[key]
      return b
    }
    return { "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": "backup list", "exit_code": 0,
      "partial": false, "warnings": [], "commands": [],
      "data": { "targets": [
        { "target": { "id": "era" }, "auto": { "mode": "off", "capability": "supported", "last": null }, "bytes": 100, "free_bytes": 1073741824,
          "shown": 1, "total": 1, "backups": [backup("01ERA00000000000000000000A", "2099-12-31T23:00:00Z", {})] },
        { "target": { "id": "retail" }, "auto": { "mode": "daily", "capability": "supported", "last": { "at": "2099-12-31T21:00:00Z", "result": "created" } },
          "bytes": 31875072, "free_bytes": 227633266688, "shown": 2, "total": 2, "backups": [
            backup("01RET00000000000000000000B", "2099-12-31T22:00:00Z", { "integrity": "verified", "baseline": "capture" }),
            backup("01RET00000000000000000000A", "2099-12-30T00:00:00Z", { "busy": true, "consistency": "best_effort", "kind": "auto", "trigger": "post_session" })
          ] }
      ] } }
  }

  function backupsReady() {
    svc.backups = Model.reduceBackups(backupsEnvelope())
    view.nowMs = Date.parse("2100-01-01T00:00:00Z")
    keyClick("5")
    compare(view.view, "backups")
  }

  function test_backups_say_what_each_proves_and_which_is_in_use() {
    backupsReady()
    var backups = content()
    compare(json(backups.rows.map(function(r) { return [r.target, r.id] })), json([
      ["era", "01ERA00000000000000000000A"], ["retail", "01RET00000000000000000000B"], ["retail", "01RET00000000000000000000A"]]))
    compare(findChild(row("backupRow", 0), "line").text, "1 hour ago · manual · 412 files · 15.2 MiB · stable")
    compare(findChild(row("backupRow", 1), "line").text, "2 hours ago · manual · 412 files · 15.2 MiB · stable · verified (capture)")
    compare(findChild(row("backupRow", 2), "line").text, "2 days ago · automatic (post session) · 412 files · 15.2 MiB · best effort · in use")
  }

  function test_b_backs_up_the_filtered_client_or_all() {
    backupsReady()
    keyClick("b")
    compare(json(svc.runs[0]), json(["backup_all", {}, "Back up all"]))
    keyClick("t")
    keyClick("t")
    compare(view.filter, "retail")
    keyClick("b")
    compare(json(svc.runs[1]), json(["backup_target", { "target": "retail" }, "Back up retail"]))
    compare(json(words(svc.runs[1])), json(["backup", "create", "-t", "retail"]))
  }

  function test_x_prunes_the_exact_backup_and_never_one_in_use() {
    backupsReady()
    keyClick("j")
    keyClick("j")
    keyClick("x")
    compare(svc.runs.length, 0)
    compare(findChild(view, "notice").text, "this backup is in use; try again when it is free")
    keyClick("k")
    keyClick("x")
    compare(json(svc.runs[0]), json(["prune_backup", { "target": "retail", "backup": "01RET00000000000000000000B" }, "Delete backup 01RET00000000000000000000B"]))
    compare(json(words(svc.runs[0])), json(["backup", "prune", "-t", "retail", "--", "01RET00000000000000000000B"]))
  }

  function test_enter_or_r_on_a_backup_opens_the_chooser_on_it() {
    backupsReady()
    keyClick("j")
    keyClick("r")
    var chooser = findChild(view, "chooser")
    verify(chooser.opened)
    compare([chooser.target, chooser.backup, chooser.step], ["retail", "01RET00000000000000000000B", "scope"])
    compare(json(svc.detailReads), json(["retail"]))
  }

  // ------------------------------------------------------------- the real service

  readonly property string planId: "01M8Q2ZZZZZZZZZZZZZZZZZZZZ"

  // omaddons launches whose argv holds `word`.
  function launches(word) {
    return ProcessRegistry.launches.filter(function(l) {
      return l.command[1] === "--json" && l.command.indexOf(word) >= 0
    })
  }

  // A storable dry run of `command` that stored planId: `data` beside its approval, and
  // the offer to apply it.
  function planPreviewJson(command, data, approval, destructive, warnings) {
    var body = { "dry_run": true, "plan_id": planId, "plan_expires_at": "2100-01-02T00:00:00Z", "precondition": "pc1:" + "a".repeat(64),
      "approval": approval }
    for (var key in data) body[key] = data[key]
    return JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": command, "exit_code": 0,
      "partial": false, "warnings": warnings || [], "data": body,
      "commands": [{ "action": "apply", "command": "omaddons apply " + planId, "argv": ["omaddons", "apply", planId], "destructive": destructive }] })
  }

  function approvalOf(op, addon, from, to, reasons, scopeFields) {
    var scope = { "dependencies_added": [], "folders_removed": [], "local_changes_replaced": { "count": 0, "folders": [] },
      "source_changed": [], "while_running": false }
    for (var key in scopeFields) scope[key] = scopeFields[key]
    return { "routine": reasons.length === 0, "reasons": reasons, "scope": scope, "selection": [{ "target_key": "k-retail", "target_id": "retail",
      "addon": addon, "source": "github", "channel": "stable", "release_id": null, "asset": null, "from_version": from, "to_version": to,
      "op": op, "fields": [], "tx": null, "backup": null }] }
  }

  // The real Service on the mock Process, at rest with one client, its frame showing
  // Installed with `addons` (list -v's): `{service, frame}`.
  function realFrame(addons) {
    svc.destroy()
    svc = null
    ProcessRegistry.reset()
    var service = realService.createObject(testCase, { "bellDebounceMs": 10, "localDeadlineMs": 2000 })
    verify(service !== null)
    made = made.concat([service])
    var statusJson = JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION,
      "command": "status", "exit_code": 0, "partial": false, "warnings": [], "commands": [],
      "data": { "targets": [{ "id": "retail", "running": false, "updates": { "available": 1 }, "issues": [] }],
        "summary": { "targets": 1, "running": [], "updates": { "available": 1, "targets": ["retail"] },
          "issues": { "p0": 0, "p1": 0, "p2": 0, "p3": 0 }, "errors": { "new": 0, "targets": [] },
          "check_due": false, "widget_outdated": false, "state": "updates" },
        "last_change": null, "last_change_state": "ok" } })
    launches("status")[0].process.finish(0, statusJson)
    tryCompare(service, "phase", "ok")
    // The catch-up checkpoint the first status queues (F8), done, and the refresh it rings.
    launches("--auto")[0].process.finish(0, JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION,
      "command": "backup create", "exit_code": 0, "partial": false, "warnings": [], "commands": [], "data": { "targets": [] } }))
    launches("status")[1].process.finish(0, statusJson)
    var frame = make(service)
    keyClick("2")
    var list = launches("list")
    verify(list.length > 0, "switching to Installed reads list")
    list[list.length - 1].process.finish(0, JSON.stringify(listEnvelope([{ "id": "retail", "key": "k-retail", "label": "retail",
      "counts": { "unmanaged": 0 }, "addons": addons }])))
    tryVerify(function() { return frame.rowCount === addons.length })
    return { "service": service, "frame": frame }
  }

  // `x` on an Installed row: the dry run first, its text and effects in the dialog, Yes
  // applies that preview's plan; a stale plan previews again instead of removing.
  function test_x_previews_the_removal_and_yes_runs_its_plan() {
    var real = realFrame([addon("Baz", "v1", "v1", "current")])
    var frame = real.frame
    keyClick("x")
    var preview = launches("remove")
    compare(preview.length, 1)
    verify(preview[0].command.indexOf("--dry-run") >= 0)
    compare(preview[0].command.slice(-5), ["remove", "-t", "retail", "--", "Baz"])
    var removal = function(version) {
      return planPreviewJson("remove", { "target": { "id": "retail" }, "items": [{ "name": "Baz", "version": version, "folders": ["Baz"] }],
        "reload_required": "none" }, approvalOf("remove", "Baz", version, null, ["folders_removed", "destructive"], { "folders_removed": ["Baz"] }),
        true, [{ "code": "still_required", "message": "LittleWigs still requires Baz", "fix": null }])
    }
    preview[0].process.finish(0, removal("v1"))
    tryVerify(function() { return frame.confirming })
    var sheet = findChild(frame, "planSheet")
    // The removal by name, its folder, and who else needs it: urgent, first, by the
    // warning's code (its text never shows).
    var text = Model.dialogText(sheet.plan)
    compare(text.split("\n").slice(1), ["Baz v1 (1 folder)", "Removes the folder Baz. Undo brings it back.",
      "Other addons still need Baz and won't load without it."])
    verify(text.indexOf("Remove Baz from ") === 0, text)
    verify(sheet.destructive)
    compare(findChild(sheet, "sheetConfirm").text, String.fromCodePoint(0xF01B4) + " Remove Baz")
    compare(launches("apply").length, 0, "nothing runs before Yes")
    tryVerify(function() { return sheet.keyItem.activeFocus })
    keyClick("y")
    var apply = launches("apply")
    compare(apply.length, 1, "Yes applies the preview's plan")
    compare(apply[0].command.slice(-2), ["apply", planId])
    verify(apply[0].command.indexOf("--if-match") < 0)
    verify(apply[0].command.indexOf("--dry-run") < 0)
    compare(launches("remove").length, 1, "the removal runs only through its plan")
    // Something changed since the preview: nothing is removed, and it previews again.
    apply[0].process.finish(1, JSON.stringify({ "ok": false, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION,
      "command": "apply", "exit_code": 1, "warnings": [], "commands": [],
      "error": { "code": "plan_stale", "message": "plan " + planId + " is no longer what its preview showed", "fix": "omaddons remove Baz -t retail --dry-run",
        "fix_argv": ["omaddons", "remove", "Baz", "-t", "retail", "--dry-run"], "alternatives": [], "retryable": false, "details": { "changed": ["Baz"] } } }))
    tryVerify(function() { return launches("remove").length === 2 })
    var again = launches("remove")[1]
    verify(again.command.indexOf("--dry-run") >= 0)
    again.process.finish(0, removal("v2"))
    tryVerify(function() { return frame.confirming })
    // The banner says it changed (a part this build does not know is no word to show),
    // over the new preview's items.
    verify(findChild(sheet, "planStale").visible)
    compare(sheet.plan.stale, "Changed since you looked: something it depends on. Nothing ran; here is what would happen now.")
    compare(sheet.plan.items.map(function(i) { return i.text }), ["Baz v2 (1 folder)"])
    compare(sheet.selectedIndex, 0)
    compare(launches("apply").length, 1, "apply never runs twice for one preview")
    real.service.destroy()
  }

  // Enter on a drifted Installed addon: the reinstall previews and asks first, always
  // (`preview: confirm`); Yes applies its plan.
  function test_enter_on_an_installed_drifted_addon_confirms_the_reinstall() {
    var real = realFrame([addon("Foo", "v1", "v1", "current", { "drift": { "state": "modified", "files": 1, "folders": [] } })])
    var frame = real.frame
    keyClick(Qt.Key_Return)
    var preview = launches("--reinstall")
    compare(preview.length, 1)
    verify(preview[0].command.indexOf("--dry-run") >= 0)
    compare(preview[0].command.slice(-6), ["install", "-t", "retail", "--reinstall", "--", "Foo"])
    compare(real.service.action.message, "Checking what it would do…")
    preview[0].process.finish(0, planPreviewJson("install", { "target": { "id": "retail" },
      "items": [{ "name": "Foo", "status": "updated", "from_version": "v1", "to_version": "v1" }] },
      approvalOf("update", "Foo", "v1", "v1", ["reinstall"], {}), false))
    tryVerify(function() { return frame.confirming })
    var sheet = findChild(frame, "planSheet")
    // A reinstall is a repair of the addon's files, in the player's words.
    verify(sheet.plan.title.indexOf("Repair Foo on ") === 0, sheet.plan.title)
    compare(sheet.plan.confirmLabel, "Repair Foo")
    compare(sheet.plan.effects.map(function(e) { return e.text }), ["Puts back Foo's files exactly as released."])
    compare(launches("apply").length, 0, "a reinstall asks first")
    tryVerify(function() { return sheet.keyItem.activeFocus })
    keyClick("y")
    compare(launches("apply").length, 1, "Yes applies the plan")
    real.service.destroy()
  }
}
