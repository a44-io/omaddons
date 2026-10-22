// Model.js under bun (M3-D41, pipeline step 2): the reducers over envelopes the real
// binary wrote to $OMADDONS_WIDGET_OUT/fixtures/ (step 1), argv building with hostile
// slots, the CLI argv classifier, the scheduler's decisions and the settings. It writes
// $OMADDONS_WIDGET_OUT/built-argv.json, which step 3 parses with the binary's own clap.

const { describe, expect, test } = require("bun:test")
const fs = require("fs")
const path = require("path")
const M = require("../Model.js")

const OUT = process.env.OMADDONS_WIDGET_OUT
if (!OUT) throw new Error("OMADDONS_WIDGET_OUT is unset: run the widget pipeline (argc widget-test)")
const FIXTURES = path.join(OUT, "fixtures")

function fixture(name) {
  return JSON.parse(fs.readFileSync(path.join(FIXTURES, name + ".json"), "utf8"))
}

function fixtureText(name) {
  return fs.readFileSync(path.join(FIXTURES, name), "utf8")
}

const BINARY = "/opt/omaddons-fixture/omaddons"
const CONTEXT = {
  HOME: "/home/fixture",
  XDG_CONFIG_HOME: "/home/fixture/.config",
  XDG_STATE_HOME: "/home/fixture/.local/state",
  XDG_CACHE_HOME: "/home/fixture/.cache",
  XDG_DATA_HOME: "/home/fixture/.local/share",
}
const TOKEN = "pc1:" + "0123456789abcdef".repeat(4)
const NOW = Date.parse("2100-01-01T00:00:00Z")

function opts(extra) {
  return Object.assign({ binary: BINARY, label: "Test", context: CONTEXT }, extra || {})
}

function reasonOf(fn) {
  try {
    fn()
  } catch (e) {
    return e.reason || "threw without a reason: " + e.message
  }
  return null
}

// ------------------------------------------------------------------- settings

describe("settings", () => {
  test("DEFAULT_SETTINGS equals manifest.json's defaults", () => {
    const manifest = JSON.parse(fs.readFileSync(path.join(__dirname, "..", "manifest.json"), "utf8"))
    expect(M.DEFAULT_SETTINGS).toEqual(manifest.barWidget.defaults)
    const schema = Object.fromEntries(manifest.barWidget.schema.map((s) => [s.key, s.defaultValue]))
    expect(schema).toEqual(M.DEFAULT_SETTINGS)
  })

  test("a valid value is kept", () => {
    const s = { binary: "/usr/bin/omaddons", panelWidth: 600, autoCheck: "Off", refreshIntervalSec: 3600 }
    expect(M.setting(s, "binary")).toBe("/usr/bin/omaddons")
    expect(M.setting(s, "panelWidth")).toBe(600)
    expect(M.setting(s, "autoCheck")).toBe("Off")
    expect(M.setting(s, "refreshIntervalSec")).toBe(3600)
    expect(M.setting({ notifyErrors: false }, "notifyErrors")).toBe("Off")
  })

  test("missing, wrong-typed and out-of-range values fall back to the default", () => {
    for (const settings of [undefined, null, {}, { panelWidth: null }]) {
      expect(M.setting(settings, "panelWidth")).toBe(540)
    }
    expect(M.setting({ panelWidth: "600" }, "panelWidth")).toBe(540)
    expect(M.setting({ panelWidth: 399 }, "panelWidth")).toBe(540)
    expect(M.setting({ panelWidth: 801 }, "panelWidth")).toBe(540)
    expect(M.setting({ panelWidth: 540.5 }, "panelWidth")).toBe(540)
    expect(M.setting({ refreshIntervalSec: 29 }, "refreshIntervalSec")).toBe(300)
    expect(M.setting({ autoCheck: "yes" }, "autoCheck")).toBe("On")
    expect(M.setting({ autoCheck: 1 }, "autoCheck")).toBe("On")
    // A bare name would be looked up on PATH.
    expect(M.setting({ binary: "omaddons" }, "binary")).toBe("")
    expect(M.setting({ binary: 42 }, "binary")).toBe("")
    expect(M.setting({}, "nosuch")).toBeUndefined()
  })

  test("intSetting and boolSetting take explicit defaults", () => {
    expect(M.intSetting({ panelWidth: 700 }, "panelWidth", 540, 400, 800)).toBe(700)
    expect(M.intSetting({ panelWidth: 900 }, "panelWidth", 540, 400, 800)).toBe(540)
    expect(M.intSetting({}, "panelWidth", 540, 400, 800)).toBe(540)
    expect(M.boolSetting({ notifyErrors: "Off" }, "notifyErrors", true)).toBe(false)
    expect(M.boolSetting({ notifyErrors: true }, "notifyErrors", false)).toBe(true)
    expect(M.boolSetting({ notifyErrors: "maybe" }, "notifyErrors", true)).toBe(true)
  })

  test("the UI-only constants are pinned", () => {
    expect(M.WIDGET_LOCAL_DEADLINE_SEC).toBe(20)
    expect(M.WIDGET_KILL_GRACE_SEC).toBe(5)
    expect(M.WIDGET_STREAM_GRACE_SEC).toBe(2)
  })
})

