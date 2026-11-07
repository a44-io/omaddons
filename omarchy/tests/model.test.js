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
// A stored plan's id, in the ULID form the CLI gives it.
const PLAN_ID = "01M8Q2ZZZZZZZZZZZZZZZZZZZZ"

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

// What a template needs beyond its slots to build: a previewed template builds as its
// preview (its Yes is its plan's apply), a bound one with its view's token.
function runOpts(name) {
  const t = M.COMMANDS[name]
  if (t.preview) return opts({ preview: true })
  if (t.bind || M.CLI_PATHS[M.templateShape(name).path].destructive) return opts({ token: TOKEN })
  return opts()
}

// The templates whose real run is refused: they run as their preview, then apply.
const PREVIEWED = ["install", "prune_backup", "reinstall", "remove", "restore", "restore_addon", "update_addon", "update_all", "update_target"]

function benignSlots(name) {
  return Object.fromEntries(M.templateShape(name).slots.map((s) => [s, BENIGN[s]]))
}

describe("buildArgv", () => {
  test("puts the standard flags right after the binary", () => {
    const built = M.buildArgv("update_addon", { target: "retail", addon: "DBM" }, opts({ label: "Update", preview: true }))
    expect(built.argv).toEqual([
      BINARY, "--json", "--actor", "widget", "--reason", "Update (bar widget)", "--progress", "json", "--dry-run",
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

  test("a preview adds --dry-run, a bound run its token", () => {
    const slots = { target: "retail", addon: "Baz" }
    const preview = M.buildArgv("remove", slots, opts({ preview: true })).argv
    expect(preview.slice(0, 9)).toEqual([BINARY, "--json", "--actor", "widget", "--reason", "Test (bar widget)", "--progress", "json", "--dry-run"])
    const run = M.buildArgv("dismiss_errors", { target: "retail" }, opts({ token: TOKEN })).argv
    expect(run.slice(8, 10)).toEqual(["--if-match", TOKEN])
    expect(run).not.toContain("--dry-run")
  })

  test("buildArgv refuses a previewed template's real run", () => {
    expect(Object.keys(M.COMMANDS).filter((name) => M.COMMANDS[name].preview).sort()).toEqual(PREVIEWED)
    for (const name of PREVIEWED) {
      for (const extra of [{}, { confirmed: true }, { token: TOKEN }]) {
        expect([name, extra, reasonOf(() => M.buildArgv(name, benignSlots(name), opts(extra)))]).toEqual([name, extra, "preview_required"])
      }
      expect([name, M.buildArgv(name, benignSlots(name), opts({ preview: true })).argv.includes("--dry-run")]).toEqual([name, true])
    }
  })

  test("refuses what the templates do not allow", () => {
    const remove = { target: "retail", addon: "Baz" }
    const dismiss = { target: "retail" }
    expect(reasonOf(() => M.buildArgv("remove", remove, opts()))).toBe("preview_required")
    expect(reasonOf(() => M.buildArgv("remove", remove, opts({ confirmed: true })))).toBe("preview_required")
    expect(reasonOf(() => M.buildArgv("dismiss_errors", dismiss, opts({ token: "pc1:abc" })))).toBe("bad_token")
    expect(reasonOf(() => M.buildArgv("dismiss_errors", dismiss, opts({ token: TOKEN.toUpperCase() })))).toBe("bad_token")
    expect(reasonOf(() => M.buildArgv("dismiss_errors", dismiss, opts()))).toBe("token_required")
    expect(reasonOf(() => M.buildArgv("remove", remove, opts({ preview: true, token: TOKEN })))).toBe("token_in_preview")
    // `backup create` never binds (M4-D22).
    expect(reasonOf(() => M.buildArgv("backup_all", {}, opts({ token: TOKEN })))).toBe("token_not_bound")
    expect(reasonOf(() => M.buildArgv("nosuch", {}, opts()))).toBe("unknown_template")
    expect(reasonOf(() => M.buildArgv("update_target", {}, opts()))).toBe("unfilled_slot")
    expect(reasonOf(() => M.buildArgv("update_all", { target: "retail" }, opts()))).toBe("unknown_slot")
    expect(reasonOf(() => M.buildArgv("update_target", { target: "retail", evil: "x" }, opts()))).toBe("unknown_slot")
    expect(reasonOf(() => M.buildArgv("update_addon", { target: "retail", addon: "" }, opts()))).toBe("bad_value")
    expect(reasonOf(() => M.buildArgv("update_addon", { target: "retail", addon: "a\u0000b" }, opts()))).toBe("bad_value")
    expect(reasonOf(() => M.buildArgv("update_all", {}, opts({ preview: true, binary: "omaddons" })))).toBe("bad_binary")
    expect(reasonOf(() => M.buildArgv("update_all", {}, opts({ preview: true, label: "" })))).toBe("bad_label")
    expect(reasonOf(() => M.buildArgv("update_all", {}, opts({ preview: true, label: "a\nb" })))).toBe("bad_label")
    expect(reasonOf(() => M.buildArgv("update_all", {}, opts({ preview: true, context: { HOME: "/h" } })))).toBe("bad_context")
  })

  test("a bad {target} is refused", () => {
    for (const target of ["-t", "--json", "re tail", "retail;rm", "ünï", "", ".hidden"]) {
      expect(reasonOf(() => M.buildArgv("update_target", { target }, opts()))).toBe(target === "" ? "bad_value" : "bad_target")
    }
    // Ids and keys pass.
    expect(M.buildArgv("update_target", { target: "01M5JHFF37RESHT6QWM5BT2SXW" }, opts({ preview: true })).argv).toContain("01M5JHFF37RESHT6QWM5BT2SXW")
  })

  test("hostile values stay one argv element, after -- or in --opt=", () => {
    for (const name of Object.keys(M.COMMANDS)) {
      for (const slot of M.templateShape(name).slots) {
        if (slot === "target") continue
        for (const value of HOSTILE) {
          const slots = Object.assign(benignSlots(name), { [slot]: value })
          const argv = M.buildArgv(name, slots, runOpts(name)).argv
          // An option slot is one element (`--addon=<value>`, even for "--json");
          // a positional comes after `--`.
          const optionPart = M.COMMANDS[name].argv.find((part) => part.endsWith("={" + slot + "}"))
          if (optionPart) {
            expect(argv).toContain(optionPart.replace("{" + slot + "}", () => value))
          } else {
            expect(argv.indexOf("--")).toBeGreaterThan(-1)
            expect(argv.lastIndexOf(value)).toBeGreaterThan(argv.indexOf("--"))
          }
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
      const t = M.COMMANDS[name]
      expect(M.CLI_PATHS[shape.path]).toBeDefined()
      expect(t.mutates).toBe(M.CLI_PATHS[shape.path].mutates)
      // M4-D28: a preview template is on a plan-storing path, a destructive one asks
      // first, and the old confirm key is gone.
      expect([name, t.confirm]).toEqual([name, undefined])
      if (t.preview) expect([name, ["match", "confirm"].includes(t.preview), M.storesPlan(shape.path)]).toEqual([name, true, true])
      if (M.CLI_PATHS[shape.path].destructive) expect([name, t.preview]).toEqual([name, "confirm"])
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
  // A plan's apply runs only with the plan that offered it (M4-D28).
  [["omaddons", "apply", PLAN_ID], "apply_outside_plan", false],
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

  test("a destructive path runs only as a preview, then through its plan", () => {
    const argv = ["omaddons", "remove", "-t", "retail", "--", "Foo"]
    expect(reasonOf(() => M.cliArgv(argv, opts()))).toBe("destructive_without_preview")
    const preview = M.cliArgv(argv, opts({ preview: true })).argv
    expect(preview.slice(6, 9)).toEqual(["--progress", "json", "--dry-run"])
    // A plan-storing path confirms through its preview's apply, never a token (M4-D28).
    expect(reasonOf(() => M.cliArgv(argv, opts({ token: TOKEN })))).toBe("runs_through_apply")
    // targets rm binds nothing: an explicit confirmation runs it.
    const rm = ["omaddons", "targets", "rm", "retail"]
    expect(reasonOf(() => M.cliArgv(rm, opts()))).toBe("destructive_without_preview")
    expect(reasonOf(() => M.cliArgv(rm, opts({ token: TOKEN })))).toBe("token_not_bound")
    expect(M.cliArgv(rm, opts({ confirmed: true })).argv.slice(-3)).toEqual(["targets", "rm", "retail"])
    // An argv that is a preview already gets no second --dry-run.
    const dry = M.cliArgv(["omaddons", "remove", "--dry-run", "--", "Foo"], opts()).argv
    expect(dry.filter((a) => a === "--dry-run").length).toBe(1)
    // A dry run's own alternative (it repeats --dry-run) previews; a token never confirms
    // it (backup restore stores a plan).
    const alt = ["omaddons", "backup", "restore", "01M", "-t", "retail", "--addon", "folder:WeakAuras", "--dry-run"]
    expect(M.cliArgv(alt, opts({ preview: true })).argv.filter((a) => a === "--dry-run").length).toBe(1)
    expect(reasonOf(() => M.cliArgv(alt, opts({ token: TOKEN })))).toBe("runs_through_apply")
    // Off the plan paths, a confirmed or bound run drops the alternative's --dry-run; a
    // "--dry-run" after -- is a positional and stays.
    const rmAlt = M.cliArgv(["omaddons", "targets", "rm", "retail", "--dry-run"], opts({ confirmed: true })).argv
    expect(rmAlt).not.toContain("--dry-run")
    const bound = M.cliArgv(["omaddons", "errors", "dismiss", "-t", "retail", "--dry-run"], opts({ token: TOKEN })).argv
    expect([bound.includes("--dry-run"), bound.includes("--if-match")]).toEqual([false, true])
    const literal = M.cliArgv(["omaddons", "targets", "rm", "--", "--dry-run"], opts({ confirmed: true })).argv
    expect(literal.slice(-2)).toEqual(["--", "--dry-run"])
  })

  test("cliArgv keeps tokens off plan paths", () => {
    table([
      ["remove", ["omaddons", "remove", "-t", "retail", "--", "Foo"], "runs_through_apply"],
      ["undo", ["omaddons", "undo", "--run", "01M5JHFF8X0FXBT1K068DQKCHG"], "runs_through_apply"],
      ["backup_restore", ["omaddons", "backup", "restore", "01M6Q2VA", "-t", "retail"], "runs_through_apply"],
      ["backup_prune", ["omaddons", "backup", "prune", "-t", "retail", "--", "01M6Q2VA"], "runs_through_apply"],
      ["update", ["omaddons", "update", "-t", "retail"], "runs_through_apply"],
      ["errors_dismiss_builds", ["omaddons", "errors", "dismiss", "-t", "retail"], null],
    ], (argv) => reasonOf(() => M.cliArgv(argv, opts({ token: TOKEN }))))
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

  // A continuation carries on a preview (its plan, or the preview again): it goes first
  // and replaces nothing.
  const queuedChanges = () => {
    let q = M.emptyQueues()
    const user = req({ resource: "update_all", kind: "mutation" })
    const background = req({ resource: "backup_auto", kind: "mutation", priority: "background", target: "retail" })
    q = M.enqueue(q, background).queues
    q = M.enqueue(q, user).queues
    return { q, user, background }
  }

  test("continuation_goes_first", () => {
    const { q, user, background } = queuedChanges()
    const next = req({ resource: "update_addon", kind: "mutation", continuation: true })
    const r = M.enqueue(q, next)
    expect(["continuation_goes_first", r.accepted, r.queues.mutation.map((x) => x.id)])
      .toEqual(["continuation_goes_first", true, [next.id, background.id, user.id]])
  })

  test("continuation_replaces_nothing", () => {
    const { q, user } = queuedChanges()
    const r = M.enqueue(q, req({ resource: "update_all", kind: "mutation", continuation: true }))
    expect(["continuation_replaces_nothing", r.replaced, r.queues.mutation.some((x) => x.id === user.id)])
      .toEqual(["continuation_replaces_nothing", null, true])
    // Nor does a newer user request replace a queued continuation (decided in 7.1.2).
    const later = req({ resource: "pin", kind: "mutation", target: "retail" })
    const after = M.enqueue(r.queues, later)
    expect(["a_user_request_never_replaces_a_continuation", after.replaced, after.queues.mutation.filter((x) => x.continuation).length])
      .toEqual(["a_user_request_never_replaces_a_continuation", user.id, 1])
  })

  test("continuation_then_next_request_starts_it", () => {
    const { q } = queuedChanges()
    const next = req({ resource: "update_addon", kind: "mutation", continuation: true })
    const started = M.nextRequest(M.enqueue(q, next).queues, {})
    expect(["continuation_then_next_request_starts_it", started.start.mutation.id, started.queues.mutation.length])
      .toEqual(["continuation_then_next_request_starts_it", next.id, 2])
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

  test("no clients: the tooltip says so, never \"up to date\"", () => {
    const none = JSON.parse(JSON.stringify(fixture("status-idle")))
    none.data.targets = []
    none.data.summary.targets = 0
    expect(M.reduceStatus(none, NOW).bar.tooltip).toEqual(["No World of Warcraft clients found"])
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

describe("the panel frame", () => {
  test("every letter of the key map, and nothing else", () => {
    expect(M.panelKey("u")).toEqual({ name: "update" })
    expect(M.panelKey("/")).toEqual({ name: "search" })
    expect(M.panelKey("3")).toEqual({ name: "view", view: "issues" })
    expect(M.panelKey("5")).toEqual({ name: "view", view: "search" })
    for (const reserved of ["j", "k", "h", "l", "x", "q", "6", "0", "U", "", "uu"]) expect(M.panelKey(reserved)).toBeNull()
    expect(Object.keys(M.PANEL_KEYS).sort()).toEqual(["/", "b", "c", "d", "i", "o", "p", "r", "s", "t", "u", "z"])
    expect(M.VIEWS.map((v) => M.VIEW_TITLES[v])).toEqual(["Updates", "Installed", "Issues", "Backups", "Search"])
  })

  test("views step and wrap", () => {
    expect(M.stepView("updates", 1)).toBe("installed")
    expect(M.stepView("updates", -1)).toBe("search")
    expect(M.stepView("search", 1)).toBe("updates")
    expect(M.stepView("nonsense", 1)).toBe("installed")
  })

  test("the hero speaks the summary", () => {
    const env = fixture("status-updates")
    const model = M.reduceStatus(env, NOW)
    expect(M.heroMeta(model.summary)).toBe("2 clients · 4 updates")
    expect(M.heroMeta(null)).toBe("")
    const running = fixture("status-running")
    const at = Date.parse(running.data.targets[0].updates.checked_at) + 25 * 60000
    const r = M.reduceStatus(running, at)
    expect(M.heroDetail(r.summary, r.targets, at)).toBe("checked 25 min ago · retail running")
    const unknown = JSON.parse(JSON.stringify(running))
    unknown.data.summary.running = []
    for (const t of unknown.data.targets) t.running = null
    const u = M.reduceStatus(unknown, at)
    expect(u.targets[0].runningUnknown).toBe(true)
    expect(M.heroDetail(u.summary, u.targets, at)).toBe("checked 25 min ago · running: unknown")
    const issues = M.reduceStatus(fixture("status-attention"), NOW)
    expect(M.heroMeta(issues.summary)).toContain("1 issue")
  })

  test("the filter cycles All, each target, All", () => {
    const targets = M.reduceStatus(fixture("status-updates"), NOW).targets
    expect(M.nextFilter(targets, "")).toBe("era")
    expect(M.nextFilter(targets, "era")).toBe("retail")
    expect(M.nextFilter(targets, "retail")).toBe("")
    expect(M.nextFilter(targets, "gone")).toBe("")
    expect(M.nextFilter([], "")).toBe("")
    expect(M.targetById(targets, "retail").id).toBe("retail")
    expect(M.targetById(targets, "")).toBeNull()
  })

  test("o opens the selected client's AddOns, nothing without one", () => {
    const retail = M.targetById(M.reduceStatus(fixture("status-updates"), NOW).targets, "retail")
    expect(retail.clientDir.startsWith("/")).toBe(true)
    expect(M.addonsDirArgv(retail)).toEqual(["xdg-open", retail.clientDir + "/Interface/AddOns"])
    expect(M.addonsDirArgv({ clientDir: "/c/wow/" })).toEqual(["xdg-open", "/c/wow/Interface/AddOns"])
    expect(M.addonsDirArgv(null)).toBeNull()
    expect(M.addonsDirArgv({ clientDir: "" })).toBeNull()
    expect(M.addonsDirArgv({ clientDir: "-x" })).toBeNull()
  })

  test("u updates all, or the selected client", () => {
    expect(M.updateRequest("")).toEqual({ template: "update_all", slots: {}, label: "Update all" })
    expect(M.updateRequest("retail")).toEqual({ template: "update_target", slots: { target: "retail" }, label: "Update retail" })
    // An update runs as its preview, then its plan's apply (M4-D28).
    for (const r of [M.updateRequest(""), M.updateRequest("retail")]) {
      expect(() => M.buildArgv(r.template, r.slots, opts({ preview: true }))).not.toThrow()
      expect(reasonOf(() => M.buildArgv(r.template, r.slots, opts()))).toBe("preview_required")
    }
  })

  test("undo last: its label, its argv, or why not", () => {
    const after = M.reduceStatus(fixture("status-after-update"), NOW)
    const undo = M.undoAction(after.lastChange)
    expect(undo.label).toBe("Undo update on era and retail")
    expect(undo.argv[1]).toBe("undo")
    expect(undo.why).toBe("")
    expect(M.undoAction({ unknown: true })).toEqual({ label: "Undo last", argv: null, why: "last change: unknown" })
    expect(M.undoAction(null).why).toBe("nothing to undo")
    const refused = JSON.parse(JSON.stringify(after.lastChange))
    refused.undo = { command: "rm -rf /", argv: null, runnable: false, destructive: true }
    expect(M.undoAction(refused).argv).toBeNull()
    expect(M.undoAction(refused).why).toContain("not a command the widget runs")
  })

  test("progress, the result's undo and a fix as text", () => {
    expect(M.progressFraction({ current: 1, total: 4 })).toBe(0.25)
    expect(M.progressFraction({ current: 9, total: 4 })).toBe(1)
    expect(M.progressFraction({ current: 0, total: 0 })).toBe(-1)
    expect(M.progressFraction(null)).toBe(-1)
    const undo = { action: "undo", command: "omaddons undo", argv: ["omaddons", "undo"], runnable: true }
    expect(M.resultUndo({ commands: [{ action: "update", runnable: true, argv: ["omaddons", "update"] }, undo] })).toBe(undo)
    expect(M.resultUndo({ commands: [Object.assign({}, undo, { runnable: false, argv: null })] })).toBeNull()
    // A change on several clients: the run's undo, never one client's.
    const run = { action: "undo_run", command: "omaddons undo --run 01M5", argv: ["omaddons", "undo", "--run", "01M5"], runnable: true }
    expect(M.resultUndo({ commands: [run, undo] })).toBe(run)
    expect(M.resultUndo(null)).toBeNull()
    expect(M.fixText({ ok: false, error: { fix: "this needs the Omarchy desktop", fix_argv: null } })).toBe("this needs the Omarchy desktop")
    expect(M.fixText({ ok: false, error: { fix: "omaddons status", fix_argv: ["omaddons", "status"] } })).toBe("")
    expect(M.fixText({ ok: false, error: { fix: "rm it", fix_argv: ["rm", "it"] } })).toBe("rm it")
    expect(M.fixText({ ok: true })).toBe("")
  })
})

describe("the Updates and Installed views", () => {
  const STATUS = () => M.reduceStatus(fixture("status-updates"), NOW).targets

  test("a ref's source kind", () => {
    expect(["gh:o/r", "wowi:123", "tukui:elvui", "git:host/o/r", "wowace:bug-grabber", "https://x/y.zip", "./Foo.zip", "", null]
      .map(M.sourceKind)).toEqual(["gh", "wowi", "tukui", "git", "wowace", "file", "file", "file", "file"])
  })

  test("updates per client: updates first, grouped in All, filtered otherwise", () => {
    const lists = M.reduceList(fixture("list"))
    const all = M.updateRows(lists, "", STATUS())
    expect(all.map((r) => [r.group, r.target, r.name, r.kind, r.version, r.latest]))
      .toEqual([["era", "era", "Bar", "update", "v1", "v2"], ["", "era", "Foo", "update", "v1", "v2"],
        ["retail", "retail", "Bar", "update", "v1", "v2"], ["", "retail", "Foo", "update", "v1", "v2"]])
    expect(M.updateCount(all)).toBe(4)
    const retail = M.updateRows(lists, "retail", STATUS())
    expect(retail.map((r) => [r.group, r.name])).toEqual([["", "Bar"], ["", "Foo"]])
    expect(M.updateCount(retail)).toBe(2)
    expect(M.updateRows(null, "", [])).toEqual([])
  })

  test("pinned with a newer release, blocked and failed follow, labelled", () => {
    const env = JSON.parse(JSON.stringify(fixture("list-markers")))
    const addons = env.data.targets[0].addons
    addons.push(Object.assign({}, addons[0], { name: "Zed", latest: { version: "v9", status: "blocked" } }))
    addons.push(Object.assign({}, addons[0], { name: "Aaa", latest: { version: null, status: "failed" } }))
    addons.push(Object.assign({}, addons[0], { name: "Old", pinned: true, latest: { version: "v1", status: "pinned" } }))
    const running = JSON.parse(JSON.stringify(fixture("status-running")))
    const rows = M.updateRows(M.reduceList(env), "retail", M.reduceStatus(running, NOW).targets)
    expect(rows.map((r) => [r.name, r.kind, r.label])).toEqual([
      ["Bar", "update", ""], ["Foo", "update", ""], ["Baz", "pinned", "pinned"], ["Zed", "blocked", "blocked"], ["Aaa", "failed", "check failed"]])
    expect(rows.every((r) => r.reload)).toBe(true)
  })

  test("nothing to update says when it was checked", () => {
    const targets = M.reduceStatus(fixture("status-running"), NOW).targets
    const at = Date.parse(targets[0].updates.checkedAt) + 25 * 60000
    expect(M.updatesEmptyText(targets, "", at)).toBe("Everything is current · checked 25 min ago")
    expect(M.updatesEmptyText(targets, "nosuch", at)).toBe("Everything is current")
  })

  test("installed rows carry their markers and Enter's action", () => {
    const env = JSON.parse(JSON.stringify(fixture("list-markers")))
    env.data.targets[0].addons[1].reason = "dependency"
    env.data.targets[0].addons[1].orphan = true
    const rows = M.installedRows(M.reduceList(env), "")
    expect(rows.map((r) => [r.group, r.name, r.source, r.markers, r.primary])).toEqual([
      ["retail", "Bar", "gh", ["missing"], "reinstall"],
      ["", "Baz", "gh", ["dep", "orphan", "pinned"], ""],
      ["", "Foo", "gh", ["modified"], "reinstall"]])
    const plain = M.installedRows(M.reduceList(fixture("list")), "retail")
    expect(plain.map((r) => [r.name, r.primary])).toEqual([["Bar", "update"], ["Baz", ""], ["Foo", "update"]])
    expect(M.unmanagedLines(M.reduceList(fixture("list")), "")).toEqual(["1 unmanaged folder on retail: omaddons adopt -t retail"])
    expect(M.unmanagedLines(M.reduceList(fixture("list")), "era")).toEqual([])
  })

  test("row actions are templates with the row's slots, and build", () => {
    const row = { target: "retail", name: "-rf Foo", pinned: false }
    expect(M.rowAction(row, "pin").template).toBe("pin")
    expect(M.rowAction(Object.assign({}, row, { pinned: true }), "pin").template).toBe("unpin")
    for (const which of ["pin", "update", "reinstall", "remove"]) {
      const action = M.rowAction(row, which)
      const built = M.buildArgv(action.template, action.slots, opts({ preview: !!M.COMMANDS[action.template].preview }))
      expect(built.argv.slice(-2)).toEqual(["--", "-rf Foo"])
    }
    expect(M.rowAction(row, "nonsense")).toBeNull()
  })

  test("release notes only for the row they were asked for, links only when safe", () => {
    const data = fixture("changelog").data
    const row = { target: "retail", name: "Foo" }
    const notes = M.notesFor({ selection: { target: "retail", addon: "Foo" }, data: data }, row)
    expect(notes).toEqual({ version: "v2", text: "Fixes the pull timer.", url: "https://github.com/o/foo/releases", link: "more on GitHub" })
    expect(M.notesFor({ selection: { target: "retail", addon: "Bar" }, data: data }, row)).toBeNull()
    expect(M.notesFor(null, row)).toBeNull()
    // A read that failed says so in the row, which would otherwise wait forever.
    const failed = M.notesFor({ selection: { target: "retail", addon: "Foo" }, data: null, failed: "timeout" }, row)
    expect([failed.text, failed.url]).toEqual(["The release notes could not be read: timeout", ""])
    const hostile = JSON.parse(JSON.stringify(data))
    hostile.changelog.more_url = "http://github.com/o/foo"
    hostile.changelog.entries[0].text = "x".repeat(900)
    const cut = M.notesFor({ selection: { target: "retail", addon: "Foo" }, data: hostile }, row)
    expect([cut.url, cut.link, cut.text.length]).toEqual(["", "", 401])
  })
})

describe("the Issues and Search views", () => {
  const ERRORS = () => M.reduceErrors(fixture("errors"))

  test("strict errors: code text only, a custom message as its length, received as a count", () => {
    const model = ERRORS()
    const text = JSON.stringify(model)
    for (const planted of ["FakeChar", "FakeSender", "FAKEACCT", "account 1"]) expect(text).not.toContain(planted)
    expect(model.raw).toBe(false)
    const [era, retail] = model.targets
    expect([era.id, era.capture, era.install.argv]).toEqual(["era", "none", ["omaddons", "install", "buggrabber", "-t", "era"]])
    expect([retail.new, retail.received, retail.partial, retail.install]).toEqual([2, 1, false, null])
    const foo = retail.addons[0]
    expect([foo.addon, foo.updateAvailable, foo.latest, foo.confidence]).toEqual(["Foo", true, "v2", "high"])
    expect(foo.errors.map(M.errorLine)).toEqual([
      "Foo/core.lua:12: attempt to index a nil value (field 'x')",
      "Foo/boss.lua:3: custom message (25 characters, hidden)"])
    expect(foo.errors.map((e) => e.raw)).toEqual([null, null])
  })

  test("raw details only from their own read, and a partial read says so", () => {
    const raw = M.reduceErrors(fixture("errors-raw"))
    expect(raw.raw).toBe(true)
    const retail = M.targetById(raw.targets, "retail")
    expect(retail.addons[0].errors[1].raw.message).toContain("FakeChar")
    const partial = M.targetById(M.reduceErrors(fixture("errors-partial")).targets, "retail")
    expect(partial.partial).toBe(true)
  })

  test("the Issues sections: problems by severity, each client's errors, the run warnings", () => {
    const status = M.reduceStatus(fixture("status-attention"), NOW)
    const sections = M.issueSections(status.targets, ERRORS(), [
      { code: "journal_pending", message: "unfinished transactions", fix: { command: "omaddons status", argv: ["omaddons", "status"], runnable: true }, fixText: "" }], "")
    expect(sections.map((s) => s.title)).toEqual(["Problems", "Lua errors (era)", "Lua errors (retail)", "Run warnings"])
    const rows = M.issueRows(sections)
    expect(rows.map((r) => [r.index, r.kind])).toEqual([[0, "problem"], [1, "capture"], [2, "addon"], [3, "warning"]])
    expect(rows[0].action.argv[1]).toBe("install")
    expect(rows[1].action.argv).toEqual(["omaddons", "install", "buggrabber", "-t", "era"])
    expect(sections[2].lines).toEqual(["1 error received from another player (not counted)"])
    const retailOnly = M.issueSections(status.targets, ERRORS(), [], "era")
    expect(retailOnly.map((s) => s.title)).toEqual(["Lua errors (era)"])
  })

  test("a problem of no client is listed first among the problems, unless narrowed", () => {
    const env = JSON.parse(JSON.stringify(fixture("status-attention")))
    env.data.issues = [{ id: "fm-journal-pending", severity: "P0", title: "An interrupted change needs manual repair",
      remediation: { command: "omaddons status", argv: ["omaddons", "status"], destructive: false } }]
    const status = M.reduceStatus(env, NOW)
    expect(status.issues.map((i) => [i.severity, i.urgent])).toEqual([["P0", true]])
    const rows = M.issueRows(M.issueSections(status.targets, null, [], "", status.issues))
    expect([rows[0].target, rows[0].title]).toEqual(["", "An interrupted change needs manual repair"])
    const narrowed = M.issueRows(M.issueSections(status.targets, null, [], "retail", status.issues))
    expect(narrowed.every((r) => r.target !== "")).toBe(true)
  })

  // An issue's remediation reaches the row through cliAction: a forced repair (M4-D38)
  // is text until its explanation has been read, never one key away.
  const remediated = (argv) => {
    const env = JSON.parse(JSON.stringify(fixture("status-attention")))
    env.data.targets[0].issues[0].remediation = { command: argv.join(" "), argv: argv, destructive: false }
    return M.reduceStatus(env, NOW).targets[0].issues[0].remediation
  }
  const FIX = ["omaddons", "doctor", "fix", "--only", "fm-x", "-t", "retail"]

  test("cliAction refuses an argv holding --force", () => {
    const forced = remediated(FIX.concat(["--force", "--yes"]))
    expect([forced.runnable, forced.argv, forced.command]).toEqual([false, null, FIX.concat(["--force", "--yes"]).join(" ")])
  })

  test("cliAction keeps an argv without --force", () => {
    const fix = remediated(FIX)
    expect([fix.runnable, fix.argv]).toEqual([true, FIX])
  })

  test("--force after -- is an argument, not a flag", () => {
    const install = remediated(["omaddons", "install", "--", "--force"])
    expect([install.runnable, install.argv]).toEqual([true, ["omaddons", "install", "--", "--force"]])
  })

  test("a remediation the classifier refuses is text", () => {
    const status = M.reduceStatus(fixture("status-attention"), NOW)
    const t = JSON.parse(JSON.stringify(status.targets))
    t[0].issues[0].remediation = { command: "rm -rf ~", argv: null, runnable: false, destructive: true }
    const rows = M.issueRows(M.issueSections(t, null, [], ""))
    expect([rows[0].action, rows[0].text]).toEqual([null, "rm -rf ~"])
  })

  test("d dismisses what was shown, one client at a time", () => {
    expect(M.dismissPlan(ERRORS(), "").map((p) => p.target)).toEqual(["retail"])
    expect(M.dismissPlan(ERRORS(), "retail")[0].token).toMatch(/^pc1:/)
    expect(M.dismissPlan(ERRORS(), "era")).toEqual([])
    expect(M.dismissPlan(null, "")).toEqual([])
    expect(M.boundStaleText("dismiss_errors", fixture("dismiss-stale"))).toBe("New errors arrived; review them")
  })

  test("All searches every client at once; installs go only where the hint allows", () => {
    expect(M.searchRequest("dbm", "")).toEqual({ template: "search_all", slots: { query: "dbm" } })
    expect(M.searchRequest("dbm", "retail")).toEqual({ template: "search", slots: { target: "retail", query: "dbm" } })
    for (const r of [M.searchRequest("--dbm", ""), M.searchRequest("--dbm", "retail")]) {
      expect(M.buildArgv(r.template, r.slots, opts()).argv.slice(-2)).toEqual(["--", "--dbm"])
    }
    const result = M.reduceSearch(fixture("search")).rows[0]
    expect(result.hints).toEqual({ retail: "likely" })
    const targets = [{ id: "retail" }, { id: "era" }]
    expect(M.installTargets(result, targets)).toEqual([
      { id: "retail", hint: "likely", eligible: true }, { id: "era", hint: "unknown", eligible: false }])
    expect(M.installTargets(Object.assign({}, result, { hints: { retail: "no" } }), targets)[0].eligible).toBe(false)
  })

  test("raw lines only for the row that asked; dots and popularity", () => {
    const raw = { selection: { target: "retail", addon: "Foo" }, data: M.reduceErrors(fixture("errors-raw")) }
    const row = { target: "retail", addon: "Foo" }
    const lines = M.rawLines(raw, row)
    expect(lines[0]).toContain("attempt to index a nil value")
    expect(lines.join("\n")).toContain("FakeChar")
    expect(M.rawLines(raw, { target: "retail", addon: "Bar" })).toBeNull()
    expect(M.rawLines(null, row)).toBeNull()
    expect([M.confidenceDots("high"), M.confidenceDots("low"), M.confidenceDots("")])
      .toEqual(["●●●", "●○○", "○○○"])
    expect([M.popularityText({ downloads: 2100000 }), M.popularityText({ downloads: null, stars: 412 }), M.popularityText({})])
      .toEqual(["2.1M downloads", "412 stars", ""])
  })

  test("the new templates build with hostile slots", () => {
    const raw = M.buildArgv("errors_raw", { target: "retail", addon: "-rf" }, opts())
    expect(raw.argv.slice(-1)).toEqual(["--addon=-rf"])
    const dismiss = M.buildArgv("dismiss_errors", { target: "retail" }, opts({ token: TOKEN }))
    expect(dismiss.argv).toContain("--if-match")
    expect(() => M.buildArgv("dismiss_errors", { target: "retail" }, opts())).toThrow()
  })
})

describe("the Backups view and the restore chooser", () => {
  const LISTED = () => M.reduceBackups(fixture("backups"))
  const DETAIL = () => M.reduceBackups(fixture("backups-detail"))

  test("times are local, as the bar's clock shows them", () => {
    const zone = process.env.TZ
    process.env.TZ = "America/Chicago"
    try {
      expect(M.dateText("2026-10-23T05:31:00Z")).toBe("2026-10-23 00:31")
      expect(M.dateText("not a time")).toBe("not a time")
      expect(M.dateText("")).toBe("")
    } finally {
      if (zone === undefined) delete process.env.TZ
      else process.env.TZ = zone
    }
  })

  test("rows say when, what, how big and what they prove; never verified for metadata only", () => {
    const listed = LISTED()
    const retail = listed.targets[0]
    expect([retail.id, retail.auto.mode, retail.freeBytes > 0, retail.backups[0].integrity]).toEqual(["retail", "daily", true, "metadata_only"])
    const at = Date.parse(retail.backups[0].capturedAt) + 3 * 3600000
    const sections = M.backupSections(listed, "", at)
    expect(sections[0].title).toBe("retail")
    expect(sections[0].lines[0]).toBe("Automatic backups: daily · last: not run yet")
    const line = sections[0].rows[0].line
    expect(line).toMatch(/^3 hours ago · manual · 5 files · 1\.2 KiB · stable$/)
    expect(line).not.toContain("verified")
    const verified = JSON.parse(JSON.stringify(fixture("backups")))
    Object.assign(verified.data.targets[0].backups[0], { integrity: "verified", baseline: "capture", busy: true, consistency: "best_effort" })
    const v = M.backupSections(M.reduceBackups(verified), "retail", at)[0].rows[0]
    expect(v.line).toContain("best effort · verified (capture) · in use")
    expect(v.busy).toBe(true)
    expect(M.backupSections(listed, "era", at)).toEqual([])
    // A client with no backups (which `backup list` omits) still has its header, from
    // status: where automatic backups cannot work, it says so.
    const era = { id: "era", backups: { count: 0, auto: { mode: "daily", capability: "unsupported", last: null }, freeBytes: 2 ** 30 } }
    const withEra = M.backupSections(listed, "", at, [era])
    expect(withEra.map((s) => s.title)).toEqual(["era", "retail"])
    expect(withEra[0].lines[0]).toBe("Automatic backups: daily, but this filesystem cannot clone")
    expect(withEra[0].lines[withEra[0].lines.length - 1]).toBe("No backups yet")
    expect(withEra[0].rows).toEqual([])
    // A header alone takes no cursor row: the first backup row is still row 0.
    expect(withEra[1].rows[0].index).toBe(0)
  })

  test("the automatic backups' four labels", () => {
    const now = Date.parse("2026-10-22T12:00:00Z")
    expect(M.autoLabel({ mode: "off", capability: "supported", last: null }, now)).toBe("Automatic backups: off")
    expect(M.autoLabel({ mode: "daily", capability: "supported", last: { at: "2026-10-22T09:00:00Z", result: "created" } }, now))
      .toBe("Automatic backups: daily, working · last: made one 3 hours ago")
    expect(M.autoLabel({ mode: "daily", capability: "unsupported", last: { at: "", result: "no_reflink" } }, now))
      .toBe("Automatic backups: daily, but this filesystem cannot clone")
    expect(M.autoLabel({ mode: "daily", capability: "supported", last: { at: "", result: "space_low" } }, now))
      .toBe("Automatic backups: skipped: low disk space")
    // daily-copy copies where clones are not possible: it works there.
    expect(M.autoLabel({ mode: "daily-copy", capability: "unsupported", last: { at: "2026-10-22T09:00:00Z", result: "created" } }, now))
      .toBe("Automatic backups: daily (copies), working · last: made one 3 hours ago")
  })

  test("b backs up the filtered client or all; restores wait for the game to close", () => {
    expect(M.backupRequest("")).toEqual({ template: "backup_all", slots: {}, label: "Back up all" })
    expect(M.backupRequest("retail").template).toBe("backup_target")
    const running = M.reduceStatus(fixture("status-running"), NOW).targets
    expect(M.restoreBlocked(running, "retail")).toBe("quit the game first")
    expect(M.restoreBlocked([{ id: "era", running: false, runningUnknown: true }], "era")).toBe("quit the game first (running: unknown)")
    expect(M.restoreBlocked([{ id: "era", running: false, runningUnknown: false }], "era")).toBe("")
  })

  test("the chooser: backups with the addon's captured version, addon scope first", () => {
    const id = LISTED().targets[0].backups[0].id
    const rows = M.chooserBackups(LISTED(), DETAIL(), "retail", "WeakAuras", Date.parse(LISTED().targets[0].backups[0].capturedAt) + 60000)
    expect(rows).toEqual([{ id: id, label: "1 min ago · manual · WeakAuras 5.19.2" }])
    expect(M.chooserBackups(LISTED(), null, "retail", "WeakAuras", NOW)[0].label).not.toContain("5.19.2")
    expect(M.restoreScopes("WeakAuras").map((s) => [s.template, s.label])).toEqual([
      ["restore_addon", "WeakAuras's settings"], ["restore", "Whole WTF/"]])
    expect(M.restoreScopes("")[0]).toMatchObject({ label: "One addon's settings…", needsAddon: true })
    expect(M.restoreScopes("")[1].note).toContain("keybindings, macros, chat settings")
  })

  test("the dry runs read as what would change, the version note included", () => {
    const addon = M.confirmText("restore_addon", fixture("restore-addon-preview")).split("\n")
    expect(addon[0]).toMatch(/^Restore WeakAuras's settings from the backup of \d{4}-\d\d-\d\d \d\d:\d\d: 2 replaced, 0 restored \(absent now\), 0 created since \(moved aside\)$/)
    expect(addon[1]).toBe("Settings written by WeakAuras 5.19.2; installed now: 5.20.0")
    const notIn = JSON.parse(JSON.stringify(fixture("restore-addon-preview")))
    notIn.data.addons[0].not_in_backup = ["WeakAurasOptions"]
    expect(M.confirmText("restore_addon", notIn)).toContain("WeakAurasOptions: not in this backup; left as it is")
    const whole = M.confirmText("restore", fixture("restore-preview")).split("\n")
    expect(whole[0]).toContain("Restore the whole WTF/ from the backup of")
    expect(whole[0]).toContain("5 files restored, 2 differ now, 0 created since (moved aside)")
    expect(whole).toContain("Also restores keybindings, macros, chat settings and every other addon's settings")
    expect(whole).toContain("The current WTF/ is kept as a backup (before restore)")
    expect(whole).toContain("Then restart the game")
    const skipped = JSON.parse(JSON.stringify(fixture("restore-preview")))
    skipped.warnings = [{ code: "before_restore_skipped", message: "this filesystem cannot clone, so the current WTF/ is kept only in the undo snapshot", fix: null }]
    expect(M.confirmText("restore", skipped)).toContain("kept only in the undo snapshot")
    expect(M.confirmText("prune_backup", fixture("prune-preview"))).toMatch(/^Delete backup 01M\w+ \(manual, 1\.2 KiB\) from retail$/)
    expect(M.resultLine(Object.assign({}, fixture("restore-addon-preview")))).toBe("Restored WeakAuras's settings on retail")
  })

  test("an ambiguous addon offers its folders; refusals keep their prose fix and folder", () => {
    const choices = M.ambiguityChoices(fixture("restore-ambiguous"))
    expect(choices).toEqual([
      { addon: "folder:WeakAuras", label: "the WeakAuras folder" }, { addon: "folder:WeakAurasOptions", label: "the WeakAurasOptions folder" }])
    // Its argv repeat the widget's own flags: never buttons as they are.
    expect(M.fixButtons(fixture("restore-ambiguous")).some((f) => f.argv && f.argv.indexOf("--addon") >= 0)).toBe(false)
    const slots = { target: "retail", addon: choices[0].addon, backup: "01M5JWP7MW633DZJ0TYDMBV8JN" }
    expect(M.buildArgv("restore_addon", slots, opts({ preview: true })).argv.slice(-3)).toEqual(["--addon=folder:WeakAuras", "--", "01M5JWP7MW633DZJ0TYDMBV8JN"])
    const running = fixture("restore-running")
    expect([running.error.code, M.fixText(running)]).toEqual(["game_running", "quit the game client, then rerun"])
    const unsupported = { ok: false, error: { code: "filesystem_unsupported", message: "no exchange", fix: "copy by hand", fix_argv: null, details: { backup_dir: "/c/Interface/.omaddons/backups/01M/WTF" } } }
    expect(M.backupFolderOf(unsupported)).toBe("/c/Interface/.omaddons/backups/01M/WTF")
    expect(M.backupFolderOf(running)).toBe("")
  })
})

describe("the service's desktop triggers", () => {
  test("a client that stopped, one known stopped, and errors that need reading again", () => {
    const running = M.reduceStatus(fixture("status-running"), NOW).targets
    const stopped = M.reduceStatus(fixture("status-updates"), NOW).targets
    expect(M.stoppedTargets(running, stopped)).toEqual(["retail"])
    expect(M.stoppedTargets(stopped, running)).toEqual([])
    // An unknown state is no transition.
    const unknown = stopped.map((t) => Object.assign({}, t, { running: false, runningUnknown: true }))
    expect(M.stoppedTargets(running, unknown)).toEqual([])
    expect(M.stoppedNow(stopped)).toEqual(["era", "retail"])
    expect(M.stoppedNow(running)).toEqual(["era"])
    expect(M.stoppedNow(unknown)).toEqual([])
  })

  test("errors are read again when stale or unchecked, never when the last read was partial", () => {
    const refresh = (name) => M.errorsToRefresh(M.reduceStatus(fixture(name), NOW).targets)
    expect(refresh("status-errors-unchecked")).toEqual(["retail"])
    expect(refresh("status-errors-stale")).toEqual(["retail"])
    expect(refresh("status-errors-partial")).toEqual([])
    expect(refresh("status-errors")).toEqual([])
  })

  test("an automatic check only when due, at most every 15 minutes", () => {
    const due = M.reduceStatus(fixture("status-check-due"), NOW).summary
    expect(due.checkDue).toBe(true)
    expect(M.autoCheckDue(true, due, 0, M.AUTO_CHECK_INTERVAL_MS)).toBe(true)
    expect(M.autoCheckDue(true, due, 1, M.AUTO_CHECK_INTERVAL_MS)).toBe(false)
    expect(M.autoCheckDue(true, due, 100, 300, 200)).toBe(true)
    expect(M.autoCheckDue(false, due, 0, NOW)).toBe(false)
    expect(M.autoCheckDue(true, M.reduceStatus(fixture("status-updates"), NOW).summary, 0, NOW)).toBe(false)
  })

  test("a rise counts from the second status on", () => {
    const before = M.reduceStatus(fixture("status-errors-unchecked"), NOW).summary.errors["new"]
    const after = M.reduceStatus(fixture("status-errors"), NOW).summary.errors["new"]
    expect([before, after]).toEqual([0, 2])
    expect([M.rose(before, after, false), M.rose(after, after, false), M.rose(before, after, true)]).toEqual([true, false, false])
  })

  test("the error toast names addons, counts and clients, nothing an error says", () => {
    const status = M.reduceStatus(fixture("status-errors"), NOW)
    const facts = M.errorFacts(status.targets)
    expect(facts).toEqual({ names: ["Foo"], counts: [2], targets: ["retail"] })
    const argv = M.errorToastArgv(facts, "a44.omaddons-fixture")
    expect(argv.slice(0, 7)).toEqual(["omarchy-notification-send", "--app-name", "omaddons", "-g", "\u{F0297}", "-u", "normal"])
    expect(argv.slice(7, 9)).toEqual(["Lua errors in your last session on retail", "Foo 2"])
    expect(argv.slice(-6)).toEqual(["--exec", "omarchy-shell", "shell", "summon", "a44.omaddons-fixture", "{}"])
    // Whatever the errors held, none of it reaches the toast.
    const texts = []
    for (const t of fixture("errors-raw").data.targets) for (const a of t.addons) for (const e of a.errors) {
      texts.push(e.message, e.location, e.raw && e.raw.message, ...(e.stack || []))
    }
    for (const text of texts.filter((x) => typeof x === "string" && x.length > 3)) {
      for (const part of argv) expect(part.includes(text)).toBe(false)
    }
    expect(argv.join(" ")).not.toContain("FakeChar")
    // The builder reads names, counts and clients only.
    const hostile = M.errorToastArgv({ names: ["X"], counts: [1], targets: ["r"], message: "secret", stack: ["s"] }, "id")
    expect(hostile.join(" ")).not.toContain("secret")
  })

  test("update and result toasts", () => {
    expect(M.updateToastArgv(3, ["era", "retail"], "id").slice(7, 9)).toEqual(["Addon updates", "3 updates on era and retail"])
    expect(M.resultToastArgv("Updated 3 addons", "id").slice(5, 9)).toEqual(["-u", "low", "omaddons", "Updated 3 addons"])
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
    // A read run from a fix reports no items: done, never "0 addons".
    const read = { schema_version: "1.0", tool_version: "0.1.0", ok: true, command: "list", data: { targets: [{ target: { id: "retail" }, addons: [] }] } }
    expect(M.resultLine(read)).toBe("Done")
  })

  test("an undo and a dismissal say what they did, never \"0 addons\"", () => {
    const undo = fixture("undo")
    const first = undo.data.transactions[0].target.id
    expect(M.resultLine(undo).startsWith(`Undid ${undo.data.reverted.kind} on ${first}`)).toBe(true)
    // A partial undo says where it failed, never that it undid there.
    const shape = { schema_version: "1.0", tool_version: "0.1.0" }
    const partialUndo = { ...shape, ok: true, partial: true, command: "undo", data: { reverted: { kind: "update" }, transactions: [
      { target: { id: "era" }, status: "undone", error: null },
      { target: { id: "retail" }, status: "failed", error: { message: "a later change conflicts" } },
    ] } }
    expect(M.resultLine(partialUndo)).toBe("Undid update on era; not on retail: a later change conflicts")
    // So does a partial backup.
    const partialBackup = { ...shape, ok: true, partial: true, command: "backup create", data: { targets: [
      { target: { id: "classic" }, backup: { id: "01M" }, error: null },
      { target: { id: "retail" }, backup: null, error: { message: "an I/O error" } },
    ] } }
    expect(M.resultLine(partialBackup)).toBe("Backed up classic; retail failed: an I/O error")
    const dismissed = fixture("dismiss")
    const n = dismissed.data.targets[0].errors
    expect(M.resultLine(dismissed)).toBe(`Dismissed ${n} new error${n === 1 ? "" : "s"} on retail`)
    const nothing = JSON.parse(JSON.stringify(dismissed))
    nothing.data.targets[0].errors = 0
    expect(M.resultLine(nothing)).toBe("No new errors to dismiss")
  })

  test("a partial result names what failed, with its fix as a button", () => {
    const partial = JSON.parse(JSON.stringify(fixture("update")))
    partial.partial = true
    const items = partial.data.targets[0].items
    items[0].status = "failed"
    items[0].name = "Plater"
    items[0].error = { code: "download_failed", message: "the download stopped", fix: "omaddons update -t era -- Plater",
      fix_argv: ["omaddons", "update", "-t", "era", "--", "Plater"], alternatives: [], retryable: true, details: {} }
    expect(M.resultLine(partial)).toBe("Updated 3 of 4; Plater failed: the download stopped")
    expect(M.itemFixes(partial).map((f) => f.argv)).toEqual([["omaddons", "update", "-t", "era", "--", "Plater"]])
    items[1].status = "failed"
    items[1].error = { code: "x", message: "y", fix: "rm -rf /", fix_argv: ["rm", "-rf", "/"], alternatives: [], retryable: false, details: {} }
    expect(M.resultLine(partial)).toBe("Updated 2 of 4; Plater failed: the download stopped (and 1 more)")
    expect(M.itemFixes(partial).length).toBe(1)
    for (const item of partial.data.targets.flatMap((t) => t.items)) item.status = "failed"
    expect(M.resultLine(partial).startsWith("Plater failed: the download stopped")).toBe(true)
    expect(M.itemFixes(fixture("update"))).toEqual([])
  })

  test("confirmation and stale texts", () => {
    expect(M.confirmText("remove", fixture("remove-preview"))).toBe("Remove Baz v1 from retail (1 folder)")
    expect(M.confirmText("undo", fixture("undo-preview"))).toBe("Undo update on era and retail: 2 folders restored, 0 removed")
    expect(M.staleText(fixture("remove-stale"))).toBe("Changed since you looked: command, targets, selection and observed. Nothing ran; check the new preview.")
    // The undo of a whole restore says it reverts the play since.
    const shape = { schema_version: "1.0", tool_version: "0.1.0", ok: true }
    const undoRestore = { ...shape, command: "undo", data: { reverted: { kind: "restore" }, transactions: [{ target: { id: "retail" } }],
      folders: { restored: [], removed: [] }, changed_since: 3 } }
    expect(M.confirmText("undo", undoRestore).split("\n")[1])
      .toBe("Also reverts every play since the restore: 3 files the game changed (kept in the undo's snapshot)")
    const uncounted = { ...undoRestore, data: { ...undoRestore.data, changed_since: null } }
    expect(M.confirmText("undo", uncounted).split("\n")[1])
      .toBe("Also reverts every play since the restore (the files it changed cannot be counted)")
    // Forgetting a client says what it does, never "0 addons".
    const rm = { ...shape, command: "targets rm", data: { id: "wotlk", managed_addons: 2, was_default: false, dry_run: true } }
    expect(M.confirmText("targets_rm", rm))
      .toBe("Forget client wotlk; nothing in its directory is touched (its record of 2 managed addons is kept)")
  })

  test("only https links to known hosts open", () => {
    expect(M.isSafeUrl("https://github.com/o/r")).toBe(true)
    expect(M.isSafeUrl("https://www.wowinterface.com/downloads/info1.html")).toBe(true)
    expect(M.isSafeUrl("https://tukui.org/elvui")).toBe(true)
    for (const url of ["http://github.com/o/r", "https://github.com.evil.example/x", "https://user@github.com/x",
      "https://github.com:8443/x", "javascript:alert(1)", "https://evil.example/https://github.com", "", null]) {
      expect([url, M.isSafeUrl(url)]).toEqual([url, false])
    }
  })
})

// ------------------------------------------------- the plan runner (G1, M4-D28)

const KEY_RETAIL = "01M8Q2RRRRRRRRRRRRRRRRRRRR"
const KEY_ERA = "01M8Q2EEEEEEEEEEEEEEEEEEEE"

// A synthetic envelope of `command` (update unless `extra` says otherwise).
function envOf(data, commands, extra) {
  return Object.assign({ ok: true, schema_version: "1.0", tool_version: "0.1.0", command: "update", exit_code: 0, partial: false,
    data: data, warnings: [], commands: commands || [] }, extra || {})
}

// The `commands[]` entry a dry run offers to run its plan.
function offer(id, extra) {
  return Object.assign({ action: "apply", command: "omaddons apply " + id, argv: ["omaddons", "apply", id], destructive: false }, extra || {})
}

// A selection item as the CLI writes it (Foo v1 → v2 on retail).
function item(fields) {
  return Object.assign({ target_key: KEY_RETAIL, target_id: "retail", addon: "Foo", source: "github", channel: "stable",
    release_id: "gh:o/foo@v2", asset: "Foo-v2.zip", from_version: "v1", to_version: "v2", op: "update",
    fields: [], tx: null, backup: null }, fields || {})
}

// The same item as the view showed it (shownOf's fields).
function shownItem(fields) {
  const out = Object.assign({}, item(fields))
  for (const key of ["op", "fields", "tx", "backup"]) delete out[key]
  return out
}

// The item approvalOf keeps.
function kept(fields) {
  const out = Object.assign({}, item(fields))
  for (const key of ["fields", "tx", "backup"]) delete out[key]
  return out
}

function scopeOf(fields) {
  return Object.assign({ dependencies_added: [], folders_removed: [], local_changes_replaced: { count: 0, folders: [] },
    source_changed: [], while_running: false }, fields || {})
}

function approvalWith(selection, extra) {
  return Object.assign({ routine: true, reasons: [], selection: selection, scope: scopeOf() }, extra || {})
}

// A storable dry run that stored plan PLAN_ID.
function previewOf(selection, extra, commands, data) {
  return envOf(Object.assign({ dry_run: true, plan_id: PLAN_ID, plan_expires_at: "2100-01-02T00:00:00Z", approval: approvalWith(selection, extra) }, data || {}),
    commands || [offer(PLAN_ID)])
}

// Each case as `[case, value]`, so a failure names it and shows both values.
function table(cases, run) {
  for (const [name, input, expected] of cases) expect([name, run(input)]).toEqual([name, expected])
}

describe("approvalOf", () => {
  test("the table", () => {
    table([
      ["well_formed", approvalWith([item({ asset: "" })]),
        { routine: true, reasons: [], selection: [kept({ asset: null })], scope: scopeOf() }],
      ["routine_with_a_reason", { routine: true, reasons: ["force"], selection: [], scope: {} },
        { routine: false, reasons: ["force"], selection: [], scope: {} }],
      ["routine_not_boolean", approvalWith([], { routine: "true" }), null],
      ["reasons_not_array", approvalWith([], { reasons: "force" }), null],
      ["reason_not_string", approvalWith([], { reasons: [3] }), null],
      ["selection_item_null", approvalWith([null]), null],
      ["selection_item_array", approvalWith([[]]), null],
      ["scope_missing", { routine: true, reasons: [], selection: [] }, { routine: true, reasons: [], selection: [], scope: {} }],
      ["not_an_object", "routine", null],
    ], M.approvalOf)
  })
})

describe("planFrom", () => {
  const withId = (id) => previewOf([item()], {}, [offer(id)], { plan_id: id })

  test("the table", () => {
    const plan = (extra) => Object.assign({ planId: PLAN_ID, argv: ["omaddons", "apply", PLAN_ID], destructive: false,
      expiresAt: "2100-01-02T00:00:00Z", approval: M.approvalOf(approvalWith([item()])) }, extra || {})
    const failed = Object.assign(previewOf([item()]), { ok: false, error: { code: "network" } })
    const noApproval = previewOf([item()])
    delete noApproval.data.approval
    table([
      ["offer_matches", previewOf([item()]), plan()],
      ["offer_destructive", previewOf([item()], {}, [offer(PLAN_ID, { destructive: true })]), plan({ destructive: true })],
      ["plan_id_null", previewOf([item()], {}, [offer(PLAN_ID)], { plan_id: null }), null],
      ["plan_id_json", withId("--json"), null],
      ["plan_id_target", withId("-t"), null],
      ["plan_id_path", withId("../x"), null],
      ["plan_id_lower_case", withId(PLAN_ID.toLowerCase()), null],
      ["plan_id_25_characters", withId(PLAN_ID.slice(1)), null],
      ["offer_for_another_id", previewOf([item()], {}, [offer("01M8Q2YYYYYYYYYYYYYYYYYYYY")]), null],
      ["two_offers", previewOf([item()], {}, [offer(PLAN_ID), offer(PLAN_ID)]), null],
      ["offer_with_extra_argument", previewOf([item()], {}, [offer(PLAN_ID, { argv: ["omaddons", "apply", PLAN_ID, "--dry-run"] })]), null],
      ["offer_not_omaddons", previewOf([item()], {}, [offer(PLAN_ID, { argv: ["/bin/omaddons", "apply", PLAN_ID] })]), null],
      ["offer_action_not_apply", previewOf([item()], {}, [offer(PLAN_ID, { action: "undo" })]), null],
      ["envelope_failed", failed, null],
      ["approval_missing", noApproval, null],
      ["approval_malformed", previewOf([item()], { routine: null }), null],
    ], M.planFrom)
  })

  test("the plan's argv is a copy", () => {
    const env = previewOf([item()])
    const plan = M.planFrom(env)
    plan.argv.push("--force")
    expect(env.commands[0].argv).toEqual(["omaddons", "apply", PLAN_ID])
  })
})

describe("requestedItems", () => {
  test("the table", () => {
    const lib = item({ addon: "LibFoo", op: "install", from_version: null })
    const withDeps = (deps) => M.approvalOf(approvalWith([item(), lib], { routine: false, reasons: ["dependency_added"], scope: scopeOf({ dependencies_added: deps }) }))
    table([
      ["string_dependency_dropped", withDeps(["LibFoo"]), [kept()]],
      ["object_dependency_dropped", withDeps([{ addon: "LibFoo" }, { name: "Foo" }]), []],
      ["case_differs_still_dropped", withDeps(["libfoo"]), [kept()]],
      ["no_dependencies_unchanged", withDeps([]), [kept(), M.approvalOf(approvalWith([lib])).selection[0]]],
    ], M.requestedItems)
  })
})

describe("selectionMatches", () => {
  test("the table", () => {
    const bar = { addon: "Bar", release_id: "gh:o/bar@v2", asset: "Bar-v2.zip" }
    const cases = [
      ["equal_one_item", [shownItem()], [kept()], true],
      ["equal_two_items_other_order", [shownItem(), shownItem(bar)], [kept(bar), kept()], true],
      ["addon_case_differs", [shownItem({ addon: "foo" })], [kept()], true],
      ["to_version_moved", [shownItem()], [kept({ to_version: "v3" })], false],
      ["from_version_moved", [shownItem()], [kept({ from_version: "v0" })], false],
      ["item_added", [shownItem()], [kept(), kept(bar)], false],
      ["item_dropped", [shownItem(), shownItem(bar)], [kept()], false],
      ["another_client_key", [shownItem()], [kept({ target_key: KEY_ERA })], false],
      ["same_versions_other_source", [shownItem()], [kept({ source: "wowi" })], false],
      ["same_versions_other_asset", [shownItem()], [kept({ asset: "Foo-retail.zip" })], false],
      ["same_versions_other_release_id", [shownItem()], [kept({ release_id: "gh:o/foo@v2-hotfix" })], false],
      ["other_channel", [shownItem()], [kept({ channel: "beta" })], false],
      ["duplicate_shown_key", [shownItem(), shownItem()], [kept(), kept(bar)], false],
      ["duplicate_selection_key", [shownItem(), shownItem(bar)], [kept(), kept()], false],
      ["empty_shown", [], [kept()], false],
      ["empty_both", [], [], false],
      ["not_arrays", null, [kept()], false],
    ]
    // Missing evidence is never equality: each match field missing on either side.
    for (const field of M.MATCH_FIELDS) {
      cases.push(["missing_on_row_" + field, [shownItem({ [field]: null })], [kept()], false])
      cases.push(["missing_on_plan_" + field, [shownItem()], [kept({ [field]: null })], false])
    }
    cases.push(["missing_on_row_asset_empty", [shownItem({ asset: "" })], [kept()], false])
    expect(M.MATCH_FIELDS.length).toBe(8)
    for (const [name, shown, selection, expected] of cases) expect([name, M.selectionMatches(shown, selection)]).toEqual([name, expected])
  })
})

describe("changeLines", () => {
  test("the table", () => {
    const bar = { addon: "Bar", target_key: KEY_ERA, target_id: "era", release_id: "gh:o/bar@v2", asset: "Bar-v2.zip" }
    const cases = [
      ["to_version", [shownItem()], [kept({ to_version: "v3", release_id: "gh:o/foo@v3", asset: "Foo-v3.zip" })], ["Foo v3 is now the newest"]],
      ["release_id", [shownItem()], [kept({ release_id: "gh:o/foo@v2-hotfix" })], ["Foo v2 is another release now"]],
      ["asset", [shownItem()], [kept({ asset: "Foo-retail.zip" })], ["Foo v2 comes in another file now (Foo-retail.zip)"]],
      ["source", [shownItem()], [kept({ source: "wowi" })], ["Foo comes from wowi now"]],
      ["channel", [shownItem()], [kept({ channel: "beta" })], ["Foo follows beta now"]],
      ["from_version", [shownItem()], [kept({ from_version: "v0" })], ["Foo on retail is at v0 now"]],
      ["missing_on_row", [shownItem({ release_id: null, asset: null })], [kept()], ["the view did not show Foo's release id and asset"]],
      ["missing_on_plan", [shownItem()], [kept({ source: null })], ["the preview did not say Foo's source"]],
      ["added", [shownItem()], [kept(), kept(bar)], ["Also: Update Bar v1 → v2 on era (not in the view)"]],
      ["dropped", [shownItem(), shownItem(bar)], [kept()], ["Bar on era is no longer part of it"]],
      ["equal", [shownItem()], [kept()], []],
    ]
    for (const [name, shown, selection, expected] of cases) expect([name, M.changeLines(shown, selection)]).toEqual([name, expected])
  })

  test("the heading", () => {
    table([
      ["heading_caps_at_three", ["a", "b", "c", "d", "e"], "Changed since you looked: a; b; c; and 2 more"],
      ["heading_three", ["a", "b", "c"], "Changed since you looked: a; b; c"],
      ["heading_one", ["Foo v3 is now the newest"], "Changed since you looked: Foo v3 is now the newest"],
      ["heading_none", [], "Changed since you looked"],
    ], M.changeHeading)
  })
})

describe("reasonText", () => {
  test("the table", () => {
    const cases = [
      ["dependency_added", "dependency_added", scopeOf({ dependencies_added: ["LibFoo"] }), ["Also installs LibFoo (a new dependency)"]],
      ["dependency_added_with_version", "dependency_added", scopeOf({ dependencies_added: [{ addon: "LibFoo", version: "1.2" }] }),
        ["Also installs LibFoo 1.2 (a new dependency)"]],
      ["folders_removed", "folders_removed", scopeOf({ folders_removed: ["Foo_Options", { folder: "Foo_Extra" }] }),
        ["Removes the folder Foo_Options (kept for undo)", "Removes the folder Foo_Extra (kept for undo)"]],
      ["local_changes_replaced_two_folders", "local_changes_replaced", scopeOf({ local_changes_replaced: { count: 3, folders: ["DBM-Core", "DBM-GUI"] } }),
        ["Replaces 3 files you changed in DBM-Core and DBM-GUI (kept for undo)"]],
      ["local_changes_replaced_one", "local_changes_replaced", scopeOf({ local_changes_replaced: { count: 1, folders: ["Foo"] } }),
        ["Replaces 1 file you changed in Foo (kept for undo)"]],
      ["source_changed", "source_changed", scopeOf({ source_changed: ["Foo", { addon: "Bar", to: "wowi" }] }),
        ["Foo comes from another source now", "Bar comes from another source now (wowi)"]],
      ["channel_changed", "channel_changed", scopeOf(), ["Follows another release channel than the one installed"]],
      ["while_running", "while_running", scopeOf({ while_running: true }), ["Changes files while the game is running (/reload in game afterwards)"]],
      ["reinstall", "reinstall", scopeOf(), ["Reinstalls over the installed files"]],
      ["force", "force", scopeOf(), ["Takes over folders another addon owns (--force)"]],
      ["downgrade", "downgrade", scopeOf(), ["Installs an older version than the one installed"]],
      ["destructive", "destructive", scopeOf(), ["Removes, reverts or replaces something you have now"]],
      ["dependency_added_empty", "dependency_added", scopeOf(), ["Also installs a new dependency"]],
      ["folders_removed_empty", "folders_removed", scopeOf(), ["Removes folders (kept for undo)"]],
      ["local_changes_replaced_empty", "local_changes_replaced", scopeOf(), ["Replaces files you changed (kept for undo)"]],
      ["source_changed_empty", "source_changed", scopeOf(), ["An addon comes from another source now"]],
      ["no_scope", "dependency_added", undefined, ["Also installs a new dependency"]],
      ["unknown_reason", "some_new_thing", scopeOf(), ["Also: some new thing"]],
    ]
    for (const [name, reason, scope, expected] of cases) expect([name, M.reasonText(reason, scope)]).toEqual([name, expected])
  })
})

describe("planItemLines", () => {
  test("the table", () => {
    const many = Array.from({ length: 14 }, (_, i) => kept({ addon: "Addon" + i }))
    const lines = Array.from({ length: 12 }, (_, i) => "Update Addon" + i + " v1 → v2 on retail").concat(["(and 2 more)"])
    table([
      ["update", [kept()], ["Update Foo v1 → v2 on retail"]],
      ["install", [kept({ op: "install", addon: "WeakAuras", target_id: "era", from_version: null, to_version: "5.22.1" })], ["Install WeakAuras 5.22.1 on era"]],
      ["remove", [kept({ op: "remove", addon: "Baz", to_version: null })], ["Remove Baz v1 from retail"]],
      ["reinstall", [kept({ op: "reinstall", to_version: "v1" })], ["Reinstall Foo v1 on retail"]],
      ["unknown_op", [kept({ op: "adopt" })], ["Adopt Foo on retail"]],
      ["a_missing_version_is_left_out", [kept({ from_version: null })], ["Update Foo → v2 on retail"]],
      ["caps_at_twelve", many, lines],
      ["not_an_array", null, []],
    ], M.planItemLines)
    expect(M.WIDGET_DIALOG_ITEMS_MAX).toBe(12)
  })
})

describe("previewDecision", () => {
  const pick = (d) => ({ kind: d.kind, heading: d.heading, lines: d.lines, line: d.line, fixText: d.fixText, planId: d.plan ? d.plan.planId : null })
  const decision = (fields) => Object.assign({ kind: "dialog", heading: "", lines: [], line: "", fixText: "", planId: PLAN_ID }, fields)
  const refused = (line, fixText) => decision({ kind: "refuse", line: line, fixText: fixText || "", planId: null })
  const match = (shown, extra) => Object.assign({ mode: "match", shown: shown, again: false, path: "update" }, extra || {})
  const nothing = previewOf([], {}, [], { plan_id: null, plan_expires_at: null })
  const notStored = previewOf([item()], {}, [], { plan_id: null, plan_expires_at: null })
  notStored.warnings = [{ code: "plan_not_stored", message: "the plan store is full", fix: "previews are kept again as plans expire", fix_argv: null }]
  const noApproval = previewOf([item()])
  delete noApproval.data.approval
  const lib = item({ addon: "LibFoo", op: "install", from_version: null, to_version: "1.0", release_id: "gh:o/libfoo@1.0", asset: "LibFoo.zip" })
  const withDependency = previewOf([item(), lib], { routine: false, reasons: ["dependency_added"], scope: scopeOf({ dependencies_added: ["LibFoo"] }) })
  const removal = previewOf([item({ op: "remove", addon: "Baz", to_version: null, release_id: null, asset: null })],
    { routine: false, reasons: ["folders_removed", "destructive"], scope: scopeOf({ folders_removed: ["Baz"] }) },
    [offer(PLAN_ID, { destructive: true })], { target: { id: "retail" }, items: [{ name: "Baz", version: "v1", folders: ["Baz"] }] })
  removal.command = "remove"

  test("the table", () => {
    const cases = [
      ["failed_envelope", match([shownItem()]), Object.assign(previewOf([item()]), { ok: false }), decision({ kind: "failed", planId: null })],
      ["no_approval", match([shownItem()]), noApproval, refused("The preview did not say what it would change; nothing ran")],
      ["nothing_to_do_match", match([shownItem()]), nothing, decision({ kind: "refresh", line: "Already up to date; the view is refreshed", planId: null })],
      ["nothing_to_do_confirm", { mode: "confirm", path: "remove" }, nothing, decision({ kind: "refresh", line: "Nothing to do; the view is refreshed", planId: null })],
      ["not_stored", match([shownItem()]), notStored, refused("The preview could not be kept as a plan; nothing ran", "previews are kept again as plans expire")],
      ["offer_mismatch", match([shownItem()]), previewOf([item()], {}, [offer("01M8Q2YYYYYYYYYYYYYYYYYYYY")]), refused("The preview offered no plan to apply; nothing ran")],
      ["confirm", { mode: "confirm", path: "remove" }, removal, decision({
        lines: ["Remove Baz v1 from retail (1 folder)", "Removes the folder Baz (kept for undo)", "Removes, reverts or replaces something you have now"] })],
      ["match_equal_routine", match([shownItem()]), previewOf([item()]), decision({ kind: "direct" })],
      ["match_equal_not_routine", match([shownItem()]), withDependency, decision({
        lines: ["Update Foo v1 → v2 on retail", "Install LibFoo 1.0 on retail", "Also installs LibFoo (a new dependency)"] })],
      ["match_different", match([shownItem()]), previewOf([item({ to_version: "v3", release_id: "gh:o/foo@v3", asset: "Foo-v3.zip" })]), decision({
        heading: "Changed since you looked: Foo v3 is now the newest",
        lines: ["Changed since you looked: Foo v3 is now the newest", "Update Foo v1 → v3 on retail"] })],
      ["match_empty_shown", match([]), previewOf([item()]), decision({ lines: ["Update Foo v1 → v2 on retail"] })],
      ["match_no_shown", { mode: "match", path: "update" }, previewOf([item()]), decision({ lines: ["Update Foo v1 → v2 on retail"] })],
      ["match_equal_routine_again", match([shownItem()], { again: true }), previewOf([item()]), decision({ lines: ["Update Foo v1 → v2 on retail"] })],
      ["match_equal_routine_destructive_offer", match([shownItem()]), previewOf([item()], {}, [offer(PLAN_ID, { destructive: true })]),
        decision({ lines: ["Update Foo v1 → v2 on retail"] })],
      ["unknown_mode_is_never_direct", { mode: "auto", shown: [shownItem()], path: "update" }, previewOf([item()]),
        decision({ lines: [M.confirmText("update", previewOf([item()]))] })],
    ]
    for (const [name, spec, env, expected] of cases) expect([name, pick(M.previewDecision(spec, env))]).toEqual([name, expected])
  })

  test("dialogText joins the lines", () => {
    expect(M.dialogText(M.previewDecision(match([shownItem()]), withDependency)))
      .toBe("Update Foo v1 → v2 on retail\nInstall LibFoo 1.0 on retail\nAlso installs LibFoo (a new dependency)")
    expect(M.dialogText(null)).toBe("")
  })
})

describe("applyResult and needsPreviewAgain", () => {
  const applied = (result, outcomes, extra) => envOf({ plan_id: PLAN_ID, applied: { command: result.command, display_argv: ["omaddons", result.command] },
    result: result.data, outcomes: outcomes }, result.commands, Object.assign({ command: "apply", exit_code: result.exit_code, partial: result.partial }, extra || {}))
  const refusal = (code, details) => ({ ok: false, schema_version: "1.0", tool_version: "0.1.0", command: "apply", exit_code: code === "locked" ? 5 : code === "plan_not_found" ? 3 : 1,
    error: { code: code, message: "x", fix: "omaddons update --dry-run", fix_argv: ["omaddons", "update", "--dry-run"], alternatives: [], retryable: false, details: details || {} },
    warnings: [], commands: [] })
  const committed = [{ target: "era", key: KEY_ERA, status: "committed", tx: "01M8Q2TTTTTTTTTTTTTTTTTTTT", error_code: null }]
  const someStale = committed.concat([{ target: "retail", key: KEY_RETAIL, status: "stale", tx: null, error_code: "plan_stale" }])

  test("an apply reads as the command it ran", () => {
    const update = fixture("update")
    const view = M.applyResult(applied(update, committed))
    expect(["wrapper_reads_as_its_command", M.resultLine(view), view.command, view.outcomes])
      .toEqual(["wrapper_reads_as_its_command", M.resultLine(update), "update", committed])
    expect(M.resultLine(update)).toBe("Updated 4 addons on 2 clients")
    expect(M.resultUndo({ commands: view.commands.map((c) => Object.assign({ runnable: true }, c)) }).action).toBe("undo_run")
    table([
      ["not_apply", update, null],
      ["failed_apply", refusal("plan_stale", { reason: "expired" }), null],
      ["no_result", envOf({ plan_id: PLAN_ID, applied: { command: "update" } }, [], { command: "apply" }), null],
    ], M.applyResult)
  })

  test("needsPreviewAgain", () => {
    const update = fixture("update")
    table([
      ["stale_expired", refusal("plan_stale", { reason: "expired" }), true],
      ["stale_binary_changed", refusal("plan_stale", { reason: "binary_changed" }), true],
      ["stale_input_missing", refusal("plan_stale", { reason: "input_missing", path: "/x" }), true],
      ["stale_already_applied", refusal("plan_stale", { reason: "already_applied", txs: [] }), true],
      ["stale_changed_selection", refusal("plan_stale", { changed: ["selection"] }), true],
      ["not_found", refusal("plan_not_found", { prefix: PLAN_ID }), true],
      ["outcome_stale", applied(update, someStale, { partial: true }), true],
      ["locked", refusal("locked", { lease: PLAN_ID }), false],
      ["ok", applied(update, committed), false],
      ["not_apply", Object.assign(refusal("plan_stale", { changed: ["selection"] }), { command: "remove" }), false],
    ], M.needsPreviewAgain)
  })

  test("staleText", () => {
    const after = " Nothing ran; check the new preview."
    table([
      ["reason_expired", refusal("plan_stale", { reason: "expired" }), "The plan expired." + after],
      ["reason_binary_changed", refusal("plan_stale", { reason: "binary_changed" }), "omaddons changed since the preview." + after],
      ["reason_input_missing", refusal("plan_stale", { reason: "input_missing", path: "/x" }), "A file the plan needs is gone." + after],
      ["reason_already_applied", refusal("plan_stale", { reason: "already_applied", txs: [] }), "The plan was applied already." + after],
      ["changed", refusal("plan_stale", { changed: ["selection", "observed"] }), "Changed since you looked: selection and observed." + after],
      ["plan_not_found", refusal("plan_not_found", { prefix: PLAN_ID }), "The plan is gone (expired or cleared)." + after],
      ["outcome_stale", applied(fixture("update"), someStale, { partial: true }), "Changed since you looked on retail. Nothing ran there; check the new preview."],
      ["bound_unchanged", fixture("remove-stale"), "Changed since you looked: command, targets, selection and observed." + after],
    ], M.staleText)
  })
})

describe("what the view showed", () => {
  const lists = () => M.reduceList(fixture("list"))
  const retailKey = () => M.targetById(fixture("list").data.targets, "retail").key

  test("the rows carry the release the list names", () => {
    const rows = M.updateRows(lists(), "retail", [])
    const bar = M.shownOf(rows[0])
    expect(["shownOf_from_an_update_row", bar]).toEqual(["shownOf_from_an_update_row", {
      target_key: retailKey(), target_id: "retail", addon: "Bar", source: "github", channel: "stable",
      release_id: "gh:o/bar@v2", asset: "Bar.zip", from_version: "v1", to_version: "v2" }])
    // An Installed row's `source` is its ref's kind, for display; shownOf takes the
    // release's.
    const first = M.installedRows(lists(), "retail")[0]
    expect(["shownOf_uses_latestSource_not_the_display_source", first.source, M.shownOf(first).source])
      .toEqual(["shownOf_uses_latestSource_not_the_display_source", "gh", "github"])
    const installed = M.installedRows(lists(), "retail")[1]
    expect([installed.name, installed.latest, installed.latestReleaseId, installed.targetKey]).toEqual(["Baz", "v1", "gh:o/baz@v1", retailKey()])
    expect(M.rowAction(rows[0], "update").shown).toEqual([bar])
  })

  test("shownForAddon", () => {
    const partial = (target, addon) => [{ target_key: null, target_id: target, addon: addon, source: null, channel: null,
      release_id: null, asset: null, from_version: null, to_version: null }]
    table([
      ["shownForAddon_found", [lists(), "retail", "foo"], [M.shownOf(M.installedRows(lists(), "retail").filter((r) => r.name === "Foo")[0])]],
      ["shownForAddon_unknown_addon", [lists(), "retail", "Nope"], partial("retail", "Nope")],
      ["shownForAddon_no_lists", [null, "era", "Foo"], partial("era", "Foo")],
    ], ([l, target, addon]) => M.shownForAddon(l, target, addon))
    expect(M.shownForAddon(lists(), "retail", "foo")[0].release_id).toBe("gh:o/foo@v2")
  })

  test("updateRequest_takes_only_update_rows_in_the_filter", () => {
    const env = JSON.parse(JSON.stringify(fixture("list")))
    const retail = M.targetById(env.data.targets, "retail")
    retail.addons.push(Object.assign({}, retail.addons[0], { name: "Pinned", pinned: true, latest: Object.assign({}, retail.addons[0].latest, { status: "pinned" }) }))
    retail.addons.push(Object.assign({}, retail.addons[0], { name: "Blocked", latest: Object.assign({}, retail.addons[0].latest, { status: "blocked" }) }))
    retail.addons.push(Object.assign({}, retail.addons[0], { name: "Failed", latest: { version: null, status: "failed", source: null, release_id: null, asset: null } }))
    const rows = M.updateRows(M.reduceList(env), "", [])
    expect(rows.map((r) => r.kind)).toContain("pinned")
    const names = (r) => r.shown.map((s) => s.target_id + "/" + s.addon)
    expect(names(M.updateRequest("retail", rows))).toEqual(["retail/Bar", "retail/Foo"])
    expect(names(M.updateRequest("", rows))).toEqual(["era/Bar", "era/Foo", "retail/Bar", "retail/Foo"])
    // Without the view's rows the request is as before.
    expect(M.updateRequest("retail")).toEqual({ template: "update_target", slots: { target: "retail" }, label: "Update retail" })
  })
})

describe("classifyCliArgv's apply rule", () => {
  const plan = M.planFrom(previewOf([item()]))
  const argv = ["omaddons", "apply", PLAN_ID]

  test("apply runs only with its plan", () => {
    const other = M.planFrom(previewOf([item()], {}, [offer("01M8Q2YYYYYYYYYYYYYYYYYYYY")], { plan_id: "01M8Q2YYYYYYYYYYYYYYYYYYYY" }))
    table([
      ["apply_without_plan", [argv, undefined], { ok: false, path: "", reason: "apply_outside_plan" }],
      ["apply_with_its_plan", [argv, plan], { ok: true, path: "apply", reason: "" }],
      ["apply_with_another_plans_argv", [argv, other], { ok: false, path: "", reason: "apply_outside_plan" }],
      ["apply_with_a_plan_like_object", [argv, { argv: ["omaddons", "apply"] }], { ok: false, path: "", reason: "apply_outside_plan" }],
    ], ([a, p]) => {
      const v = M.classifyCliArgv(a, p)
      return { ok: v.ok, path: v.path, reason: v.reason }
    })
  })

  test("cliArgv builds it only with its plan, never previewed or bound", () => {
    expect(["cliArgv_builds_with_its_plan", M.cliArgv(argv, opts({ plan: plan })).argv]).toEqual(["cliArgv_builds_with_its_plan",
      [BINARY, "--json", "--actor", "widget", "--reason", "Test (bar widget)", "--progress", "json", "apply", PLAN_ID]])
    expect(M.cliArgv(argv, opts({ plan: plan, confirmed: true })).env).toEqual(CONTEXT)
    table([
      ["cliArgv_without_plan", opts(), "apply_outside_plan"],
      ["cliArgv_refuses_preview", opts({ plan: plan, preview: true }), "apply_outside_plan"],
      ["cliArgv_refuses_token", opts({ plan: plan, token: TOKEN }), "apply_outside_plan"],
    ], (o) => reasonOf(() => M.cliArgv(argv, o)))
  })

  test("an apply from any other envelope is never a button", () => {
    const status = JSON.parse(JSON.stringify(fixture("status-attention")))
    status.data.targets[0].issues[0].remediation = { command: "omaddons apply " + PLAN_ID, argv: argv, destructive: false }
    status.commands = [offer(PLAN_ID)]
    status.warnings = [{ code: "w", message: "m", fix: "omaddons apply " + PLAN_ID, fix_argv: argv }]
    const reduced = M.reduceStatus(status)
    const failure = { ok: false, schema_version: "1.0", tool_version: "0.1.0", command: "update", exit_code: 1,
      error: { code: "x", message: "y", fix: "omaddons apply " + PLAN_ID, fix_argv: argv, alternatives: [{ display: "omaddons apply " + PLAN_ID, argv: argv }], retryable: false, details: {} },
      warnings: [], commands: [] }
    table([
      ["remediation", reduced.targets[0].issues[0].remediation.runnable, false],
      ["commands_entry", reduced.commands.filter((c) => c.action === "apply").map((c) => [c.runnable, c.argv]), [[false, null]]],
      ["fix_and_alternative", M.fixButtons(failure), []],
      ["fix_as_text", M.fixText(failure), "omaddons apply " + PLAN_ID],
      ["warning_fix", [reduced.warnings[0].fix, reduced.warnings[0].fixText], [null, "omaddons apply " + PLAN_ID]],
      ["result_undo", M.resultUndo({ commands: [{ action: "undo", command: "omaddons apply " + PLAN_ID, argv: argv, runnable: false }] }), null],
    ], (x) => x)
  })
})

// --------------------------------------- plans from the binary (M4-D28, step 1's scenes)

// The previews of the scenes that store a plan, each the CLI's own envelope.
const PLANNED_PREVIEWS = ["plan-update-routine", "plan-update-all", "plan-undo", "plan-remove", "plan-update-moved", "plan-update-v4",
  "plan-update-asset", "plan-update-dependency", "plan-update-local-edits", "plan-update-removal"]

describe("plans from the binary", () => {
  // What the Updates view shows for `list`'s envelope in `filter`, as Model.shownOf
  // gives it; one addon's row, as Enter on it passes.
  const shownFrom = (list, filter) => M.updateRows(M.reduceList(fixture(list)), filter, []).map(M.shownOf)
  const fooRow = (list) => shownFrom(list, "retail").filter((s) => Object.is(s.addon, "Foo"))
  // A preview by its fixture's name, or an envelope derived from one.
  const envOf = (preview) => (preview instanceof Object ? preview : fixture(preview))
  const decide = (spec, preview) => M.previewDecision(Object.assign({ again: false, path: "update" }, spec), envOf(preview))
  const match = (list, preview) => decide({ mode: "match", shown: fooRow(list) }, preview)
  const copy = (name) => JSON.parse(JSON.stringify(fixture(name)))
  const after = " Nothing ran; check the new preview."

  test("a routine matching update applies its plan directly", () => {
    const d = match("plan-list", "plan-update-routine")
    expect(["plan-list + plan-update-routine", d.kind]).toEqual(["plan-list + plan-update-routine", "direct"])
    const argv = M.cliArgv(d.plan.argv, opts({ plan: d.plan })).argv
    expect(["the apply argv", argv.slice(-4), argv.includes("--dry-run"), argv.includes("--if-match")])
      .toEqual(["the apply argv", ["--progress", "json", "apply", d.plan.planId], false, false])
  })

  test("the apply's result reads as the update's", () => {
    const view = M.applyResult(fixture("plan-update-routine-apply"))
    expect(["plan-update-routine-apply", M.resultLine(view)]).toEqual(["plan-update-routine-apply", "Updated 1 addon"])
    expect(["plan-update-routine-apply", view.commands.some((c) => c.action === "undo" || c.action === "undo_run")])
      .toEqual(["plan-update-routine-apply", true])
    expect(M.resultLine(M.applyResult(fixture("plan-update-all-apply")))).toBe("Updated 3 addons on 2 clients")
  })

  test("nothing to do refreshes", () => {
    const d = match("plan-list-after", "plan-update-noop")
    expect(["plan-update-noop", d.kind, d.line]).toEqual(["plan-update-noop", "refresh", "Already up to date; the view is refreshed"])
  })

  test("update all applies when every row matches", () => {
    const shown = shownFrom("plan-list-after", "")
    expect(["plan-list-after rows", shown.map((s) => s.target_id + "/" + s.addon)]).toEqual(["plan-list-after rows", ["era/Bar", "era/Foo", "retail/Bar"]])
    const d = decide({ mode: "match", shown: shown }, "plan-update-all")
    expect(["plan-update-all", d.kind]).toEqual(["plan-update-all", "direct"])
    const fewer = decide({ mode: "match", shown: shown.slice(1) }, "plan-update-all")
    expect(["one row fewer", fewer.kind, fewer.heading, fewer.lines[0]]).toEqual(["one row fewer", "dialog",
      "Changed since you looked: Also: Update Bar v1 → v2 on era (not in the view)",
      "Changed since you looked: Also: Update Bar v1 → v2 on era (not in the view)"])
  })

  test("a moved release opens the dialog as changed", () => {
    const d = match("plan-moved-list", "plan-update-moved")
    expect(["plan-update-moved", d.kind, d.lines[0]]).toEqual(["plan-update-moved", "dialog", "Changed since you looked: Foo v3 is now the newest"])
  })

  test("another asset opens the dialog as changed", () => {
    const d = match("plan-asset-list", "plan-update-asset")
    expect(["plan-update-asset", d.kind, d.heading]).toEqual(["plan-update-asset", "dialog",
      "Changed since you looked: Foo v2 comes in another file now (Foo-retail.zip)"])
  })

  test("another source opens the dialog as changed (derived)", () => {
    const preview = copy("plan-update-routine")
    preview.data.approval.selection[0].source = "wowi"
    const d = match("plan-list", preview)
    expect(["derived: plan-update-routine from wowi", d.kind, d.heading]).toEqual(["derived: plan-update-routine from wowi", "dialog",
      "Changed since you looked: Foo comes from wowi now"])
  })

  test("a dependency added opens the dialog naming it", () => {
    const d = match("plan-dependency-list", "plan-update-dependency")
    expect(["plan-update-dependency", d.kind, d.heading, d.lines.some((l) => l.startsWith("Also installs LibFoo") && l.endsWith("(a new dependency)"))])
      .toEqual(["plan-update-dependency", "dialog", "", true])
  })

  test("local edits replaced open the dialog", () => {
    const d = match("plan-local-edits-list", "plan-update-local-edits")
    expect(["plan-update-local-edits", d.kind, d.heading, d.lines.includes("Replaces 1 file you changed in Foo (kept for undo)")])
      .toEqual(["plan-update-local-edits", "dialog", "", true])
  })

  test("extra removals open the dialog", () => {
    const d = match("plan-removal-list", "plan-update-removal")
    expect(["plan-update-removal", d.kind, d.heading, d.lines.includes("Removes the folder Foo_Options (kept for undo)")])
      .toEqual(["plan-update-removal", "dialog", "", true])
  })

  test("a full store runs nothing (derived)", () => {
    const preview = copy("plan-update-routine")
    preview.data.plan_id = null
    preview.data.plan_expires_at = null
    preview.commands = preview.commands.filter((c) => !Object.is(c.action, "apply"))
    preview.warnings = [{ code: "plan_not_stored", message: "the plan store is full", fix: "previews are kept again as plans expire", fix_argv: null }]
    const d = match("plan-list", preview)
    expect(["derived: plan-update-routine, store full", d.kind, d.fixText]).toEqual(["derived: plan-update-routine, store full", "refuse",
      "previews are kept again as plans expire"])
  })

  test("a stale, an expired and a missing plan preview again", () => {
    table([
      ["plan-apply-stale", "plan-apply-stale", [true, "Changed since you looked: selection." + after]],
      ["plan-apply-expired", "plan-apply-expired", [true, "The plan expired." + after]],
      ["plan-apply-not-found", "plan-apply-not-found", [true, "The plan is gone (expired or cleared)." + after]],
    ], (name) => [M.needsPreviewAgain(fixture(name)), M.staleText(fixture(name))])
  })

  test("a removal and an undo confirm through apply", () => {
    const removal = decide({ mode: "confirm", shown: [], path: "remove" }, "plan-remove")
    expect(["plan-remove", removal.kind, removal.lines[0], removal.plan.destructive])
      .toEqual(["plan-remove", "dialog", "Remove Bar v1 from retail (1 folder)", true])
    const undo = decide({ mode: "confirm", shown: [], path: "undo" }, "plan-undo")
    expect(["plan-undo", undo.kind, undo.lines[0].indexOf("Undo update on") === 0, undo.plan.destructive])
      .toEqual(["plan-undo", "dialog", true, true])
    expect(["plan-remove-apply", M.resultLine(M.applyResult(fixture("plan-remove-apply")))]).toEqual(["plan-remove-apply", "Removed 1 addon"])
    expect(["plan-undo-apply", M.resultLine(M.applyResult(fixture("plan-undo-apply"))).indexOf("Undid update on") === 0])
      .toEqual(["plan-undo-apply", true])
  })

  test("every preview that stored a plan offers exactly its apply", () => {
    for (const name of PLANNED_PREVIEWS) {
      const plan = M.planFrom(fixture(name))
      expect([name, plan !== null && M.PLAN_ID_PATTERN.test(plan.planId)]).toEqual([name, true])
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
  // A previewed template builds only as its preview (its Yes is its plan's apply, below);
  // its real form is refused.
  const realForms = []
  for (const name of Object.keys(M.COMMANDS)) {
    const base = benignSlots(name)
    const extra = M.COMMANDS[name].preview ? { preview: true } : { token: runOpts(name).token }
    record(name, base, extra)
    if (M.COMMANDS[name].preview) realForms.push([name, reasonOf(() => M.buildArgv(name, base, opts()))])
    for (const slot of M.templateShape(name).slots) {
      if (slot === "target") continue
      for (const value of HOSTILE) record(name, Object.assign({}, base, { [slot]: value }), extra)
    }
  }
  expect(refused).toEqual([])
  expect(realForms.sort((a, b) => (a[0] < b[0] ? -1 : 1))).toEqual(PREVIEWED.map((name) => [name, "preview_required"]))

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
  // The apply a preview's plan runs, built only with that plan (M4-D28): a synthetic
  // preview's, and every real one step 1 wrote.
  const planned = []
  for (const extra of [{}, { confirmed: true }]) {
    const plan = M.planFrom(previewOf([item()]))
    planned.push({ plan_id: plan.planId, argv: M.cliArgv(plan.argv, opts(Object.assign({ plan: plan }, extra))).argv })
  }
  for (const name of PLANNED_PREVIEWS) {
    const plan = M.planFrom(fixture(name))
    planned.push({ plan_id: plan.planId, argv: M.cliArgv(plan.argv, opts({ plan: plan })).argv })
  }

  const stringArgv = (argv) => argv.every((a) => typeof a === "string")
  const doc = { binary: BINARY, built, classified: classified.filter((c) => stringArgv(c.argv)), planned }
  fs.writeFileSync(path.join(OUT, "built-argv.json"), JSON.stringify(doc, null, 1))
  expect(built.length).toBeGreaterThan(Object.keys(M.COMMANDS).length * 5)
  expect(classified.some((c) => c.refusal === "destructive_without_preview")).toBe(true)
  expect(classified.some((c) => c.verdict.reason === "apply_outside_plan")).toBe(true)
})
