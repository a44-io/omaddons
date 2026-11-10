import QtQuick
import QtTest
import Quickshell
import Quickshell.Io
import "../.."
import "../../Model.js" as Model
import "../../Build.js" as Build
import "Fixtures.js" as Fixtures

// Sharing an addon set (1n2p.37) over envelopes the real binary wrote (pipeline step 1's
// share scenes): the Service on the mock Process. Copy addon list reads `export` and puts
// its list on the clipboard through a fake wl-copy; Paste addon list reads the clipboard
// through a fake wl-paste, checks it, and previews `sync -` with the list on the child's
// standard input; the plan sheet groups that preview, and its Yes applies the stored
// plan, which needs no standard input.
TestCase {
  id: testCase
  name: "Share"

  property var service: null

  Component {
    id: serviceComponent
    Service {}
  }

  // A list as `export` writes one, which the paste checks pass.
  readonly property string list: JSON.parse(Fixtures.text("share-export-retail")).data.content

  function make(extra) {
    if (service) service.destroy()
    service = null
    wait(0)
    ProcessRegistry.reset()
    Quickshell.resetDetachedCommands()
    var props = { "bellDebounceMs": 10, "localDeadlineMs": 400, "networkDeadlineMs": 2000, "killGraceMs": 60, "streamGraceMs": 60 }
    for (var key in extra || {}) props[key] = extra[key]
    service = serviceComponent.createObject(testCase, props)
    verify(service !== null)
  }

  function init() {
    make()
    ready()
  }

  function cleanup() {
    if (service) service.destroy()
    service = null
    ProcessRegistry.reset()
    wait(0)
  }

  // ---- helpers (tst_plans.qml's)

  // The command words and arguments after the standard flags.
  function words(command) {
    var i = 6
    while (i < command.length) {
      if (command[i] === "--progress" || command[i] === "--if-match") i += 2
      else if (command[i] === "--dry-run") i += 1
      else break
    }
    return command.slice(i)
  }

  function runs(word) {
    var out = []
    for (var i = 0; i < ProcessRegistry.launches.length; i++) {
      var launch = ProcessRegistry.launches[i]
      if (launch.command[1] !== "--json" || launch.command[2] !== "--actor") continue
      if (word === undefined || words(launch.command)[0] === word) out.push(launch)
    }
    return out
  }

  function lastRun(word) {
    var all = runs(word)
    return all.length ? all[all.length - 1] : null
  }

  // The launches of a desktop tool (`wl-copy`, `wl-paste`).
  function tool(name) {
    return ProcessRegistry.launches.filter(function(l) { return l.command[0] === name })
  }

  function launchLog() {
    return JSON.stringify(ProcessRegistry.launches.map(function(l) { return l.command[0] === "wl-copy" ? ["wl-copy"] : words(l.command) }))
  }

  function envelope(command, data) {
    return JSON.stringify({ "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": command,
      "exit_code": 0, "partial": false, "data": data, "warnings": [], "commands": [] })
  }

  // The binary's fixture `name` as the answer of `launch` (exit `code`, 0 by default).
  function answer(launch, name, code) {
    verify(launch !== null, "a launch to answer with " + name + ": " + launchLog())
    launch.process.finish(code || 0, Fixtures.text(name))
  }

  // The startup status (the share home's two clients), and every checkpoint and refresh
  // it queues, answered until nothing of theirs runs.
  function ready() {
    lastRun("status").process.finish(0, Fixtures.text("share-status"))
    compare(service.phase, "ok")
    for (var i = 0; i < 12; i++) {
      wait(0)
      var open = ProcessRegistry.launches.filter(function(l) { return !!l.process && l.process.running && l.command[1] === "--json" })
      if (open.length === 0) break
      var next = open[0]
      var word = words(next.command)[0]
      if (word === "backup") next.process.finish(0, envelope("backup create", { "dry_run": false, "run": null, "targets": [] }))
      else next.process.finish(0, Fixtures.text("share-status"))
    }
  }

  // A child's start (the mock's `started` comes on a later turn), as a real child's.
  function started(launch) {
    tryVerify(function() { return !!launch.process && launch.process.processId !== null }, 1000, "the child started")
  }

  // The clipboard read for a paste, answered with `text` (wl-paste's exit `code`).
  function clipboard(text, code) {
    var reads = tool("wl-paste")
    verify(reads.length > 0, "the clipboard was read: " + launchLog())
    var read = reads[reads.length - 1]
    compare(read.command, ["wl-paste", "--no-newline", "--type", "text"])
    compare(read.environment, {}, "a desktop helper: no stamped context")
    read.process.finish(code || 0, text)
  }

  function sheetText() {
    verify(service.confirm !== null && !!service.confirm.sheet, "a plan sheet is open: " + launchLog())
    return Model.dialogText(service.confirm.sheet).split("\n")
  }

  // ---- Copy addon list

  function test_a_copy_reads_the_export_and_puts_its_list_on_the_clipboard() {
    var answers = []
    verify(service.copyAddonList("retail", function(ok, line) { answers.push([ok, line]) }))
    var exported = lastRun("export")
    compare(words(exported.command), ["export", "-t", "retail"])
    verify(exported.command.indexOf("--dry-run") === -1 && exported.command.indexOf("--progress") === -1, "a read")
    compare(exported.environment, Build.CONTEXT)
    compare(exported.stdinEnabled, false)
    compare(answers, [], "nothing said before the export answers")
    answer(exported, "share-export-retail")
    var copies = tool("wl-copy")
    compare(copies.length, 1)
    compare(copies[0].command, ["wl-copy", "--", list], "the export's TOML, as one argument")
    compare(answers, [], "nothing said before wl-copy answers")
    copies[0].process.exit(0, 0)
    compare(answers, [[true, "Copied Retail's addon list: 3 addons"]])
    // Every client's: export with no -t.
    verify(service.copyAddonList("", function(ok, line) { answers.push([ok, line]) }))
    compare(words(lastRun("export").command), ["export"])
    answer(lastRun("export"), "share-export-all")
    tool("wl-copy")[1].process.exit(0, 0)
    compare(answers[1], [true, "Copied the addon list of every client: 3 addons"])
    // A copy is no change: nothing on the mutation lane, no failure card.
    compare(service.lastError, null)
    compare(runs("apply").length + runs("sync").length, 0)
  }

  function test_a_copy_with_nothing_to_copy_or_no_wl_copy_says_why() {
    var answers = []
    service.copyAddonList("era", function(ok, line) { answers.push([ok, line]) })
    answer(lastRun("export"), "share-export-era")
    compare(answers, [[false, "omaddons manages no addons on Classic Era yet, so there's no list to copy"]])
    compare(tool("wl-copy").length, 0, "nothing to copy runs no wl-copy")
    // wl-copy missing: the copy says so, never "Copied".
    service.copyAddonList("retail", function(ok, line) { answers.push([ok, line]) })
    ProcessRegistry.missingBinary = true
    answer(lastRun("export"), "share-export-retail")
    tryVerify(function() { return answers.length === 2 }, 1000, "wl-copy's absence is answered")
    ProcessRegistry.missingBinary = false
    compare(answers[1], [false, "Couldn't copy (wl-copy is missing)"])
    // An export that failed is the copy's to say, never a failure card.
    service.copyAddonList("retail", function(ok, line) { answers.push([ok, line]) })
    lastRun("export").process.finish(1, "")
    tryVerify(function() { return answers.length === 3 }, 1000)
    compare(answers[2], [false, "Couldn't read the addon list to copy"])
    compare(service.lastError, null)
    compare(service.phase, "ok")
  }

  // ---- Paste addon list

  function test_a_paste_previews_sync_with_the_list_on_standard_input_and_applies_its_plan() {
    var refusals = []
    verify(service.pasteAddonList("", function(refusal) { refusals.push(refusal) }))
    compare(runs("sync").length, 0, "nothing runs before the clipboard is read")
    clipboard(list)
    compare(refusals, [""])
    var preview = lastRun("sync")
    compare(words(preview.command), ["sync", "-v", "-"])
    verify(preview.command.indexOf("--dry-run") !== -1, "its preview")
    compare(preview.environment, Build.CONTEXT)
    compare(preview.stdinEnabled, true)
    started(preview)
    compare(preview.process.written, list, "the list, whole, on its standard input")
    verify(preview.process.stdinClosed, "then its end, so sync - reads it all")
    answer(preview, "share-sync-preview")
    compare(sheetText(), [
      "Use the pasted addon list on 2 clients?",
      "Classic Era · New",
      "Baz v1 (new)",
      "Retail · Conflicts",
      "qux installed here from another source; kept as it is",
      "Bar pinned at v1",
      "Retail · New",
      "Baz v1 (new)",
      "Retail · Unchanged",
      "Foo v1",
      "Part of the list is for a client that isn't on this computer; it's left out.",
      "Addons the list doesn't have stay as they are."
    ])
    compare(service.confirm.sheet.confirmLabel, "Make 3 changes")
    compare(runs("apply").length, 0, "nothing applies before Yes")
    // Yes: the plan's apply, which holds the list, with no standard input.
    compare(service.confirmYes(), "")
    var applied = lastRun("apply")
    compare(words(applied.command), ["apply", JSON.parse(Fixtures.text("share-sync-preview")).data.plan_id])
    compare(applied.stdinEnabled, false)
    started(applied)
    compare(applied.process.written, "")
    answer(applied, "share-sync-apply")
    tryVerify(function() { return service.lastResult !== null }, 1000)
    compare(service.lastResult.line, "Matched the addon list: 3 addons changed on 2 clients · 1 kept as it is")
    // Nothing but the preview was ever given standard input.
    compare(ProcessRegistry.launches.filter(function(l) { return l.stdinEnabled }).length, 1)
  }

  function test_a_paste_on_one_client_previews_there() {
    var refusals = []
    service.pasteAddonList("retail", function(refusal) { refusals.push(refusal) })
    clipboard(list)
    compare(words(lastRun("sync").command), ["sync", "-v", "-", "-t", "retail"])
    started(lastRun("sync"))
    answer(lastRun("sync"), "share-sync-preview-retail")
    compare(sheetText()[0], "Use the pasted addon list on Retail?")
    compare(service.confirm.sheet.items.map(function(i) { return i.header }).filter(function(h) { return h !== "" }),
      ["Conflicts", "New", "Unchanged"])
  }

  function test_what_is_no_addon_list_runs_nothing_and_says_why() {
    var cases = [
      ["wl-paste found no text", "", 1, "Couldn't read the clipboard: wl-paste is missing, or it holds no text"],
      ["an empty clipboard", "", 0, "The clipboard is empty. Copy an addon list first, then paste it here."],
      ["a link", "https://github.com/o/foo", 0, "The clipboard doesn't hold an addon list. Ask for one copied with Copy addon list."],
      ["omaddons' settings", "running_policy = \"allow\"\n", 0, "The clipboard doesn't hold an addon list. Ask for one copied with Copy addon list."],
      ["control characters", list + "\u001b]52;c;x\u0007", 0, "The clipboard doesn't hold plain text"],
      ["more than a list holds", list + "#".repeat(Model.PASTE_MAX_BYTES), 0, "The clipboard holds more than an addon list can (64 KB at most)"]
    ]
    for (var i = 0; i < cases.length; i++) {
      var refusals = []
      verify(service.pasteAddonList("retail", function(refusal) { refusals.push(refusal) }), cases[i][0])
      clipboard(cases[i][1], cases[i][2])
      tryVerify(function() { return refusals.length === 1 }, 1000, cases[i][0])
      compare(refusals, [cases[i][3]], cases[i][0])
    }
    compare(runs("sync").length, 0, "no sync ran: " + launchLog())
    compare(service.confirm, null)
  }

  function test_a_missing_or_silent_wl_paste_pastes_nothing() {
    var refusals = []
    ProcessRegistry.missingBinary = true
    service.pasteAddonList("", function(refusal) { refusals.push(refusal) })
    tryVerify(function() { return refusals.length === 1 }, 1000, "wl-paste's absence is answered")
    ProcessRegistry.missingBinary = false
    compare(refusals, ["Couldn't read the clipboard: wl-paste is missing, or it holds no text"])
    compare(service.phase, "ok", "the phase is the binary's, never wl-paste's")
    // One that never answers is stopped by its deadline.
    make({ "pasteDeadlineMs": 80 })
    ready()
    refusals = []
    service.pasteAddonList("", function(refusal) { refusals.push(refusal) })
    compare(tool("wl-paste").length, 1)
    wait(20)
    compare(refusals, [])
    tryVerify(function() { return refusals.length === 1 }, 1000, "the deadline answers")
    compare(refusals, ["Couldn't read the clipboard: wl-paste is missing, or it holds no text"])
    compare(runs("sync").length, 0)
  }

  function test_a_list_the_cli_refuses_says_where_its_mistake_is() {
    service.pasteAddonList("", function(refusal) {})
    clipboard(list)
    started(lastRun("sync"))
    answer(lastRun("sync"), "share-sync-invalid", 2)
    tryVerify(function() { return service.lastError !== null }, 1000)
    compare(service.lastError.message, "The pasted addon list names a game omaddons doesn't know on line 3; nothing ran")
    compare(service.lastError.fixes, [])
    compare(service.confirm, null)
  }

  function test_a_list_everything_matches_says_so_and_runs_nothing() {
    service.pasteAddonList("retail", function(refusal) {})
    clipboard(list)
    started(lastRun("sync"))
    answer(lastRun("sync"), "share-sync-nothing")
    tryVerify(function() { return service.lastResult !== null }, 1000)
    compare(service.lastResult.line, "Your addons already match the pasted list; the view is refreshed")
    compare(service.confirm, null)
    compare(runs("apply").length, 0)
  }

  function test_a_stale_plan_previews_the_same_list_again() {
    service.pasteAddonList("", function(refusal) {})
    clipboard(list)
    started(lastRun("sync"))
    answer(lastRun("sync"), "share-sync-preview")
    compare(service.confirmYes(), "")
    answer(lastRun("apply"), "plan-apply-stale", 1)
    // The preview again, first in line, with the same list on its standard input.
    tryVerify(function() { return runs("sync").length === 2 }, 1000, "previewed again: " + launchLog())
    var again = lastRun("sync")
    compare(again.stdinEnabled, true)
    started(again)
    compare(again.process.written, list)
    answer(again, "share-sync-preview")
    verify(service.confirm.stale !== "" || service.confirm.sheet.stale !== "", "the sheet says it changed since")
  }
}