// --------------------------------------------------------------------- argv

const HOSTILE = [
  "name with spaces",
  "!bang",
  "$(rm -rf ~)",
  "a;b && c",
  "-x",
  "--json",
  "--target=evil",
  "--",
  "$&$1",
  "ünïcødé Ä 魔兽",
  "'quoted\"",
]
const BENIGN = {
  target: "retail",
  addon: "DBM",
  ref: "gh:o/foo",
  query: "damage meter",
  backup: "01M6QZ0000000000000000000A",
}

// What a template needs beyond its slots to build: a confirm template's token.
function runOpts(name) {
  const t = M.COMMANDS[name]
  if (t.confirm || t.bind || M.CLI_PATHS[M.templateShape(name).path].destructive) return opts({ token: TOKEN })
  return opts()
}

function benignSlots(name) {
  return Object.fromEntries(M.templateShape(name).slots.map((s) => [s, BENIGN[s]]))
}

describe("buildArgv", () => {
  test("puts the standard flags right after the binary", () => {
    const built = M.buildArgv("update_addon", { target: "retail", addon: "DBM" }, opts({ label: "Update" }))
    expect(built.argv).toEqual([
      BINARY, "--json", "--actor", "widget", "--reason", "Update (bar widget)", "--progress", "json",
      "update", "-t", "retail", "--", "DBM",
    ])
    expect(built.env).toEqual(CONTEXT)
    expect(built.env).not.toBe(CONTEXT)
  })

  test("--progress json goes on mutations and check only", () => {
    expect(M.buildArgv("check", {}, opts()).argv).toContain("--progress")
    expect(M.buildArgv("status", {}, opts()).argv).not.toContain("--progress")
    expect(M.buildArgv("search", { target: "retail", query: "x" }, opts()).argv).not.toContain("--progress")
  })

  test("a preview adds --dry-run, a confirmed run its token", () => {
    const slots = { target: "retail", addon: "Baz" }
    const preview = M.buildArgv("remove", slots, opts({ preview: true })).argv
    expect(preview.slice(0, 9)).toEqual([BINARY, "--json", "--actor", "widget", "--reason", "Test (bar widget)", "--progress", "json", "--dry-run"])
    const run = M.buildArgv("remove", slots, opts({ token: TOKEN })).argv
    expect(run.slice(8, 10)).toEqual(["--if-match", TOKEN])
    expect(run).not.toContain("--dry-run")
  })

  test("refuses what the templates do not allow", () => {
    const remove = { target: "retail", addon: "Baz" }
    expect(reasonOf(() => M.buildArgv("remove", remove, opts()))).toBe("destructive_without_preview")
    expect(reasonOf(() => M.buildArgv("remove", remove, opts({ confirmed: true })))).toBe("destructive_without_preview")
    expect(reasonOf(() => M.buildArgv("remove", remove, opts({ token: "pc1:abc" })))).toBe("bad_token")
    expect(reasonOf(() => M.buildArgv("remove", remove, opts({ token: TOKEN.toUpperCase() })))).toBe("bad_token")
    expect(reasonOf(() => M.buildArgv("remove", remove, opts({ preview: true, token: TOKEN })))).toBe("token_in_preview")
    expect(reasonOf(() => M.buildArgv("update_all", {}, opts({ token: TOKEN })))).toBe("token_not_bound")
    expect(reasonOf(() => M.buildArgv("nosuch", {}, opts()))).toBe("unknown_template")
    expect(reasonOf(() => M.buildArgv("update_target", {}, opts()))).toBe("unfilled_slot")
    expect(reasonOf(() => M.buildArgv("update_all", { target: "retail" }, opts()))).toBe("unknown_slot")
    expect(reasonOf(() => M.buildArgv("update_target", { target: "retail", evil: "x" }, opts()))).toBe("unknown_slot")
    expect(reasonOf(() => M.buildArgv("update_addon", { target: "retail", addon: "" }, opts()))).toBe("bad_value")
    expect(reasonOf(() => M.buildArgv("update_addon", { target: "retail", addon: "a\u0000b" }, opts()))).toBe("bad_value")
    expect(reasonOf(() => M.buildArgv("update_all", {}, opts({ binary: "omaddons" })))).toBe("bad_binary")
    expect(reasonOf(() => M.buildArgv("update_all", {}, opts({ label: "" })))).toBe("bad_label")
    expect(reasonOf(() => M.buildArgv("update_all", {}, opts({ label: "a\nb" })))).toBe("bad_label")
    expect(reasonOf(() => M.buildArgv("update_all", {}, opts({ context: { HOME: "/h" } })))).toBe("bad_context")
  })

  test("a bad {target} is refused", () => {
    for (const target of ["-t", "--json", "re tail", "retail;rm", "ünï", "", ".hidden"]) {
      expect(reasonOf(() => M.buildArgv("update_target", { target }, opts()))).toBe(target === "" ? "bad_value" : "bad_target")
    }
    // Ids and keys pass.
    expect(M.buildArgv("update_target", { target: "01M5JHFF37RESHT6QWM5BT2SXW" }, opts()).argv).toContain("01M5JHFF37RESHT6QWM5BT2SXW")
  })

  test("hostile values stay one argv element, after -- or in --opt=", () => {
    for (const name of Object.keys(M.COMMANDS)) {
      for (const slot of M.templateShape(name).slots) {
        if (slot === "target") continue
        for (const value of HOSTILE) {
          const slots = Object.assign(benignSlots(name), { [slot]: value })
          const argv = M.buildArgv(name, slots, runOpts(name)).argv
          const at = argv.lastIndexOf(value)
          const option = argv.find((a) => a.endsWith("=" + value))
          expect(at !== -1 || option !== undefined).toBe(true)
          if (at !== -1) expect(argv.indexOf("--")).toBeLessThan(at)
        }
      }
    }
  })

  test("{query} in a mutating template, and malformed templates, are refused", () => {
    const planted = {
      query_mutation: { argv: ["update", "--", "{query}"], mutates: true },
      positional_first: { argv: ["update", "{addon}"], mutates: true },
      option_split: { argv: ["errors", "--addon", "{addon}"], mutates: false, lane: "local" },
      flag_after: { argv: ["update", "--", "{addon}", "--json"], mutates: true },
      owned_flag: { argv: ["status", "--json"], mutates: false, lane: "local" },
      no_path: { argv: ["frobnicate"], mutates: false, lane: "local" },
    }
    try {
      Object.assign(M.COMMANDS, planted)
      for (const name of Object.keys(planted)) expect(reasonOf(() => M.templateShape(name))).toBe("bad_template")
    } finally {
      for (const name of Object.keys(planted)) delete M.COMMANDS[name]
    }
  })

  test("a template table can be passed in place of COMMANDS", () => {
    const table = Object.assign({}, M.COMMANDS, {
      pin_silent: { argv: ["pin", "-t", "{target}", "--", "{addon}"], mutates: true, silent: true },
    })
    const built = M.buildArgv("pin_silent", { target: "retail", addon: "Baz" }, opts({ templates: table }))
    expect(built.argv.slice(-5)).toEqual(["pin", "-t", "retail", "--", "Baz"])
    expect(reasonOf(() => M.buildArgv("pin_silent", { target: "retail", addon: "Baz" }, opts()))).toBe("unknown_template")
    expect(M.COMMANDS.pin_silent).toBeUndefined()
  })

  test("every shipped template is well-formed and builds with benign slots", () => {
    for (const name of Object.keys(M.COMMANDS)) {
      const shape = M.templateShape(name)
      expect(M.CLI_PATHS[shape.path]).toBeDefined()
      expect(M.COMMANDS[name].mutates).toBe(M.CLI_PATHS[shape.path].mutates)
      if (M.COMMANDS[name].confirm) expect(M.CLI_PATHS[shape.path].destructive).toBe(true)
      expect(M.buildArgv(name, benignSlots(name), runOpts(name)).argv[0]).toBe(BINARY)
    }
  })
})

// ---------------------------------------------------------- the classifier

// [argv, expected reason ("" when accepted), destructive]
const CLASSIFIER_TABLE = [
  [["omaddons", "targets"], "", false],
  [["omaddons", "install", "Baz", "-t", "retail", "--reinstall"], "", false],
  [["omaddons", "undo", "--run", "01M5JHFF8X0FXBT1K068DQKCHG"], "", true],
  [["omaddons", "help", "update"], "", false],
  [["omaddons", "targets", "rm", "retail"], "", true],
  [["omaddons", "remove", "-t", "retail", "--", "Foo"], "", true],
  [["omaddons", "remove", "--", "--json"], "", true],
  [["omaddons", "--actor", "agent", "update"], "widget_flag", false],
  [["omaddons", "status", "--json"], "widget_flag", false],
  [["omaddons", "update", "--reason=sneaky"], "widget_flag", false],
  [["omaddons", "remove", "--if-match", TOKEN, "--", "Foo"], "widget_flag", false],
  [["omaddons", "--progress", "none", "status"], "widget_flag", false],
  [["omaddons", "frobnicate"], "unknown_path", false],
  [["omaddons", "-t", "retail", "update"], "unknown_path", false],
  [["rm", "-rf", "/"], "not_omaddons", false],
  [["/usr/bin/omaddons", "status"], "not_omaddons", false],
  [["omaddons"], "malformed", false],
  [["omaddons", 7], "malformed", false],
]

describe("classifyCliArgv", () => {
  test("the table", () => {
    for (const [argv, reason, destructive] of CLASSIFIER_TABLE) {
      const verdict = M.classifyCliArgv(argv)
      expect([argv, verdict.ok, verdict.reason]).toEqual([argv, reason === "", reason])
      if (verdict.ok) expect([argv, verdict.destructive]).toEqual([argv, destructive])
    }
  })

  test("a destructive path runs only as a preview or with its token", () => {
    const argv = ["omaddons", "remove", "-t", "retail", "--", "Foo"]
    expect(reasonOf(() => M.cliArgv(argv, opts()))).toBe("destructive_without_preview")
    const preview = M.cliArgv(argv, opts({ preview: true })).argv
    expect(preview.slice(6, 9)).toEqual(["--progress", "json", "--dry-run"])
    expect(M.cliArgv(argv, opts({ token: TOKEN })).argv).toContain("--if-match")
    // targets rm binds nothing: an explicit confirmation runs it.
    const rm = ["omaddons", "targets", "rm", "retail"]
    expect(reasonOf(() => M.cliArgv(rm, opts()))).toBe("destructive_without_preview")
    expect(reasonOf(() => M.cliArgv(rm, opts({ token: TOKEN })))).toBe("token_not_bound")
    expect(M.cliArgv(rm, opts({ confirmed: true })).argv.slice(-3)).toEqual(["targets", "rm", "retail"])
    // An argv that is a preview already gets no second --dry-run.
    const dry = M.cliArgv(["omaddons", "remove", "--dry-run", "--", "Foo"], opts()).argv
    expect(dry.filter((a) => a === "--dry-run").length).toBe(1)
  })

  test("the argv in real envelopes", () => {
    const attention = M.reduceStatus(fixture("status-attention"))
    const remediation = attention.targets[0].issues[0].remediation
    expect(remediation.runnable).toBe(true)
    expect(M.cliArgv(remediation.argv, opts()).argv.slice(-5)).toEqual(["install", "Baz", "-t", "retail", "--reinstall"])
    const undo = M.reduceStatus(fixture("status-after-update")).lastChange.undo
    expect([undo.runnable, undo.destructive]).toEqual([true, true])
    // plan_stale's fix repeats the widget's own --actor and --reason: text, never a button.
    const stale = fixture("remove-stale")
    expect(M.classifyCliArgv(stale.error.fix_argv).reason).toBe("widget_flag")
    expect(M.fixButtons(stale)).toEqual([])
    expect(M.fixButtons(fixture("failure")).map((b) => b.argv)).toEqual([["omaddons", "targets"]])
  })
})

// --------------------------------------------------------- compatibility

describe("compatibility", () => {
  const env = fixture("status-updates")
  const build = { TOOL_VERSION: env.tool_version }

  test("the same build is compatible", () => {
    expect(M.envelopeOk(env)).toBe(true)
    expect(M.compatible(env, build)).toBe(true)
  })

  test("another tool version, another schema major or an outdated widget is not", () => {
    expect(M.compatible(env, { TOOL_VERSION: "0.0.0-other" })).toBe(false)
    expect(M.compatible(Object.assign({}, env, { schema_version: "2.0" }), build)).toBe(false)
    const outdated = JSON.parse(JSON.stringify(env))
    outdated.data.widget = { outdated: true }
    expect(M.compatible(outdated, build)).toBe(false)
    expect(M.incompatibleText(env, { TOOL_VERSION: "0.0.0-other" })).toContain("0.0.0-other")
    expect(M.compatible({ ok: true }, build)).toBe(false)
  })
})

// ------------------------------------------------------------ the scheduler

let nextId = 0
function req(fields) {
  nextId++
  return Object.assign({ id: nextId, contextGeneration: 1, target: "", resource: "status", kind: "data", priority: "user", selection: null }, fields)
}

describe("scheduler", () => {
  test("a late search for target A after a switch to B is dropped", () => {
    const a = req({ resource: "search", kind: "view", target: "retail", selection: { query: "dbm", target: "retail" } })
    const b = req({ resource: "search", kind: "view", target: "era", selection: { query: "dbm", target: "era" } })
    const current = { query: "dbm", target: "era" }
    expect(M.acceptResult(a, {}, current, 1)).toBe(false)
    expect(M.acceptResult(b, {}, current, 1)).toBe(true)
  })

  test("a doorbell flood leaves one queued status", () => {
    let q = M.emptyQueues()
    let replaced = 0
    for (let i = 0; i < 50; i++) {
      const r = M.enqueue(q, req({ resource: "status" }))
      q = r.queues
      if (r.replaced !== null) replaced++
    }
    expect(q.local.length).toBe(1)
    expect(replaced).toBe(49)
    expect(q.local[0].id).toBe(nextId)
  })

  test("a slow status loses to a newer one, and a newer one is never starved", () => {
    const slow = req({ resource: "status" })
    const fresh = req({ resource: "status" })
    let applied = {}
    expect(M.acceptResult(fresh, applied, null, 1)).toBe(true)
    applied = M.markApplied(applied, fresh)
    expect(M.acceptResult(slow, applied, null, 1)).toBe(false)
    // A result newer than the last applied applies though an even newer one is queued.
    const newer = req({ resource: "status" })
    req({ resource: "status" })
    expect(M.acceptResult(newer, applied, null, 1)).toBe(true)
  })

  test("a result from before a reinstall or context change is dropped", () => {
    expect(M.acceptResult(req({ contextGeneration: 1 }), {}, null, 2)).toBe(false)
    expect(M.acceptResult(req({ kind: "mutation", resource: "update_all", contextGeneration: 1 }), {}, null, 2)).toBe(false)
  })

  test("status jumps the local queue", () => {
    let q = M.emptyQueues()
    q = M.enqueue(q, req({ resource: "list" })).queues
    q = M.enqueue(q, req({ resource: "status" })).queues
    const next = M.nextRequest(q, {})
    expect(next.start.local.resource).toBe("status")
    expect(next.queues.local.map((r) => r.resource)).toEqual(["list"])
    expect(M.nextRequest(q, { local: true }).start.local).toBeNull()
  })

  test("user mutations go before background ones; one background per key; one queued user", () => {
    let q = M.emptyQueues()
    const auto1 = req({ resource: "update_target", kind: "mutation", priority: "background", target: "retail" })
    const auto2 = req({ resource: "update_target", kind: "mutation", priority: "background", target: "retail" })
    const auto3 = req({ resource: "update_target", kind: "mutation", priority: "background", target: "era" })
    for (const r of [auto1, auto2, auto3]) q = M.enqueue(q, r).queues
    expect(q.mutation.map((r) => r.id)).toEqual([auto2.id, auto3.id])
    const user1 = req({ resource: "update_all", kind: "mutation" })
    const user2 = req({ resource: "pin", kind: "mutation", target: "retail" })
    q = M.enqueue(q, user1).queues
    const second = M.enqueue(q, user2)
    expect(second.replaced).toBe(user1.id)
    q = second.queues
    expect(q.mutation.filter((r) => r.priority === "user").map((r) => r.id)).toEqual([user2.id])
    expect(M.nextRequest(q, {}).start.mutation.id).toBe(user2.id)
    expect(M.nextRequest(q, { mutation: true }).start.mutation).toBeNull()
  })

  test("a newer cached status never downgrades a queued full one", () => {
    let q = M.emptyQueues()
    q = M.enqueue(q, req({ resource: "status", template: "status_full" })).queues
    const cached = req({ resource: "status", template: "status" })
    q = M.enqueue(q, cached).queues
    expect(q.local.map((r) => [r.id, r.template])).toEqual([[cached.id, "status_full"]])
    expect(cached.template).toBe("status")
    q = M.enqueue(M.emptyQueues(), req({ resource: "status", template: "status" })).queues
    q = M.enqueue(q, req({ resource: "status", template: "status_full" })).queues
    expect(q.local[0].template).toBe("status_full")
  })

  test("the network lane holds four, and a newer search replaces a queued one", () => {
    let q = M.emptyQueues()
    q = M.enqueue(q, req({ resource: "search", kind: "view", target: "retail" })).queues
    const s2 = req({ resource: "search", kind: "view", target: "era" })
    q = M.enqueue(q, s2).queues
    expect(q.network.map((r) => r.id)).toEqual([s2.id])
    for (const target of ["a", "b", "c"]) q = M.enqueue(q, req({ resource: "changelog", kind: "view", target })).queues
    const full = M.enqueue(q, req({ resource: "check" }))
    expect([full.accepted, full.queues.network.length]).toEqual([false, 4])
    expect(M.laneOf(req({ resource: "check" }))).toBe("network")
    expect(M.laneOf(req({ resource: "update_all", kind: "mutation" }))).toBe("mutation")
  })

  test("outcomeOf", () => {
    const ok = fixtureText("update.json")
    const build = { TOOL_VERSION: JSON.parse(ok).tool_version }
    expect(M.outcomeOf(0, false, "", false, false)).toBe("missing")
    expect(M.outcomeOf(0, true, ok, false, true)).toBe("cancelled")
    expect(M.outcomeOf(15, true, "", true, false)).toBe("timeout")
    expect(M.outcomeOf(0, true, "x".repeat(M.WIDGET_OUTPUT_MAX_BYTES + 1), false, false)).toBe("oversized")
    expect(M.outcomeOf(0, true, "é".repeat(M.WIDGET_OUTPUT_MAX_BYTES / 2 + 1), false, false)).toBe("oversized")
    expect(M.outcomeOf(9, true, "", false, false)).toBe("malformed")
    expect(M.outcomeOf(0, true, "{\"half\":", false, false)).toBe("malformed")
    expect(M.outcomeOf(0, true, ok, false, false, { TOOL_VERSION: "0.0.0-other" })).toBe("incompatible")
    expect(M.outcomeOf(3, true, fixtureText("failure.json"), false, false, build)).toBe("failed")
    expect(M.outcomeOf(0, true, ok, false, false, build)).toBe("ok")
  })
})

// --------------------------------------------------------------- reducers

describe("reduceStatus", () => {
  test("updates", () => {
    const model = M.reduceStatus(fixture("status-updates"), NOW)
    expect(model.bar.badge).toBe(4)
    expect([model.bar.urgent, model.bar.dimmed, model.bar.spinning]).toEqual([false, false, false])
    expect(model.bar.tooltip[0]).toBe("4 updates on 2 clients (era 2, retail 2)")
    expect(model.summary.state).toBe("updates")
    expect(model.targets.map((t) => t.id)).toEqual(["era", "retail"])
    expect(model.commands.map((c) => [c.action, c.runnable])).toEqual([["update", true], ["list_unmanaged", true]])
    expect(model.lastChange.unknown).toBe(false)
  })

  test("idle", () => {
    const model = M.reduceStatus(fixture("status-idle"))
    expect([model.bar.badge, model.bar.urgent]).toEqual([0, false])
    expect(model.bar.tooltip).toEqual(["Everything is up to date"])
    expect(model.summary.state).toBe("idle")
  })

  test("attention", () => {
    const model = M.reduceStatus(fixture("status-attention"))
    expect(model.bar.urgent).toBe(true)
    expect(model.bar.tooltip[0]).toBe("retail: Baz's folder is gone")
    const issue = model.targets[0].issues[0]
    expect([issue.severity, issue.urgent, issue.addon]).toEqual(["P1", true, "Baz"])
  })

  test("a running client and the check's age", () => {
    const model = M.reduceStatus(fixture("status-running"), Date.parse(fixture("status-running").data.targets[0].updates.checked_at) + 25 * 60000)
    expect(model.bar.tooltip).toContain("retail is running: updates land, then /reload")
    expect(model.bar.tooltip).toContain("checked 25 min ago")
  })

  test("the last change and its undo", () => {
    const model = M.reduceStatus(fixture("status-after-update"))
    expect(model.lastChange.label).toBe("Undo update on era and retail")
    expect(model.lastChange.undo.argv[1]).toBe("undo")
    const redo = JSON.parse(JSON.stringify(fixture("status-after-update")))
    redo.data.last_change.kind = "undo"
    expect(M.reduceStatus(redo).lastChange.label.startsWith("Redo")).toBe(true)
  })

  test("an unknown last change is unknown, never nothing to undo", () => {
    const env = JSON.parse(JSON.stringify(fixture("status-idle")))
    env.data.last_change = null
    env.data.last_change_state = "unknown"
    expect(M.reduceStatus(env).lastChange).toEqual({ unknown: true })
    env.data.last_change_state = "ok"
    expect(M.reduceStatus(env).lastChange).toBeNull()
  })

  test("blocks the binary does not report yet are unknown, not zero", () => {
    const env = JSON.parse(JSON.stringify(fixture("status-updates")))
    delete env.data.summary.errors
    delete env.data.targets[0].errors
    delete env.data.targets[0].backups
    const model = M.reduceStatus(env)
    expect(model.summary.errors).toBeNull()
    expect([model.targets[0].errors, model.targets[0].backups, model.widget]).toEqual([null, null, null])
  })

  test("the badge hides at 0 and caps at 99+", () => {
    expect([0, -1, null, undefined, 1, 4, 99, 100, 120].map(M.badgeText)).toEqual(["", "", "", "", "1", "4", "99", "99+", "99+"])
  })

  test("a stale bar keeps its numbers, dimmed", () => {
    const bar = M.reduceStatus(fixture("status-attention")).bar
    const stale = M.staleBar(bar)
    expect([stale.badge, stale.urgent, stale.dimmed, stale.stale, stale.spinning]).toEqual([bar.badge, true, true, true, false])
    expect(stale.tooltip).toEqual(bar.tooltip)
    expect(stale.tooltip).not.toBe(bar.tooltip)
    expect(M.staleBar(null).dimmed).toBe(true)
  })

  test("a failed status keeps nothing and says why", () => {
    const model = M.reduceStatus(fixture("failure"))
    expect([model.bar.dimmed, model.summary, model.targets.length]).toEqual([true, null, 0])
    expect(model.bar.tooltip).toEqual(['no target matches "nosuch"'])
  })
})

describe("the other reducers", () => {
  test("list", () => {
    const model = M.reduceList(fixture("list"))
    const retail = model.targets.filter((row) => row.id.startsWith("retail"))[0]
    expect(retail.addons.map((a) => [a.name, a.updateAvailable])).toEqual([["Bar", true], ["Baz", false], ["Foo", true]])
    expect(retail.counts.unmanaged).toBe(1)
  })

  test("search", () => {
    const model = M.reduceSearch(fixture("search"))
    expect(model.query).toBe("damage meter")
    expect(model.rows.map((r) => [r.ref, r.source])).toContainEqual(["gh:Skada-Damage-Meter/Skada", "github"])
    for (const row of model.rows) expect(typeof row.compat).toBe("string")
  })

  test("errors and backups read the documented shapes, never accounts", () => {
    const errors = M.reduceErrors({
      ok: true, schema_version: "1.0", tool_version: "x", command: "errors",
      data: { targets: [{ target: { id: "retail" }, capture: "buggrabber", coverage: "complete", dismiss: { precondition: TOKEN },
        databases: [{ account: "account 1" }],
        addons: [{ addon: "Foo", managed: true, count: 5, new: 1, errors: [{ fingerprint: "e:1", message: "boom", location: "Foo/a.lua:1", count: 4, new: true, stack: ["x"] }] }] }] },
    })
    expect(errors.targets[0].dismissToken).toBe(TOKEN)
    expect(errors.targets[0].addons[0].errors[0].message).toBe("boom")
    expect(JSON.stringify(errors)).not.toContain("account")
    const backups = M.reduceBackups({
      ok: true, schema_version: "1.0", tool_version: "x", command: "backup list",
      data: { targets: [{ target: { id: "retail" }, total: 1, bytes: 200, backups: [{ id: "01M", captured_at: "t", kind: "manual", trigger: null, files: 2, bytes: 200, restorable: true, qualifying: true }] }] },
    })
    expect(backups.targets[0].backups[0].restorable).toBe(true)
  })

  test("progress and result lines", () => {
    const lines = fixtureText("update.stderr.ndjson").trim().split("\n").map((l) => M.progressLine(JSON.parse(l)))
    expect(lines[0]).toBe("download 1/4 · gh:o/bar@v2 on era")
    expect(M.progressLine({ event: "log", message: "x" })).toBe("")
    expect(M.resultLine(fixture("update"))).toBe("Updated 4 addons on 2 clients")
    expect(M.resultLine(fixture("failure"))).toBe('no target matches "nosuch"')
    const reload = JSON.parse(JSON.stringify(fixture("update")))
    reload.data.targets[1].reload_required = "reload"
    expect(M.resultLine(reload)).toBe("Updated 4 addons on 2 clients · /reload in game")
  })

  test("confirmation and stale texts", () => {
    expect(M.confirmText("remove", fixture("remove-preview"))).toBe("Remove Baz v1 from retail (1 folder)")
    expect(M.confirmText("undo", fixture("undo-preview"))).toBe("Undo update on era and retail: 2 folders restored, 0 removed")
    expect(M.staleText(fixture("remove-stale"))).toBe("Changed since you looked: command, targets, selection and observed. Nothing ran; check the new preview.")
  })

  test("only https links to known hosts open", () => {
    expect(M.isSafeUrl("https://github.com/o/r")).toBe(true)
    expect(M.isSafeUrl("https://www.wowinterface.com/downloads/info1.html")).toBe(true)
    for (const url of ["http://github.com/o/r", "https://github.com.evil.example/x", "https://user@github.com/x",
      "https://github.com:8443/x", "javascript:alert(1)", "https://evil.example/https://github.com", "", null]) {
      expect([url, M.isSafeUrl(url)]).toEqual([url, false])
    }
  })
})

// --------------------------------------------------- built-argv.json (step 3)

test("writes every built argv and the classifier's verdicts for the Rust check", () => {
  const built = []
  const refused = []
  const record = (template, slots, extra) => {
    try {
      const o = Object.assign(opts(), extra || {})
      const out = M.buildArgv(template, slots, o)
      const preview = Boolean(o.preview)
      const ifMatch = o.token || ""
      built.push({ template, slots, preview, token: ifMatch, path: M.templateShape(template).path, mutates: M.COMMANDS[template].mutates, argv: out.argv })
    } catch (e) {
      refused.push({ template, slots, reason: e.reason || e.message })
    }
  }
  for (const name of Object.keys(M.COMMANDS)) {
    const base = benignSlots(name)
    const extra = runOpts(name)
    record(name, base, { token: extra.token })
    if (M.COMMANDS[name].confirm) record(name, base, { preview: true })
    for (const slot of M.templateShape(name).slots) {
      if (slot === "target") continue
      for (const value of HOSTILE) record(name, Object.assign({}, base, { [slot]: value }), { token: extra.token })
    }
  }
  expect(refused).toEqual([])

  const classified = []
  const real = [
    M.reduceStatus(fixture("status-attention")).targets[0].issues[0].remediation.argv,
    fixture("status-after-update").data.last_change.undo.argv,
    fixture("failure").error.fix_argv,
    fixture("remove-stale").error.fix_argv,
  ].concat(fixture("update").commands.map((c) => c.argv))
  for (const argv of CLASSIFIER_TABLE.map((row) => row[0]).concat(real)) {
    const verdict = M.classifyCliArgv(argv)
    let runnable = null
    let refusal = ""
    if (verdict.ok) {
      try {
        runnable = M.cliArgv(argv, opts()).argv
      } catch (e) {
        refusal = e.reason
        runnable = M.cliArgv(argv, opts({ preview: true })).argv
      }
    }
    classified.push({ argv, verdict, runnable, refusal })
  }
  const stringArgv = (argv) => argv.every((a) => typeof a === "string")
  const doc = { binary: BINARY, built, classified: classified.filter((c) => stringArgv(c.argv)) }
  fs.writeFileSync(path.join(OUT, "built-argv.json"), JSON.stringify(doc, null, 1))
  expect(built.length).toBeGreaterThan(Object.keys(M.COMMANDS).length * 5)
  expect(classified.some((c) => c.refusal === "destructive_without_preview")).toBe(true)
})
