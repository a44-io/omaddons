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

// `fn` run with an unstable Array.prototype.sort, as Qt's V4 engine has (bun's is
// stable): an insertion sort that puts each item before the ones it ties with, so every
// tie comes out reversed.
function withTiesReversed(fn) {
  const original = Array.prototype.sort
  Array.prototype.sort = function (compare) {
    const cmp = compare || ((a, b) => (String(a) < String(b) ? -1 : String(a) > String(b) ? 1 : 0))
    const out = []
    for (let i = 0; i < this.length; i++) {
      let at = out.length
      while (at > 0 && cmp(out[at - 1], this[i]) >= 0) at--
      out.splice(at, 0, this[i])
    }
    for (let i = 0; i < out.length; i++) this[i] = out[i]
    return this
  }
  try {
    return fn()
  } finally {
    Array.prototype.sort = original
  }
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
    expect(M.WIDGET_EXPLAIN_INSTANCES_MAX).toBe(5)
    expect(M.WIDGET_EXPLAIN_EVIDENCE_MAX).toBe(12)
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
  fm: "fm-secrets-credential-file-mode",
  path: "/home/fixture/Games/World of Warcraft",
  run: "01M8Q2VA7K3N5P8R1T4W6X9Y0Z",
}

// What a template needs beyond its slots to build: a planned template builds as its
// preview (its Yes is its plan's apply), a bound one (a `bind` preview, G2) with its
// view's token.
function runOpts(name) {
  const t = M.COMMANDS[name]
  if (t.preview === "bind") return opts({ token: TOKEN })
  if (t.preview) return opts({ preview: true })
  if (t.bind || M.CLI_PATHS[M.templateShape(name).path].destructive) return opts({ token: TOKEN })
  return opts()
}

// The templates whose real run is refused: they run as their preview, then apply.
const PREVIEWED = ["adopt_target", "install", "prune_backup", "reinstall", "remove", "restore", "restore_addon", "undo_run", "undo_run_target", "update_addon", "update_all", "update_target"]
// The templates bound to their preview's token (G2): their real run needs it.
const BOUND_PREVIEWS = ["doctor_fix", "doctor_fix_target"]

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

  test("--progress json goes on mutations and every read that asks the sources", () => {
    const cases = [
      ["check", {}, true],
      ["search", { target: "retail", query: "x" }, true],
      ["search_all", { query: "x" }, true],
      ["changelog", { target: "retail", addon: "DBM" }, true],
      ["status", {}, false],
      ["list", {}, false],
      ["errors", {}, false],
      ["backups", {}, false],
    ]
    for (const [name, slots, progress] of cases) {
      expect([name, M.buildArgv(name, slots, opts()).argv.includes("--progress")]).toEqual([name, progress])
    }
    const cli = (argv) => M.cliArgv(argv, opts()).argv.includes("--progress")
    expect([cli(["omaddons", "outdated"]), cli(["omaddons", "search", "dbm"]), cli(["omaddons", "info", "--changelog", "dbm"]),
      cli(["omaddons", "info", "dbm"]), cli(["omaddons", "list", "-v"])]).toEqual([true, true, true, false, false])
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
    expect(Object.keys(M.COMMANDS).filter((name) => ["match", "confirm"].includes(M.COMMANDS[name].preview)).sort()).toEqual(PREVIEWED)
    expect(Object.keys(M.COMMANDS).filter((name) => M.COMMANDS[name].preview === "bind").sort()).toEqual(BOUND_PREVIEWS)
    // A bound preview's real run needs its token, and its preview none.
    for (const name of BOUND_PREVIEWS) {
      expect([name, reasonOf(() => M.buildArgv(name, benignSlots(name), opts()))]).toEqual([name, "token_required"])
      expect([name, reasonOf(() => M.buildArgv(name, benignSlots(name), opts({ confirmed: true })))]).toEqual([name, "token_required"])
      expect([name, reasonOf(() => M.buildArgv(name, benignSlots(name), opts({ preview: true, token: TOKEN })))]).toEqual([name, "token_in_preview"])
      expect([name, M.buildArgv(name, benignSlots(name), opts({ preview: true })).argv.includes("--dry-run")]).toEqual([name, true])
    }
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
        // A failure-mode id has its own grammar: every hostile value is refused.
        if (slot === "fm") {
          for (const value of HOSTILE) {
            const slots = Object.assign(benignSlots(name), { fm: value })
            expect([name, value, reasonOf(() => M.buildArgv(name, slots, runOpts(name)))]).toEqual([name, value, "bad_fm"])
          }
          continue
        }
        // A run's id has its own grammar (a whole ULID, 1n2p.30): every hostile value is
        // refused.
        if (slot === "run") {
          for (const value of HOSTILE) {
            const slots = Object.assign(benignSlots(name), { run: value })
            expect([name, value, reasonOf(() => M.buildArgv(name, slots, runOpts(name)))]).toEqual([name, value, "bad_run"])
          }
          continue
        }
        // A client folder has its own grammar (absolute, one line, 1n2p.32): every
        // hostile value is refused, and the same value made absolute is one element after --.
        if (slot === "path") {
          for (const value of HOSTILE) {
            const slots = Object.assign(benignSlots(name), { path: value })
            expect([name, value, reasonOf(() => M.buildArgv(name, slots, runOpts(name)))]).toEqual([name, value, "bad_path"])
            const argv = M.buildArgv(name, Object.assign(benignSlots(name), { path: "/" + value }), runOpts(name)).argv
            expect(argv.slice(argv.indexOf("--") + 1)).toEqual(["/" + value])
          }
          continue
        }
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
      if (t.preview === "bind") expect([name, M.CLI_PATHS[shape.path].binds, M.storesPlan(shape.path)]).toEqual([name, true, false])
      else if (t.preview) expect([name, ["match", "confirm"].includes(t.preview), M.storesPlan(shape.path)]).toEqual([name, true, true])
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
  // G2: a repair runs only through the bound doctor_fix templates, its undo only through
  // undo's plan, and consent is a terminal's (M4-D38).
  [["omaddons", "doctor", "fix", "--only", "fm-x"], "own_flow_only", false],
  [["omaddons", "doctor", "fix", "--only", "fm-x", "-t", "retail", "--dry-run"], "own_flow_only", false],
  [["omaddons", "doctor", "undo", "2026-10-30T18-03-40Z__01M8S0000000000000000000000"], "own_flow_only", false],
  [["omaddons", "doctor", "fix", "--only", "fm-x", "-t", "retail", "--force", "--yes"], "consent_flag", false],
  [["omaddons", "install", "Foo", "-t", "retail", "--force"], "consent_flag", false],
  [["omaddons", "backup", "import", "x.tar", "--force"], "consent_flag", false],
  [["omaddons", "install", "--force=true", "Foo"], "consent_flag", false],
  [["omaddons", "remove", "--", "--force"], "consent_flag", false],
  [["omaddons", "doctor", "gc", "--before", "30d", "--dry-run"], "doctor_gc", false],
  [["omaddons", "doctor", "gc", "--before", "30d", "--yes"], "consent_flag", false],
  [["omaddons", "--yes", "status"], "consent_flag", false],
  [["omaddons", "doctor", "fix", "--if-match", TOKEN], "widget_flag", false],
  [["omaddons", "doctor", "explain", "fm-x", "-t", "retail"], "", false],
  [["omaddons", "doctor", "diagnose"], "", false],
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
    // targets rm binds and stores no plan (omad-launch-week-fixes-bmcx.2): its preview's
    // token runs it, and a bare confirmation does not.
    const rm = ["omaddons", "targets", "rm", "retail"]
    expect(reasonOf(() => M.cliArgv(rm, opts()))).toBe("destructive_without_preview")
    expect(reasonOf(() => M.cliArgv(rm, opts({ confirmed: true })))).toBe("destructive_without_preview")
    const rmBound = M.cliArgv(rm, opts({ token: TOKEN })).argv
    expect(rmBound.slice(-3)).toEqual(["targets", "rm", "retail"])
    expect(rmBound[rmBound.indexOf("--if-match") + 1]).toBe(TOKEN)
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
    const rmAlt = M.cliArgv(["omaddons", "targets", "rm", "retail", "--dry-run"], opts({ token: TOKEN })).argv
    expect(rmAlt).not.toContain("--dry-run")
    const bound = M.cliArgv(["omaddons", "errors", "dismiss", "-t", "retail", "--dry-run"], opts({ token: TOKEN })).argv
    expect([bound.includes("--dry-run"), bound.includes("--if-match")]).toEqual([false, true])
    const literal = M.cliArgv(["omaddons", "targets", "rm", "--", "--dry-run"], opts({ token: TOKEN })).argv
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
    const model = M.reduceStatus(fixture("status-updates"))
    expect(model.failed).toBe(false)
    expect(model.summary.state).toBe("updates")
    expect(model.summary.updates.available).toBe(4)
    expect(model.targets.map((t) => t.id)).toEqual(["era", "retail"])
    expect(model.commands.map((c) => [c.action, c.runnable])).toEqual([["update", true], ["list_unmanaged", true]])
    expect(model.lastChange.unknown).toBe(false)
  })

  test("idle", () => {
    const model = M.reduceStatus(fixture("status-idle"))
    expect(model.summary.state).toBe("idle")
    expect(model.summary.issuesUrgent).toBe(0)
  })

  test("attention", () => {
    const model = M.reduceStatus(fixture("status-attention"))
    expect([model.summary.state, model.summary.issuesUrgent]).toEqual(["attention", 1])
    const issue = model.targets[0].issues[0]
    expect([issue.severity, issue.urgent, issue.addon]).toEqual(["P1", true, "Baz"])
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

  test("a failed status keeps nothing and is failed; no answer yet is not", () => {
    const model = M.reduceStatus(fixture("failure"))
    expect([model.failed, model.summary, model.targets.length]).toEqual([true, null, 0])
    expect(M.reduceStatus(null).failed).toBe(false)
    expect(M.reduceStatus(undefined).failed).toBe(false)
  })
})

// The first run (1n2p.32, design doc §6.4): the welcome card, and the path box's checks.
describe("first run", () => {
  const noClients = () => {
    const env = JSON.parse(JSON.stringify(fixture("status-idle")))
    env.data.targets = []
    env.data.summary.targets = 0
    return M.reduceStatus(env)
  }

  test("with no client on this computer, every view shows the welcome card and none says it is empty", () => {
    const model = noClients()
    const card = M.welcomeCard(model.summary, model.targets)
    expect(card).toEqual({ kind: "welcome", title: "No World of Warcraft here yet", sentence: M.COPY["welcome.sentence"],
      buttons: [{ name: "rescan", text: "Scan again" }, { name: "add", text: "Add a client folder…" }] })
    for (const view of M.VIEWS) {
      const s = M.viewState(view, { service: { summary: model.summary, targets: model.targets, phase: "ok" }, nowMs: NOW })
      expect([view, s.state, s.sentence, s.notices.map((n) => [n.kind, n.sentence, n.actions.map((a) => a.name)])])
        .toEqual([view, "welcome", "", [["welcome", "No World of Warcraft here yet", ["rescan", "add"]]]])
    }
    // The hero says there is none yet, never "all good".
    expect(M.heroMeta(model.summary, model.targets, "", [])).toBe("NO CLIENTS YET")
    // A client here, or no answer yet: no welcome card.
    const some = M.reduceStatus(fixture("status-idle"))
    expect(M.welcomeCard(some.summary, some.targets)).toBeNull()
    expect(M.welcomeCard(null, [])).toBeNull()
    // Only a client gone from this computer counts as none.
    const gone = JSON.parse(JSON.stringify(fixture("status-idle")))
    gone.data.targets[0].missing = true
    const g = M.reduceStatus(gone)
    expect(M.welcomeCard(g.summary, g.targets).kind).toBe("welcome")
    // No onboarding sentence names an omaddons command line.
    const commandLine = /\bomaddons (install|update|remove|undo|status|targets|adopt|errors|backup|doctor|search|list|outdated|apply|config|omarchy)\b|(^|\s)--?[a-z]/
    for (const id of Object.keys(M.COPY).filter((k) => /^(welcome|addclient)\./.test(k))) {
      expect([id, commandLine.test(String(M.COPY[id]))]).toEqual([id, false])
    }
  })

  test("the path box takes a folder from / or ~/, one line, and says why when it cannot", () => {
    const home = CONTEXT.HOME
    const cases = [
      ["/home/fixture/Games/wow", "/home/fixture/Games/wow", ""],
      ["  /mnt/games/World of Warcraft  ", "/mnt/games/World of Warcraft", ""],
      ["~/Games/wow", home + "/Games/wow", ""],
      ["~", home, ""],
      ["/-t", "/-t", ""],
      ["", "", "Type the folder's path first."],
      ["   ", "", "Type the folder's path first."],
      ["Games/wow", "", "Type the whole path, from / or ~/."],
      ["~other/Games", "", "Type the whole path, from / or ~/."],
      ["-t", "", "Type the whole path, from / or ~/."],
      ["/a\nb", "", M.COPY["addclient.error.bad"]],
      ["/a\u0000b", "", M.COPY["addclient.error.bad"]],
      ["/" + "x".repeat(M.PATH_MAX_BYTES), "", M.COPY["addclient.error.bad"]],
    ]
    for (const [typed, path, error] of cases) {
      expect([typed, M.clientPath(typed, home)]).toEqual([typed, { path, error }])
    }
    // With no home known, "~" stays relative.
    expect(M.clientPath("~/Games", "").error).toBe("Type the whole path, from / or ~/.")
  })

  test("over the binary's envelopes: no client, an add refused, then an add the scan lists", () => {
    const none = M.reduceStatus(fixture("status-none"))
    expect(M.welcomeCard(none.summary, none.targets).kind).toBe("welcome")
    expect(fixture("targets-add-none").error.code).toBe("target_not_found")
    expect(M.resultLine(fixture("targets-add"), CONTEXT, [])).toBe("Added Wrath 3.3.5a private server")
    const again = JSON.parse(JSON.stringify(fixture("targets-add")))
    again.data.changed = false
    expect(M.resultLine(again, CONTEXT, [])).toBe("Nothing new to add: omaddons knows this client already")
    const added = M.reduceStatus(fixture("status-added"))
    expect(added.targets.map((t) => [t.displayName, t.missing])).toEqual([["Wrath 3.3.5a private server", false]])
    expect(M.welcomeCard(added.summary, added.targets)).toBeNull()
  })

  test("the first run's cards come one at a time: take over, then BugGrabber, then the key tip", () => {
    const adoptable = M.reduceStatus(fixture("status-adoptable"))
    const input = (status, extra) => Object.assign({ summary: status.summary, targets: status.targets, errors: null,
      dismissed: {}, keyHint: true, openedByMouse: true, pluginId: "a44.fixture" }, extra || {})
    // Take over: a client that manages nothing, with folders there.
    const adopt = M.onboardingCard(input(adoptable))
    expect([adopt.kind, adopt.target, adopt.title, adopt.buttons.map((b) => b.name)])
      .toEqual(["adopt", "retail", "Take over the addons already on Retail?", ["take_over", "not_now"]])
    expect(adopt.sentence).toBe("3 folders there came by hand or from another addon manager. omaddons can keep track of them as they are: nothing is reinstalled or moved.")
    // Not now: the next card (here the key tip; no client records nothing).
    const keyhint = M.onboardingCard(input(adoptable, { dismissed: { adopt: ["retail"] } }))
    expect([keyhint.kind, keyhint.buttons.map((b) => b.text)]).toEqual(["keyhint", ["Copy the line", "Got it"]])
    expect(keyhint.line).toBe('o.bind("SUPER + ALT + W", "WoW addons", { panel = "a44.fixture" })')
    // The key tip only after a mouse open, only while its setting is On, once dismissed gone.
    const quiet = { dismissed: { adopt: ["retail"] } }
    expect(M.onboardingCard(input(adoptable, Object.assign({ openedByMouse: false }, quiet)))).toBeNull()
    expect(M.onboardingCard(input(adoptable, Object.assign({ keyHint: false }, quiet)))).toBeNull()
    expect(M.onboardingCard(input(adoptable, { dismissed: { adopt: ["retail"], keyhint: [""] } }))).toBeNull()
    // BugGrabber: Classic Era records nothing, and the errors read knows its build.
    const errs = M.reduceStatus(fixture("status-errors"))
    expect(M.needsCaptureRead(errs.targets, null)).toBe(true)
    expect(M.needsCaptureRead(errs.targets, M.reduceErrors(fixture("errors")))).toBe(false)
    expect(M.onboardingCard(input(errs, { keyHint: false })), "no read yet: no card").toBeNull()
    const bg = M.onboardingCard(input(errs, { errors: M.reduceErrors(fixture("errors")) }))
    expect([bg.kind, bg.target, bg.title, bg.buttons.map((b) => b.text)])
      .toEqual(["buggrabber", "era", "Record Lua errors on Classic Era?", ["Install BugGrabber", "Not now"]])
    expect(bg.action.argv).toEqual(["omaddons", "install", "buggrabber", "-t", "era"])
    // A client no build is known for gets no card.
    const unknown = JSON.parse(JSON.stringify(fixture("errors")))
    unknown.commands = []
    expect(M.onboardingCard(input(errs, { errors: M.reduceErrors(unknown), keyHint: false }))).toBeNull()
    // With no client here, the views' welcome card comes first: none of these.
    const none = M.reduceStatus(fixture("status-none"))
    expect(M.onboardingCard(input(none))).toBeNull()
    expect(M.onboardingCard(input(adoptable, { summary: null }))).toBeNull()
    // No onboarding sentence names an omaddons command line.
    for (const id of Object.keys(M.COPY).filter((k) => /^onboard\./.test(k))) {
      const words = typeof M.COPY[id] === "string" ? [M.COPY[id]] : Object.values(M.COPY[id])
      for (const w of words) expect([id, /\bomaddons (install|adopt|targets|errors|status)\b/.test(w)]).toEqual([id, false])
    }
  })

  test("targets add takes only an absolute path, after --", () => {
    const built = M.buildArgv("targets_add", { path: "/home/fixture/Games/World of Warcraft" }, opts())
    expect(built.argv.slice(-4)).toEqual(["targets", "add", "--", "/home/fixture/Games/World of Warcraft"])
    for (const path of ["Games/wow", "-t", "--force", "/a\nb", "/" + "y".repeat(M.PATH_MAX_BYTES)]) {
      expect([path, reasonOf(() => M.buildArgv("targets_add", { path }, opts()))]).toEqual([path, "bad_path"])
    }
    expect(reasonOf(() => M.buildArgv("targets_add", { path: "" }, opts()))).toBe("bad_value")
  })
})

// Failures and recovery (1n2p.34, design doc §6.4): the cards, their details, the
// retries and an interrupted change, over envelopes the binary wrote.
describe("failures and recovery", () => {
  const FACTS = {
    label: "Refresh", outcome: "malformed", exitCode: 101,
    argv: ["/home/fixture/.cargo/bin/omaddons", "--json", "status", "--cached", "--reason", "two words"],
    tail: Array.from({ length: 15 }, (_, i) => "line " + i + " at /home/fixture/.config/omaddons/config.toml"),
  }
  const INFO = { binary: "/home/fixture/.cargo/bin/omaddons", home: "/home/fixture", mine: "0.1.0", deadlineSec: 20, nowMs: NOW }

  test("retries come at 5 s, 15 s, then every 60 s", () => {
    expect([0, 1, 2, 3, 9, -1, null].map((s) => M.retryDelayMs(s))).toEqual([5000, 15000, 60000, 60000, 60000, 5000, 5000])
    expect([0, 1, 2].map((s) => M.retryDelayMs(s, [30, 60]))).toEqual([30, 60, 60])
  })

  test("another build is typed: another version, an answer it cannot read, or the same version's other files", () => {
    const build = { TOOL_VERSION: fixture("status-idle").tool_version }
    const other = JSON.parse(JSON.stringify(fixture("status-idle")))
    other.tool_version = "9.9.9"
    expect(M.incompatibleOf(other, build)).toEqual({ kind: "version", theirs: "9.9.9" })
    expect(M.incompatibleOf({ nonsense: true }, build)).toEqual({ kind: "version", theirs: "" })
    const major = JSON.parse(JSON.stringify(fixture("status-idle")))
    major.schema_version = "2.0"
    expect(M.incompatibleOf(major, build).kind).toBe("version")
    expect(M.incompatibleOf(fixture("status-idle"), build).kind).toBe("outdated")
  })

  test("the details say what ran and how it ended, with the home as ~ and the binary as omaddons", () => {
    const text = M.failureDetails(FACTS, CONTEXT, "0.1.0")
    const lines = text.split("\n")
    expect(lines.slice(0, 5)).toEqual([
      "What: Refresh",
      'Command: omaddons --json status --cached --reason "two words"',
      "Ended: an answer the panel can't read, exit code 101",
      "Panel: omaddons 0.1.0",
      "Last output:",
    ])
    // The last 12 lines of the child's output, and no path of the home spelled out.
    expect(lines.slice(5)).toEqual(FACTS.tail.slice(-12).map((l) => "  " + l.replace("/home/fixture", "~")))
    expect(text).not.toContain("/home/fixture")
    // Nothing from the environment beyond the home: no other context value appears.
    for (const value of Object.values(CONTEXT)) if (value !== CONTEXT.HOME) expect(text).not.toContain(value)
    const killed = M.failureDetails({ label: "Check now", outcome: "stuck", argv: [], tail: [] }, CONTEXT, "")
    expect(killed.split("\n")).toEqual(["What: Check now", "Ended: still running after SIGKILL, let go, no exit code", "Panel: omaddons unknown"])
    expect(M.failureDetails(null, null, null).split("\n")[0]).toBe("What: unknown")
  })

  test("a request with no envelope says what happened in words", () => {
    expect(["timeout", "malformed", "oversized", "cancelled", "stuck", "missing", "weird"].map((o) => M.failureWords("Update all", o))).toEqual([
      "Update all didn't finish: omaddons took too long",
      "Update all: omaddons answered with something the panel can't read",
      "Update all: omaddons' answer was too large to read",
      "Update all was stopped",
      "omaddons stopped answering during Update all; the panel freed its worker and goes on",
      "Can't find omaddons",
      "Update all didn't finish (weird)",
    ])
  })

  test("each failure has its own card: words, one action besides Retry, and details where they help", () => {
    const card = (phase, extra) => M.phaseCard(phase, Object.assign({}, INFO, extra || {}))
    const missing = card("missing")
    expect([missing.kind, missing.title, missing.actions]).toEqual(["missing", "Can't find omaddons", ["retry", "copy_command"]])
    expect(missing.sentence).toContain("It was at ~/.cargo/bin/omaddons.")
    expect(missing.notes).toEqual(["The panel tries again by itself."])
    expect(M.missingBinaryCommand()).toBe("omaddons omarchy install")
    const cases = [
      ["malformed", "omaddons didn't answer", "It stopped before it said anything the panel can read."],
      ["oversized", "omaddons didn't answer", "Its answer was larger than the panel reads."],
      ["timeout", "omaddons didn't answer in time", "It was still working after 20 seconds, so the panel stopped it."],
      ["stuck", "omaddons stopped answering", "It did not end even when stopped, so the panel let it go and freed its worker."],
      ["cancelled", "omaddons didn't answer", "It stopped before it said anything the panel can read."],
    ]
    for (const [outcome, title, sentence] of cases) {
      const c = card("unreachable", { outcome, failure: Object.assign({}, FACTS, { outcome }) })
      expect([outcome, c.title, c.sentence, c.actions]).toEqual([outcome, title, sentence, ["retry", "copy_details"]])
      expect(c.details).toBe(M.failureDetails(Object.assign({}, FACTS, { outcome }), { HOME: "/home/fixture" }, "0.1.0"))
    }
    const version = card("incompatible", { incompatible: { kind: "version", theirs: "0.2.0" } })
    expect([version.kind, version.title, version.actions, version.notes]).toEqual(["version", "The panel is from another omaddons version", ["reinstall", "retry"], []])
    expect(version.sentence).toBe("Panel 0.1.0, omaddons 0.2.0. Update the panel to match; until then it only shows what it read last.")
    expect(card("incompatible", { incompatible: { kind: "version", theirs: "" } }).sentence).toContain("omaddons a version this panel can't read")
    const outdated = card("incompatible", { incompatible: { kind: "outdated", theirs: "0.1.0" }, reinstalledAtMs: NOW - 120000 })
    expect([outdated.kind, outdated.title]).toEqual(["outdated", "The panel is out of date"])
    expect(outdated.notes).toEqual(["Update the panel ran 2 min ago; this is still the old panel."])
    for (const phase of ["ok", "starting", "", undefined]) expect(M.phaseCard(phase, INFO)).toBeNull()
    // With no facts at all, a card still says something.
    expect(M.phaseCard("unreachable", {}).kind).toBe("malformed")
    expect(M.phaseCard("incompatible", null).kind).toBe("version")
  })

  test("an interrupted change is announced from status alone, and never while another process commits", () => {
    const stuck = M.interruptedChange(M.reduceStatus(fixture("setaside-status")))
    expect(stuck).toEqual({ clients: ["Retail"], target: "retail", title: "A change on Retail was interrupted",
      sentence: "omaddons finishes or undoes it before the next change. See what it would do first." })
    expect(M.interruptedChange(M.reduceStatus(fixture("status-updates")))).toBeNull()
    expect(M.interruptedChange(null)).toBeNull()
    const edit = (name, f) => {
      const env = JSON.parse(JSON.stringify(fixture(name)))
      f(env)
      return M.interruptedChange(M.reduceStatus(env))
    }
    const two = edit("status-updates", (env) => { env.data.journal.pending = 2; env.data.targets.forEach((t) => { t.attention = ["journal_pending"] }) })
    expect([two.title, two.target]).toEqual(["Changes on Classic Era and Retail were interrupted", ""])
    const machine = edit("status-updates", (env) => { env.data.journal.pending = 1 })
    expect([machine.title, machine.clients, machine.target]).toEqual(["A change was interrupted", [], ""])
    expect(edit("setaside-status", (env) => { env.data.journal.in_progress = true })).toBeNull()
    expect(M.reduceStatus(fixture("setaside-status")).journal).toEqual({ pending: 1, inProgress: false })
  })
})

// The bar button (1n2p.16, design doc §2.5): Model.barState and Model.barTooltip over
// envelopes the binary wrote.
describe("the bar button", () => {
  const status = (name) => M.reduceStatus(fixture(name))
  // An answer applied a moment ago, its oldest check 25 minutes before `now`.
  const STALE_AFTER = M.barStaleAfterMs({})
  const ctxFor = (model, extra) => {
    const oldest = M.oldestCheck(model.targets, "")
    const now = oldest ? Date.parse(oldest) + 25 * 60000 : NOW
    return Object.assign({ phase: "ok", working: false, nowMs: now, answeredAtMs: now - 1000, staleAfterMs: STALE_AFTER }, extra || {})
  }
  const bar = (model, extra) => M.barState(model, ctxFor(model, extra))
  const view = (b) => [b.state, b.badge, b.badgeUrgent, b.urgent, b.dimmed, b.spinning, b.mark]
  const withEdit = (name, edit) => {
    const env = JSON.parse(JSON.stringify(fixture(name)))
    edit(env)
    return M.reduceStatus(env)
  }
  // The CLI words the copy guide's glossary (design doc §6.3) keeps out of the widget,
  // and anything shaped like a command line.
  const CLI_WORDS = [
    /\b(targets?|refs?|plans?|preview|token|precondition|transactions?|tx|run|SavedVariables|WTF|flavou?r|family|interface|issues?|P[0-3])\b/,
    /\bfm-[a-z_-]+/, /(^|\s)--[a-z]/, /-t\s/,
    /\bomaddons (install|update|remove|undo|status|targets|adopt|errors|backup|doctor|search|list|outdated|apply|config|omarchy)\b/,
  ]
  const cliWord = (line) => CLI_WORDS.some((re) => re.test(line))

  test("idle: nothing to do, no badge, no mark", () => {
    const b = bar(status("status-idle"))
    expect(view(b)).toEqual(["idle", 0, false, false, false, false, ""])
    expect(b.tooltip).toEqual(["All good", "Checked 25 min ago", "Left: open · Middle: Updates · Right: check for updates"])
  })

  test("updates: the accent badge counts them, one line per client", () => {
    const b = bar(status("status-updates"))
    expect(view(b)).toEqual(["updates", 4, false, false, false, false, ""])
    expect(b.tooltip).toEqual(["4 updates", "Classic Era · 2 updates", "Retail · 2 updates", "Checked 25 min ago",
      "Left: open · Middle: Updates · Right: check for updates"])
  })

  test("attention without updates: the urgent badge counts the P1 problem, the attention mark says so", () => {
    const b = bar(status("status-attention"))
    expect(view(b)).toEqual(["attention", 1, true, true, false, false, "attention"])
    expect(b.markUrgent).toBe(true)
    expect(b.tooltip[0]).toBe("1 problem needs attention")
    expect(b.tooltip[1]).toBe("Retail · 1 problem needs attention")
    expect(b.tooltip[b.tooltip.length - 1]).toBe("Left: open · Middle: Issues · Right: check for updates")
  })

  test("attention with updates: the badge counts the reason for its colour, never the updates (UX-30)", () => {
    const b = bar(status("status-errors"))
    expect(view(b)).toEqual(["attention", 2, true, true, false, false, "attention"])
    expect(b.tooltip[0]).toBe("2 new Lua errors · 1 update")
    expect(b.tooltip).toContain("Retail · 2 new Lua errors · 1 update")
    expect(b.tooltip[b.tooltip.length - 1]).toBe("Left: open · Middle: Errors · Right: check for updates")
    // P0 and P1 problems and new errors add up; a P2 never makes it urgent.
    const both = withEdit("status-errors", (env) => { env.data.summary.issues.p0 = 1; env.data.summary.issues.p1 = 2 })
    expect(bar(both).badge).toBe(5)
    const minor = withEdit("status-updates", (env) => { env.data.summary.issues.p2 = 3 })
    expect(view(bar(minor)).slice(0, 3)).toEqual(["updates", 4, false])
    expect(bar(minor).tooltip[0]).toBe("4 updates · 3 problems")
  })

  test("playing: a running client adds the running mark and says so on its line", () => {
    const b = bar(status("status-running"))
    expect(view(b)).toEqual(["updates", 4, false, false, false, false, "running"])
    expect([b.playing, b.markUrgent]).toEqual([true, false])
    expect(b.tooltip).toContain("Retail · 2 updates · running")
    // The running mark yields to the attention mark; the line still says it runs.
    const both = withEdit("status-errors", (env) => { env.data.targets[1].running = true })
    expect([bar(both).playing, bar(both).mark]).toEqual([true, "attention"])
    expect(bar(both).tooltip).toContain("Retail · 2 new Lua errors · 1 update · running")
    const clients = bar(status("status-clients"))
    expect(clients.mark).toBe("running")
    expect(clients.tooltip).toContain("Mists of Pandaria Classic · running")
    // A client no longer installed says so, not its problem.
    expect(clients.tooltip).toContain("Burning Crusade Anniversary · not found")
  })

  test("working: the glyph spins in every state but broken, the badge stays", () => {
    const b = bar(status("status-updates"), { working: true })
    expect(view(b)).toEqual(["updates", 4, false, false, false, true, ""])
    expect(M.barState(null, { phase: "starting", working: true }).spinning).toBe(true)
    expect(M.barState(status("status-updates"), { phase: "missing", working: true }).spinning).toBe(false)
  })

  test("stale: past the safety net the bar keeps the last answer, dimmed and marked", () => {
    const model = status("status-errors")
    const fresh = bar(model)
    const ctx = ctxFor(model)
    const at = ctx.nowMs - STALE_AFTER - 1
    const b = M.barState(model, Object.assign({}, ctx, { answeredAtMs: at }))
    expect(view(b)).toEqual(["stale", 2, true, true, true, false, "stale"])
    expect(b.tooltip[0]).toMatch(/^Last heard from omaddons (\d+ min|\d+ hours?) ago$/)
    expect(b.tooltip.slice(1)).toEqual(fresh.tooltip)
    // Exactly the safety net is still fresh; an unreachable answer is too, until then.
    expect(M.barState(model, Object.assign({}, ctx, { answeredAtMs: ctx.nowMs - STALE_AFTER })).state).toBe("attention")
    expect(M.barState(model, Object.assign({}, ctx, { phase: "unreachable" })).state).toBe("attention")
    expect(M.barState(model, Object.assign({}, ctx, { phase: "unreachable", answeredAtMs: at })).state).toBe("stale")
    // A stale bar shows no running client: that answer is old.
    const running = status("status-running")
    expect(M.barState(running, Object.assign({}, ctxFor(running), { answeredAtMs: 1 })).mark).toBe("stale")
  })

  test("stale after the safety net: refreshIntervalSec plus a status read's deadline", () => {
    expect(M.barStaleAfterMs({})).toBe((300 + M.WIDGET_LOCAL_DEADLINE_SEC) * 1000)
    expect(M.barStaleAfterMs({ refreshIntervalSec: 60 })).toBe((60 + M.WIDGET_LOCAL_DEADLINE_SEC) * 1000)
    expect(M.barStaleAfterMs({ refreshIntervalSec: 5 })).toBe(M.barStaleAfterMs({}))
  })

  test("broken: no binary, another build, a failed status or no answer ever; dimmed with a warning mark", () => {
    const model = status("status-errors")
    const cases = [
      ["missing", model, "Can't find omaddons"],
      ["incompatible", model, "The panel is from another omaddons version"],
      ["unreachable", M.reduceStatus(null), "omaddons didn't answer"],
      ["ok", status("failure"), "omaddons ran into a problem"],
    ]
    for (const [phase, m, head] of cases) {
      const b = M.barState(m, { phase, nowMs: NOW, answeredAtMs: NOW, staleAfterMs: STALE_AFTER })
      expect([phase, ...view(b)]).toEqual([phase, "broken", 0, false, false, true, false, "broken"])
      expect(b.tooltip).toEqual([head, "Left: open to see what to do"])
    }
  })

  test("starting and empty: dimmed with no mark, and the tooltip says which", () => {
    const starting = M.barState(null, { phase: "starting" })
    expect(view(starting)).toEqual(["starting", 0, false, false, true, false, ""])
    expect(starting.tooltip).toEqual(["Asking omaddons…", "Left: open · Middle: Updates · Right: check for updates"])
    const none = withEdit("status-idle", (env) => { env.data.targets = []; env.data.summary.targets = 0 })
    const empty = bar(none)
    expect(view(empty)).toEqual(["empty", 0, false, false, true, false, ""])
    expect(empty.tooltip).toEqual(["No World of Warcraft here yet", "Left: open · Middle: Updates · Right: check for updates"])
    // A client no longer installed counts as none, and says it is gone.
    const gone = withEdit("status-idle", (env) => { env.data.targets[0].missing = true })
    expect(bar(gone).state).toBe("empty")
  })

  test("the tooltip names clients by display name, the problems of no client, and checks never made", () => {
    const machine = withEdit("status-check-due", (env) => {
      env.data.issues = [JSON.parse(JSON.stringify(fixture("status-attention").data.targets[0].issues[0]))]
      env.data.summary.issues.p1 = 1
      env.data.targets[1].running = null
    })
    const b = M.barState(machine, { phase: "ok", nowMs: NOW, answeredAtMs: NOW, staleAfterMs: STALE_AFTER })
    expect(b.tooltip).toEqual(["1 problem needs attention", "This computer · 1 problem needs attention",
      "Can't tell if the game is running", "Not checked for updates yet", "Left: open · Middle: Issues · Right: check for updates"])
  })

  test("no tooltip line holds a CLI word or a client id, and every one ends with the click hints", () => {
    const names = ["status-idle", "status-updates", "status-attention", "status-errors", "status-running", "status-clients",
      "status-check-due", "status-errors-unchecked", "status-errors-stale", "status-errors-partial", "status-after-update", "failure"]
    const phases = ["starting", "ok", "missing", "unreachable", "incompatible"]
    for (const name of names) {
      const model = status(name)
      const ids = model.targets.map((t) => t.id)
      for (const phase of phases) {
        for (const answeredAtMs of [ctxFor(model).nowMs, 1]) {
          const lines = M.barState(model, Object.assign(ctxFor(model), { phase, answeredAtMs })).tooltip
          expect(lines.length).toBeGreaterThan(1)
          expect(lines[lines.length - 1]).toMatch(/^Left: open/)
          for (const line of lines) {
            expect([name, phase, line, cliWord(line)]).toEqual([name, phase, line, false])
            for (const id of ids) expect([name, line, new RegExp("\\b" + id + "\\b").test(line)]).toEqual([name, line, false])
          }
        }
      }
    }
  })

  test("every combination: one state, its look follows from it, and the states look distinct besides colour", () => {
    const base = fixture("status-updates")
    // What tells a bar apart without its colours (the badge's fill and the glyph's are
    // colour): whether a badge shows, the mark, and dimming; the spinner is the glyph.
    const looks = {}
    for (const errors of [0, 2]) for (const urgent of [0, 1]) for (const updates of [0, 3]) for (const running of [false, true])
      for (const working of [false, true]) for (const phase of ["starting", "ok", "missing", "unreachable", "incompatible"])
        for (const age of [0, STALE_AFTER + 1]) for (const clients of [true, false]) {
          const env = JSON.parse(JSON.stringify(base))
          env.data.summary.errors.new = errors
          env.data.summary.issues.p1 = urgent
          env.data.summary.updates.available = updates
          env.data.targets[1].running = running
          if (!clients) env.data.targets = []
          const model = phase === "starting" ? null : M.reduceStatus(env)
          const ctx = { phase, working, nowMs: NOW, answeredAtMs: NOW - age, staleAfterMs: STALE_AFTER }
          const b = M.barState(model, ctx)
          const label = JSON.stringify({ errors, urgent, updates, running, working, phase, age, clients })
          expect([label, M.BAR_STATES.includes(b.state)]).toEqual([label, true])
          // Pure: the same answer twice, and its inputs untouched.
          expect(M.barState(model, ctx)).toEqual(b)
          expect(ctx.answeredAtMs).toBe(NOW - age)
          const dimmed = ["broken", "starting", "stale", "empty"].includes(b.state)
          const level = !clients ? "empty" : errors + urgent > 0 ? "attention" : updates > 0 ? "updates" : "idle"
          expect([label, b.dimmed]).toEqual([label, dimmed])
          expect([label, b.spinning]).toEqual([label, working && b.state !== "broken"])
          expect([label, b.badge]).toEqual([label, ["attention", "updates", "stale"].includes(b.state)
            ? (level === "attention" ? errors + urgent : level === "updates" ? updates : 0) : 0])
          expect([label, b.badgeUrgent, b.urgent]).toEqual([label, b.badge > 0 && level === "attention", b.badge > 0 && level === "attention"])
          const playing = running && ["attention", "updates", "idle"].includes(b.state)
          expect([label, b.playing]).toEqual([label, playing])
          expect([label, b.mark]).toEqual([label, ["broken", "stale", "attention"].includes(b.state) ? b.state : playing ? "running" : ""])
          expect([label, b.markUrgent]).toEqual([label, ["broken", "attention"].includes(b.mark)])
          // The state as the snapshots name it: playing is its own look.
          const shown = b.mark === "running" ? b.state + "+playing" : b.state
          const look = [b.badge > 0, b.mark, b.dimmed].join("|")
          if (!working) (looks[look] = looks[look] || new Set()).add(shown)
        }
    // decided: starting and empty share a look (nothing to show yet; the tooltip says
    // which). Every other look is one state's.
    const shared = Object.entries(looks).filter(([, states]) => states.size > 1)
    expect(shared.map(([look, states]) => [look, [...states].sort()])).toEqual([["false||true", ["empty", "starting"]]])
    const all = new Set(Object.values(looks).flatMap((states) => [...states]))
    expect([...all].sort()).toEqual([...M.BAR_STATES, "idle+playing", "updates+playing"].sort())
  })
})

describe("the key map", () => {
  // The contexts whose rows one lookup reads together: no key may appear twice in one.
  const LOOKUPS = M.VIEWS.map((v) => M.keyScopes(v)).concat([M.keyScopes("card"), ["field"], ["sheet"], ["help"]])
  // The keys PanelKeyCatcher turns into its own signals (moves and the delete key):
  // never a text key the frame looks up.
  const CATCHER = ["j", "k", "h", "l", "x", "X", "Down", "Up", "Left", "Right", "Esc", "Tab", "Shift+Tab"]

  test("each key does what its context's row says", () => {
    table([
      ["U_updates_all", ["U", "updates"], { action: "update_all", step: 0 }],
      ["u_is_this_addon_in_updates", ["u", "updates"], { action: "update", step: 0 }],
      ["u_in_installed", ["u", "installed"], { action: "update", step: 0 }],
      ["e_shows_notes", ["e", "updates"], { action: "notes", step: 0 }],
      ["s_is_search_everywhere", ["s", "backups"], { action: "search", step: 0 }],
      ["i_installs_a_result", ["i", "search"], { action: "install", step: 0 }],
      ["a_digit_names_its_view", ["3", "updates"], { action: "view", step: 0, view: "search" }],
      ["the_errors_digit", ["4", "issues"], { action: "view", step: 0, view: "errors" }],
      ["the_backups_digit", ["6", "issues"], { action: "view", step: 0, view: "backups" }],
      ["the_last_digit", ["7", "issues"], { action: "view", step: 0, view: "history" }],
      // History (1n2p.30): Enter opens a change's page, `/` filters, Ctrl+Z undoes (the
      // frame asks the view first, so the cursored change).
      ["enter_opens_a_change", ["Enter", "history"], { action: "enter", step: 0 }],
      ["slash_filters_the_history", ["/", "history"], { action: "filter", step: 0 }],
      ["Ctrl_Z_in_history", ["Ctrl+Z", "history"], { action: "undo", step: 0 }],
      ["x_in_history_says_why_not", ["x", "history"], { action: null, note: "Nothing to remove here" }],
      ["t_steps_forward", ["t", "installed"], { action: "client", step: 1 }],
      ["T_steps_back", ["T", "installed"], { action: "client", step: -1 }],
      ["g_goes_to_the_top", ["g", "updates"], { action: "edge", step: -1 }],
      ["G_to_the_bottom", ["G", "updates"], { action: "edge", step: 1 }],
      ["PgDn", ["PgDn", "installed"], { action: "page", step: 1 }],
      ["Ctrl_Z_undoes", ["Ctrl+Z", "backups"], { action: "undo", step: 0 }],
      ["question_mark_opens_the_reference", ["?", "search"], { action: "help", step: 0 }],
      ["x_removes_an_addon", ["x", "installed"], { action: "remove", step: 0 }],
      ["x_deletes_a_backup", ["x", "backups"], { action: "remove", step: 0 }],
      ["enter_on_a_card_presses", ["Enter", "card"], { action: "press", step: 0 }],
      ["the_reference_opens_on_a_card", ["?", "card"], { action: "help", step: 0 }],
      ["views_step_on_a_card", ["h", "card"], { action: "view_step", step: -1 }],
      ["no_actions_on_a_card", ["c", "card"], null],
      ["a_key_another_view_has", ["p", "issues"], { action: null, note: "Nothing to pin here" }],
      ["install_outside_search", ["i", "updates"], { action: null, note: "Nothing to install here" }],
      ["x_where_nothing_goes", ["x", "updates"], { action: null, note: "Nothing to remove here" }],
      ["no_such_key", ["q", "updates"], null],
      ["z_zooms", ["z", "updates"], { action: "zoom", step: 0 }],
      ["z_zooms_on_a_card_too", ["z", "card"], { action: "zoom", step: 0 }],
      ["ctrl_equals_grows_the_text", ["Ctrl+=", "search"], { action: "text_zoom", step: 1 }],
      ["ctrl_minus_shrinks_it", ["Ctrl+-", "search"], { action: "text_zoom", step: -1 }],
      ["ctrl_0_resets_it", ["Ctrl+0", "backups"], { action: "text_zoom", step: 0 }],
      ["a_page_has_the_frames_keys", ["?", "page"], { action: "help", step: 0 }],
      // The error page (.27) has its own u, r and y; no other view's key.
      ["a_page_has_its_own_keys", ["u", "page"], { action: "update", step: 0 }],
      // An addon page (.23) pins, removes and installs on its client line, and opens its
      // web page; a key no page has does nothing there.
      ["a_page_pins", ["p", "page"], { action: "pin", step: 0 }],
      ["a_page_opens_its_web_page", ["w", "page"], { action: "web", step: 0 }],
      ["a_page_has_no_views_keys", ["S", "page"], null],
      ["slash_filters_the_addons", ["/", "installed"], { action: "filter", step: 0 }],
      ["slash_filters_the_updates", ["/", "updates"], { action: "filter", step: 0 }],
      ["slash_elsewhere_says_why_not", ["/", "backups"], { action: null, note: "Nothing to filter here" }],
      ["S_sorts_the_addons", ["S", "installed"], { action: "sort", step: 0 }],
      ["a_digit_past_the_views", ["8", "updates"], null],
      ["two_letters", ["uu", "updates"], null],
      ["nothing", ["", "updates"], null],
      ["no_context_is_the_first_view", ["u", undefined], { action: "update", step: 0 }],
    ], ([name, context]) => M.panelKey(name, context))
  })

  test("no key is bound twice where one lookup reads", () => {
    for (const scopes of LOOKUPS) {
      const seen = {}
      for (const row of M.KEYMAP.filter((r) => scopes.includes(r.context))) {
        for (const key of row.keys) {
          expect([scopes.join("+"), key, seen[key] || null]).toEqual([scopes.join("+"), key, null])
          seen[key] = row.action
        }
      }
    }
  })

  test("every row is whole, in a known context, and every context has rows", () => {
    const ids = M.KEY_CONTEXTS.map((c) => c.id)
    for (const row of M.KEYMAP) {
      expect([row.action, ids.includes(row.context)]).toEqual([row.action, true])
      expect(row.keys.length).toBeGreaterThan(0)
      for (const field of ["action", "label", "hint"]) expect([row.action, field, typeof row[field]]).toEqual([row.action, field, "string"])
      // A key name is one character, or a name keyName gives.
      for (const key of row.keys) expect([key, key.length === 1 || /^(Esc|Tab|Shift\+Tab|Enter|Space|Left|Right|Up|Down|PgUp|PgDn|Ctrl\+[A-Z0-9=-])$/.test(key)]).toEqual([key, true])
    }
    for (const id of ids) expect([id, M.KEYMAP.some((r) => r.context === id)]).toEqual([id, true])
    // Every view has a context of its own, titled as the switcher titles it.
    for (const v of M.VIEWS) expect(M.KEY_CONTEXTS.find((c) => c.id === v).title).toBe(M.VIEW_TITLES[v])
    expect(M.VIEWS.map((v) => M.VIEW_TITLES[v])).toEqual(["Updates", "Addons", "Search", "Errors", "Issues", "Backups", "History"])
    for (const v of M.VIEWS) expect([v, typeof M.VIEW_GLYPHS[v]]).toEqual([v, "number"])
  })

  test("the digits are the views, one each", () => {
    const digits = M.KEYMAP.find((r) => r.action === "view")
    expect(digits.keys).toEqual(M.VIEWS.map((_, i) => String(i + 1)))
    expect(digits.hint).toBe("1–" + M.VIEWS.length)
    for (const [i, v] of M.VIEWS.entries()) expect(digits.label).toContain(M.VIEW_TITLES[v])
    expect(digits.keys.length).toBe(M.VIEWS.length)
  })

  test("every typed key of a view resolves to its row, and a view-only key says why elsewhere", () => {
    for (const v of M.VIEWS) {
      for (const row of M.KEYMAP.filter((r) => M.keyScopes(v).includes(r.context))) {
        for (const key of row.keys.filter((k) => !CATCHER.includes(k))) {
          expect([v, key, M.panelKey(key, v).action]).toEqual([v, key, row.action])
        }
      }
    }
    // A key only some views bind has words for the others, unless every view binds it.
    for (const row of M.KEYMAP.filter((r) => M.VIEWS.includes(r.context))) {
      const everywhere = M.VIEWS.every((v) => M.KEYMAP.some((r) => r.context === v && r.keys.includes(row.keys[0])))
      if (!everywhere) expect([row.context, row.action, typeof row.nothing]).toEqual([row.context, row.action, "string"])
    }
  })

  test("only a dialog's keys confirm, and never from auto-repeat", () => {
    for (const row of M.KEYMAP.filter((r) => r.confirms)) expect(row.context).toBe("sheet")
    const every = [...new Set(M.KEYMAP.flatMap((r) => r.keys))]
    for (const key of every) {
      expect([key, ["confirm", "press"].includes(M.dialogKey(key, true))]).toEqual([key, false])
    }
    table([
      ["y_confirms", ["y", false], "confirm"],
      ["Y_too", ["Y", false], "confirm"],
      ["held_y_is_inert", ["y", true], "inert"],
      ["enter_presses", ["Enter", false], "press"],
      ["held_enter_is_inert", ["Enter", true], "inert"],
      ["space_presses", ["Space", false], "press"],
      ["esc_cancels_held_or_not", ["Esc", true], "cancel"],
      ["n_cancels", ["n", false], "cancel"],
      ["tab_switches", ["Tab", false], "switch"],
      ["h_switches", ["h", false], "switch"],
      ["j_moves_a_line", ["j", false], "line"],
      ["a_letter_is_inert", ["u", false], "inert"],
      ["nothing_is_inert", ["", false], "inert"],
    ], ([key, held]) => M.dialogKey(key, held))
  })

  test("the reference closes on ? and Esc, scrolls on j and k, and takes every other key", () => {
    table([
      ["question_mark", "?", "close"],
      ["esc", "Esc", "close"],
      ["j", "j", "scroll"],
      ["up", "Up", "scroll"],
      ["a_letter", "u", "inert"],
      ["enter", "Enter", "inert"],
      ["y_never_confirms_here", "y", "inert"],
    ], (key) => M.helpKey(key))
  })

  test("key events get KEYMAP's names", () => {
    const SHIFT = 0x02000000
    const CTRL = 0x04000000
    table([
      ["escape", [0x01000000, "\u001b", 0], "Esc"],
      ["tab", [0x01000001, "\t", 0], "Tab"],
      ["shift_tab", [0x01000001, "", SHIFT], "Shift+Tab"],
      ["backtab", [0x01000002, "", SHIFT], "Shift+Tab"],
      ["return", [0x01000004, "\r", 0], "Enter"],
      ["keypad_enter", [0x01000005, "\r", 0], "Enter"],
      ["space", [0x20, " ", 0], "Space"],
      ["down", [0x01000015, "", 0], "Down"],
      ["page_down", [0x01000017, "", 0], "PgDn"],
      ["ctrl_z_as_qt_types_it", [0x5a, "\u001a", CTRL], "Ctrl+Z"],
      ["ctrl_z_as_qtest_types_it", [0x5a, "z", CTRL], "Ctrl+Z"],
      ["ctrl_equals", [0x3d, "=", CTRL], "Ctrl+="],
      ["ctrl_minus", [0x2d, "-", CTRL], "Ctrl+-"],
      ["ctrl_0", [0x30, "0", CTRL], "Ctrl+0"],
      ["a_letter", [0x55, "u", 0], "u"],
      ["a_capital", [0x55, "U", SHIFT], "U"],
      ["question_mark", [0x3f, "?", SHIFT], "?"],
      ["a_control_character", [0, "\u0007", 0], ""],
      ["no_modifiers_given", [0x55, "u", undefined], "u"],
      ["nothing", [0x01000020, "", 0], ""],
    ], ([code, text, mods]) => M.keyName(code, text, mods))
  })

  test("buttons say their key, from the table", () => {
    expect(M.keyHint("check")).toBe("c")
    expect(M.keyHint("undo")).toBe("Ctrl+Z")
    expect(M.keyHint("enter", "issues")).toBe("Enter")
    expect(M.keyHint("search", "field")).toBe("Enter")
    expect(M.keyHint("search", "view")).toBe("s")
    expect(M.keyHint("zoom")).toBe("z")
    expect(M.keyHint("settings")).toBe("", "no key before its action exists (.31)")
    expect(M.withKey("Check now", "check")).toBe("Check now (c)")
    expect(M.withKey("Undo update on era", "undo")).toBe("Undo update on era (Ctrl+Z)")
    expect(M.withKey("Nothing", "no_such_action")).toBe("Nothing")
    expect(M.keyTip("dismiss", "issues")).toBe("Mark the new Lua errors shown as seen (d)")
    expect(M.keyTip("update_all", "updates")).toBe("Update everything on the chosen client, or on every client (U)")
    expect(M.keyTip("no_such_action")).toBe("")
  })

  test("the reference lists where the panel stands first, then the frame, then a dialog", () => {
    const titles = (context) => M.helpSections(context).map((s) => s.title)
    expect(titles("updates")).toEqual(["Updates", "Every view", "Anywhere", "Dialog"])
    // A view with a text box lists the box's keys right after its own.
    expect(titles("search")).toEqual(["Search", "Search box", "Every view", "Anywhere", "Dialog"])
    expect(titles("installed")).toEqual(["Addons", "Filter box", "Every view", "Anywhere", "Dialog"])
    expect(titles("card")).toEqual(["Problem card", "Anywhere", "Dialog"])
    const updates = M.helpSections("updates")[0]
    expect(updates.rows.map((r) => r.hint)).toEqual(["Enter", "u", "p", "e", "/"])
    expect(updates.rows[0]).toEqual({ hint: "Enter", label: "open the addon's page" })
    // Every row of those contexts, once.
    const listed = M.helpSections("installed").flatMap((s) => s.rows).length
    expect(listed).toBe(M.KEYMAP.filter((r) => ["installed", "filter", "view", "panel", "sheet"].includes(r.context)).length)
  })
})

describe("the panel frame", () => {

  test("views step and wrap", () => {
    expect(M.stepView("updates", 1)).toBe("installed")
    expect(M.stepView("updates", -1)).toBe("history")
    expect(M.stepView("backups", 1)).toBe("history")
    expect(M.stepView("history", 1)).toBe("updates")
    expect(M.stepView("installed", 1)).toBe("search")
    expect(M.stepView("nonsense", 1)).toBe("installed")
  })

  test("the hero speaks the scope, urgent first", () => {
    const model = M.reduceStatus(fixture("status-updates"), NOW)
    expect(model.targets.map((t) => [t.id, t.displayName, t.version, t.family])).toEqual([
      ["era", "Classic Era", "1.15.9.64520", "vanilla"], ["retail", "Retail", "12.1.5.70391", "mainline"]])
    expect(M.heroTitle(model.targets, "")).toBe("All clients")
    expect(M.heroTitle(model.targets, "era")).toBe("Classic Era")
    expect(M.heroTitle(model.targets, "gone")).toBe("gone")
    expect(M.heroMeta(model.summary, model.targets, "", [])).toBe("4 UPDATES")
    expect(M.heroMeta(model.summary, model.targets, "era", [])).toBe("2 UPDATES")
    expect(M.heroMeta(null, model.targets, "", [])).toBe("")
    expect(M.heroDetail(model.summary, model.targets, "")).toBe("2 clients")
    expect(M.heroDetail(model.summary, model.targets, "retail")).toBe("12.1.5.70391 · mainline")
    expect(M.heroDetail(null, model.targets, "")).toBe("")
    expect(M.heroState(model.summary, model.targets, "", [], false)).toBe("quiet")
    expect(M.heroState(model.summary, model.targets, "", [], true)).toBe("busy")
    // Running: by name under All, a word under the client.
    const running = fixture("status-running")
    const at = Date.parse(running.data.targets[0].updates.checked_at) + 25 * 60000
    const r = M.reduceStatus(running, at)
    expect(M.heroMeta(r.summary, r.targets, "", [])).toBe("4 UPDATES · RETAIL RUNNING")
    expect(M.heroMeta(r.summary, r.targets, "retail", [])).toBe("2 UPDATES · RUNNING")
    expect(M.heroMeta(r.summary, r.targets, "era", [])).toBe("2 UPDATES")
    const unknown = JSON.parse(JSON.stringify(running))
    unknown.data.summary.running = []
    for (const t of unknown.data.targets) t.running = null
    const u = M.reduceStatus(unknown, at)
    expect(M.heroMeta(u.summary, u.targets, "", [])).toBe("4 UPDATES · CAN'T TELL IF THE GAME IS RUNNING")
    // A P0 or P1 problem goes first, and makes the glyph urgent; new errors before it.
    const a = M.reduceStatus(fixture("status-attention"), NOW)
    expect(M.heroMeta(a.summary, a.targets, "", [])).toBe("1 PROBLEM")
    expect(M.heroState(a.summary, a.targets, "", [], true)).toBe("urgent")
    const erroring = JSON.parse(JSON.stringify(model.targets))
    erroring[1].errors = { "new": 2 }
    erroring[1].issues = [{ "urgent": false }]
    expect(M.heroMeta(model.summary, erroring, "", [])).toBe("2 NEW ERRORS · 4 UPDATES · 1 PROBLEM")
    expect(M.heroMeta(model.summary, erroring, "", [{ "urgent": true }])).toBe("2 NEW ERRORS · 2 PROBLEMS · 4 UPDATES")
    expect(M.heroMeta(model.summary, erroring, "era", [{ "urgent": true }])).toBe("2 UPDATES",
      "the machine's problems are All's")
    expect(M.heroState(model.summary, erroring, "retail", [], false)).toBe("urgent")
    expect(M.heroState(model.summary, erroring, "era", [], false)).toBe("quiet")
    // Nothing to say: all good.
    const quiet = model.targets.map((t) => Object.assign({}, t, { "updates": null }))
    expect(M.heroMeta(model.summary, quiet, "", [])).toBe("ALL GOOD")
    // A client that is gone.
    expect(M.heroDetail(model.summary, [{ "id": "x", "missing": true }], "x")).toBe("not on this computer any more")
  })

  test("the freshness footer says how many clients, when checked, and who runs", () => {
    const running = fixture("status-running")
    const at = Date.parse(running.data.targets[0].updates.checked_at) + 25 * 60000
    const r = M.reduceStatus(running, at)
    expect(M.footerText(r.summary, r.targets, "", at)).toBe("2 clients · checked 25 min ago · Retail running")
    expect(M.footerText(r.summary, r.targets, "retail", at)).toMatch(/^checked (just now|\d+ min ago) · running$/)
    expect(M.footerText(r.summary, r.targets, "era", at)).toBe("checked 25 min ago")
    const never = r.targets.map((t) => Object.assign({}, t, { "updates": null, "running": false }))
    expect(M.footerText(r.summary, never, "", at)).toBe("2 clients · never checked")
    expect(M.footerText(null, r.targets, "", at)).toBe("")
  })

  test("the scope's options name each client, its counts and its state", () => {
    const r = M.reduceStatus(fixture("status-running"), NOW)
    expect(M.scopeOptions(r.targets)).toEqual([
      { value: "", label: "All clients · 4 updates" },
      { value: "era", label: "Classic Era · 2 updates" },
      { value: "retail", label: "Retail · 2 updates · running" }])
    const odd = [
      { "id": "gone", "displayName": "Gone", "missing": true, "updates": { "available": 3 }, "running": false },
      { "id": "x", "displayName": "X", "runningUnknown": true, "errors": { "new": 1 } }]
    // A missing client comes last, whatever its place in `status`'s order.
    expect(M.scopeOptions(odd)).toEqual([
      { value: "", label: "All clients · 3 updates · 1 new error" },
      { value: "x", label: "X · 1 new error · can't tell if running" },
      { value: "gone", label: "Gone · not found" }])
    expect(M.scopeOptions(null)).toEqual([{ value: "", label: "All clients" }])
  })

  test("the tabs count what each view has in the scope", () => {
    const model = M.reduceStatus(fixture("status-updates"), NOW)
    const commands = [{ "action": "backup_create", "argv": ["omaddons", "backup", "create", "-t", "era"] },
      { "action": "update", "argv": ["omaddons", "update"] }]
    const all = M.viewCounts(model.targets, "", commands, [])
    expect(all.updates).toEqual({ count: 4, urgent: false })
    expect(all.backups).toEqual({ count: 1, urgent: false })
    expect(all.issues).toEqual({ count: 0, urgent: false })
    expect(all.installed.count + all.search.count).toBe(0)
    expect(M.viewCounts(model.targets, "retail", commands, []).backups.count).toBe(0)
    expect(M.viewCounts(model.targets, "era", commands, []).updates.count).toBe(2)
    const a = M.reduceStatus(fixture("status-attention"), NOW)
    expect(M.viewCounts(a.targets, "", a.commands, []).issues).toEqual({ count: 1, urgent: true })
    // New Lua errors count in Errors, always urgent; Issues counts problems only (.27).
    const erroring = JSON.parse(JSON.stringify(model.targets))
    erroring[0].errors = { "new": 3 }
    expect(M.viewCounts(erroring, "", [], []).errors).toEqual({ count: 3, urgent: true })
    expect(M.viewCounts(erroring, "", [], []).issues).toEqual({ count: 0, urgent: false })
    expect(M.viewCounts(erroring, "", [], [{ "urgent": false }]).issues).toEqual({ count: 1, urgent: false })
    // Every view has an entry.
    for (const v of M.VIEWS) expect([v, typeof all[v].count]).toEqual([v, "number"])
  })

  test("the filter cycles All, each target, All, both ways", () => {
    const targets = M.reduceStatus(fixture("status-updates"), NOW).targets
    expect(M.nextFilter(targets, "")).toBe("era")
    expect(M.nextFilter(targets, "era")).toBe("retail")
    expect(M.nextFilter(targets, "retail")).toBe("")
    expect(M.nextFilter(targets, "gone")).toBe("")
    expect(M.nextFilter([], "")).toBe("")
    expect(M.stepFilter(targets, "", -1)).toBe("retail")
    expect(M.stepFilter(targets, "retail", -1)).toBe("era")
    expect(M.stepFilter(targets, "era", -1)).toBe("")
    expect(M.stepFilter(targets, "gone", -1)).toBe("")
    expect(M.stepFilter(null, "", -1)).toBe("")
    expect(M.targetById(targets, "retail").id).toBe("retail")
    expect(M.targetById(targets, "")).toBeNull()
  })

  // The development machine's six clients (status-clients, list-clients): Mists Classic
  // runs, Retail and Classic Era each have an update, Anniversary is uninstalled.
  describe("the client switcher over six clients (1n2p.20)", () => {
    const CLIENTS = () => M.reduceStatus(fixture("status-clients"), NOW).targets
    const LISTS = () => M.reduceList(fixture("list-clients"))

    test("its options: All, the clients here by name with their counts and state, a missing one last", () => {
      expect(M.scopeOptions(CLIENTS())).toEqual([
        { value: "", label: "All clients · 2 updates" },
        { value: "classic", label: "Mists of Pandaria Classic · running" },
        { value: "era", label: "Classic Era · 1 update" },
        { value: "forever", label: "WoW: Forever" },
        { value: "retail", label: "Retail · 1 update" },
        { value: "wotlk", label: "Wrath 3.3.5a private server" },
        { value: "anniversary", label: "Burning Crusade Anniversary · not found" }])
    })

    test("t and T step through the clients here, never the missing one", () => {
      const targets = CLIENTS()
      const ring = [""]
      for (let at = M.stepFilter(targets, "", 1); at !== ""; at = M.stepFilter(targets, at, 1)) ring.push(at)
      expect(ring).toEqual(["", "classic", "era", "forever", "retail", "wotlk"])
      expect(M.stepFilter(targets, "", -1)).toBe("wotlk")
      // From the missing client (chosen in the Dropdown), t goes back to All.
      expect(M.stepFilter(targets, "anniversary", 1)).toBe("")
      expect(M.stepFilter(targets, "anniversary", -1)).toBe("")
    })

    test("the clients counted are the ones on this computer", () => {
      const r = M.reduceStatus(fixture("status-clients"), NOW)
      expect(M.liveTargets(r.targets).length).toBe(5)
      expect(M.heroDetail(r.summary, r.targets, "")).toBe("5 clients")
      expect(M.footerText(r.summary, r.targets, "", NOW)).toMatch(/^5 clients · /)
      expect(M.liveTargets(null)).toEqual([])
    })

    test("the panel opens on the one running client, else the last scope still here, else All", () => {
      const targets = CLIENTS()
      expect(M.openingScope(targets, "")).toBe("classic")
      expect(M.openingScope(targets, "era")).toBe("classic")
      const idle = targets.map((t) => Object.assign({}, t, { running: false }))
      expect(M.openingScope(idle, "era")).toBe("era")
      expect(M.openingScope(idle, "")).toBe("")
      // A missing client, or one no longer listed, is never a default scope.
      expect(M.openingScope(idle, "anniversary")).toBe("")
      expect(M.openingScope(idle, "gone")).toBe("")
      // Two running: neither is the one the player is about to care about.
      const two = targets.map((t) => Object.assign({}, t, { running: t.id === "classic" || t.id === "retail" }))
      expect(M.openingScope(two, "era")).toBe("era")
      expect(M.openingScope(null, "era")).toBe("")
    })

    test("a scope whose client is no longer listed falls back to All", () => {
      const targets = CLIENTS()
      expect(M.keptScope(targets, "retail")).toBe("retail")
      expect(M.keptScope(targets, "anniversary")).toBe("anniversary")
      expect(M.keptScope(targets, "gone")).toBe("")
      expect(M.keptScope(targets, "")).toBe("")
    })

    test("under All, each client's rows sit under its name and counts", () => {
      const targets = CLIENTS()
      const headers = (rows) => rows.filter((r) => r.group).map((r) => [r.group, r.groupCount, r.target])
      expect(headers(M.updateRows(LISTS(), "", targets)))
        .toEqual([["Classic Era", "1 update", "era"], ["Retail", "1 update", "retail"]])
      expect(headers(M.installedRows(LISTS(), "", targets)))
        .toEqual([["Classic Era", "1 addon", "era"], ["Retail", "1 addon", "retail"]])
      // Under one client, no header.
      expect(headers(M.installedRows(LISTS(), "retail", targets))).toEqual([])
      // A running client says so; one nobody can tell about says that.
      expect(M.clientGroup(targets, "classic", "")).toEqual({ title: "Mists of Pandaria Classic", count: "running" })
      const unknown = targets.map((t) => Object.assign({}, t, { running: false, runningUnknown: true }))
      expect(M.clientGroup(unknown, "era", "1 update")).toEqual({ title: "Classic Era", count: "can't tell if running · 1 update" })
      expect(M.clientGroup([], "era", "")).toEqual({ title: "era", count: "" })
    })

    test("a missing client's scope is one card: Rescan clients, and Forget through its own targets rm", () => {
      const targets = CLIENTS()
      const gone = M.targetById(targets, "anniversary")
      expect(M.forgetAction(gone).argv).toEqual(["omaddons", "targets", "rm", "anniversary"])
      expect(M.forgetAction(M.targetById(targets, "retail"))).toBeNull()
      expect(M.forgetAction(null)).toBeNull()
      // A purge is never offered here: the classifier refuses it, so no Forget.
      const purge = JSON.parse(JSON.stringify(gone))
      purge.issues[0].remediation = M.cliAction("omaddons targets rm anniversary --purge", ["omaddons", "targets", "rm", "anniversary", "--purge"], true)
      expect(M.forgetAction(purge)).toBeNull()
      for (const view of M.VIEWS) {
        const state = M.viewState(view, { service: { targets, lists: LISTS() }, filter: "anniversary", rows: 0, allRows: 3, query: "", nowMs: NOW })
        expect([view, state.state, state.sentence]).toEqual([view, "missing", ""])
        expect(state.notices.map((n) => [n.sentence, n.body, n.actions.map((a) => [a.name, a.text])])).toEqual([[
          "Burning Crusade Anniversary isn't on this computer any more", "Its folder is gone, or the game was uninstalled.",
          [["rescan", "Rescan clients"], ["forget", "Forget Burning Crusade Anniversary…"]]]])
      }
      // Without its problem's command, Rescan alone.
      const quiet = targets.map((t) => (t.id === "anniversary" ? Object.assign({}, t, { issues: [] }) : t))
      const state = M.viewState("updates", { service: { targets: quiet }, filter: "anniversary", rows: 0, nowMs: NOW })
      expect(state.notices[0].actions.map((a) => a.name)).toEqual(["rescan"])
    })

    test("o opens the scoped client's AddOns, under All the cursored row's, never a guess or a missing one", () => {
      const targets = CLIENTS()
      expect(M.openTarget(targets, "retail", "").id).toBe("retail")
      expect(M.openTarget(targets, "retail", "era").id).toBe("retail")
      expect(M.openTarget(targets, "", "era").id).toBe("era")
      expect(M.openTarget(targets, "", "")).toBeNull()
      expect(M.openTarget(targets, "anniversary", "")).toBeNull()
      expect(M.openTarget(targets, "", "nosuch")).toBeNull()
    })

    test("the Backups and Issues sections name each client as status does", () => {
      const targets = CLIENTS()
      const sections = M.backupSections(null, "", NOW, targets)
      expect(sections.map((s) => [s.target, s.title, s.count])).toEqual([
        ["classic", "Mists of Pandaria Classic", "running"], ["era", "Classic Era", ""], ["forever", "WoW: Forever", ""],
        ["retail", "Retail", ""], ["wotlk", "Wrath 3.3.5a private server", ""]])
      const problems = M.issueRows(M.issueSections(targets, [], "", []))
      expect(problems.map((r) => [r.kind, r.target, r.client])).toEqual([["problem", "anniversary", "Burning Crusade Anniversary"]])
    })
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
    // The label names the client as `status` does.
    const targets = M.reduceStatus(fixture("status-updates"), NOW).targets
    expect(M.updateRequest("era", undefined, targets).label).toBe("Update Classic Era")
    expect(M.backupRequest("era", targets).label).toBe("Back up Classic Era")
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
    // A last change the cached status cannot tell (never a guessed run): Undo opens
    // History, where every change has its own (1n2p.30); "last change: unknown" is gone.
    expect(M.undoAction({ unknown: true })).toEqual({ label: "Undo: choose the change in History", argv: null, why: "", history: true })
    expect(JSON.stringify(M.COPY) + JSON.stringify(M.undoAction({ unknown: true }))).not.toContain("last change: unknown")
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
    // Each client's header is its player name and its updates, on its first row only.
    expect(all.map((r) => [r.group, r.groupCount, r.target, r.name, r.kind, r.version, r.latest]))
      .toEqual([["Classic Era", "2 updates", "era", "Bar", "update", "v1", "v2"], ["", "", "era", "Foo", "update", "v1", "v2"],
        ["Retail", "2 updates", "retail", "Bar", "update", "v1", "v2"], ["", "", "retail", "Foo", "update", "v1", "v2"]])
    expect(M.updateCount(all)).toBe(4)
    // A running client says so in its header.
    const running = M.reduceStatus(fixture("status-running"), NOW).targets
    expect(M.updateRows(lists, "", running).filter((r) => r.group).map((r) => [r.group, r.groupCount]))
      .toEqual([["Classic Era", "2 updates"], ["Retail", "running · 2 updates"]])
    // Without `status`'s clients, the header falls back to the id.
    expect(M.updateRows(lists, "", [])[0].group).toBe("era")
    const retail = M.updateRows(lists, "retail", STATUS())
    expect(retail.map((r) => [r.group, r.name])).toEqual([["", "Bar"], ["", "Foo"]])
    expect(M.updateCount(retail)).toBe(2)
    expect(M.updateRows(null, "", [])).toEqual([])
  })

  // The foreground and background of six Omarchy themes (their colors.toml, 2026-11-08),
  // light and dark: the dim colour must dim on both and stay at 4.5:1 (§5.1, UX-44).
  const THEMES = {
    "rose-pine": ["#575279", "#faf4ed"], "catppuccin-latte": ["#4c4f69", "#eff1f5"], "white": ["#000000", "#ffffff"],
    "tokyo-night": ["#a9b1d6", "#1a1b26"], "everforest": ["#d3c6aa", "#2d353b"], "vantablack": ["#ffffff", "#000000"],
  }

  test("the dim colour reaches 4.5:1 and dims on light themes too", () => {
    const alphas = {}
    for (const [name, [fg, bg]] of Object.entries(THEMES)) {
      const dim = M.dimColor(fg, bg)
      expect(M.contrastRatio(dim, bg), name).toBeGreaterThanOrEqual(M.DIM_MIN_CONTRAST)
      expect(M.contrastRatio(dim, bg), name + ": dimmer than the foreground").toBeLessThan(M.contrastRatio(fg, bg))
      alphas[name] = M.dimAlpha(fg, bg)
    }
    // The values the design doc's §5.1 table quotes.
    expect(alphas).toEqual({ "rose-pine": 0.84, "catppuccin-latte": 0.82, "white": 0.55, "tokyo-night": 0.69,
      "everforest": 0.71, "vantablack": 0.55 })
  })

  test("colours read from QML values, objects and hex strings; contrast is WCAG's", () => {
    expect(M.rgbOf("#ff0000")).toEqual({ r: 1, g: 0, b: 0 })
    expect(M.rgbOf("#80ff0000")).toEqual({ r: 1, g: 0, b: 0 })
    expect(M.rgbOf({ r: 0.5, g: 0.5, b: 0.5, a: 1 })).toEqual({ r: 0.5, g: 0.5, b: 0.5 })
    expect(M.rgbOf("red")).toBeNull()
    expect(M.contrastRatio("#000000", "#ffffff")).toBeCloseTo(21, 5)
    expect(M.contrastRatio("#777777", "#777777")).toBeCloseTo(1, 5)
    expect(M.contrastRatio("nope", "#ffffff")).toBe(1)
    expect(M.mixColor("#ffffff", "#000000", 0.5)).toBe("#808080")
    // A foreground that cannot reach 4.5:1 is used as it is.
    expect(M.dimAlpha("#777777", "#808080")).toBe(1)
  })

  test("stableSort keeps ties in their input order, whatever the engine's sort", () => {
    const items = [["b", 1], ["a", 1], ["c", 0], ["d", 1], ["e", 0]]
    const byRank = (x, y) => x[1] - y[1]
    // The adversary is real: a plain sort under it reverses ties.
    expect(withTiesReversed(() => items.slice().sort(byRank)).map((i) => i[0])).toEqual(["e", "c", "d", "a", "b"])
    expect(withTiesReversed(() => M.stableSort(items, byRank)).map((i) => i[0])).toEqual(["c", "e", "b", "a", "d"])
    expect(items.map((i) => i[0])).toEqual(["b", "a", "c", "d", "e"])
    expect(M.stableSort([], byRank)).toEqual([])
  })

  test("updates keep list -v's order within a kind, whatever the engine's sort", () => {
    const env = JSON.parse(JSON.stringify(fixture("list")))
    const template = env.data.targets[1].addons[0]
    const names = []
    env.data.targets[1].addons = []
    for (let i = 0; i < 40; i++) {
      const name = "Addon" + String(i + 1).padStart(3, "0")
      names.push(name)
      env.data.targets[1].addons.push(Object.assign(JSON.parse(JSON.stringify(template)), {
        name, version: "v1", latest: { version: "v2", status: i % 5 === 3 ? "blocked" : "update_available" } }))
    }
    const rows = withTiesReversed(() => M.updateRows(M.reduceList(env), "retail", STATUS()))
    const blocked = names.filter((n, i) => i % 5 === 3)
    expect(rows.map((r) => r.name)).toEqual(names.filter((n) => blocked.indexOf(n) === -1).concat(blocked))
  })

  test("pinned with a newer release, blocked and failed follow, each said in words", () => {
    const env = JSON.parse(JSON.stringify(fixture("list-markers")))
    const addons = env.data.targets[0].addons
    addons.push(Object.assign({}, addons[0], { name: "Zed", latest: { version: "v9", status: "blocked" } }))
    addons.push(Object.assign({}, addons[0], { name: "Aaa", latest: { version: null, status: "failed" } }))
    addons.push(Object.assign({}, addons[0], { name: "Old", pinned: true, latest: { version: "v1", status: "pinned" } }))
    const running = JSON.parse(JSON.stringify(fixture("status-running")))
    const rows = M.updateRows(M.reduceList(env), "retail", M.reduceStatus(running, NOW).targets)
    expect(rows.map((r) => [r.name, r.kind, r.meta])).toEqual([
      ["Bar", "update", "Retail is running"], ["Foo", "update", "Retail is running"],
      ["Baz", "pinned", "pinned at " + rows[2].version + " · " + rows[2].latest + " available"],
      ["Zed", "blocked", "v9 is out, but it doesn't load on Retail"],
      ["Aaa", "failed", "Couldn't check GitHub for a new release"]])
    expect(rows.every((r) => r.reload)).toBe(true)
    // A glyph and a band per kind; a failed check offers Check again, never Update.
    expect(rows.map((r) => [r.kind, r.band, r.glyph > 0])).toEqual([["update", "accent", true], ["update", "accent", true],
      ["pinned", "", true], ["blocked", "", true], ["failed", "urgent", true]])
    expect(rows[0].actions.map((a) => a.name)).toEqual(["update", "notes", "pin"])
    expect(rows[4].actions.map((a) => a.name)).toEqual(["check"])
    expect(rows[4].actions[0].tooltip).toBe("Check again (c)")
    expect([rows[0].fromVersion, rows[0].toVersion, rows[2].toVersion]).toEqual([rows[0].version, rows[0].latest, ""])
  })

  test("updates are ordered by kind, then by name; a filter keeps its matches", () => {
    const env = JSON.parse(JSON.stringify(fixture("list-markers")))
    const addons = env.data.targets[0].addons
    addons.push(Object.assign({}, addons[0], { name: "aardvark", latest: { version: "v9", status: "update_available" } }))
    addons.push(Object.assign({}, addons[0], { name: "Zulu", latest: { version: "v9", status: "update_available" } }))
    const rows = M.updateRows(M.reduceList(env), "retail", [])
    const updates = rows.filter((r) => r.kind === "update").map((r) => r.name)
    expect(updates).toEqual([...updates].sort((a, b) => a.toLowerCase() < b.toLowerCase() ? -1 : 1))
    expect(updates[0]).toBe("aardvark")
    const matched = M.updateRows(M.reduceList(env), "retail", [], { query: "zlu" })
    expect(matched.map((r) => r.name)).toEqual(["Zulu"])
    expect(M.updateRows(M.reduceList(env), "retail", [], { query: "nothing like it" })).toEqual([])
  })

  // The first addon of a `list` envelope's first client with an update waiting.
  const firstWaiting = (env) => env.data.targets[0].addons.find((a) => !!a.latest && ["update_available"].includes(a.latest.status))

  test("a release's age, from its date; none without one", () => {
    const at = Date.parse("2026-11-08T12:00:00Z")
    const age = (iso) => M.releaseAge(iso, at)
    expect([age("2026-11-08T09:00:00Z"), age("2026-11-07T11:00:00Z"), age("2026-11-01T12:00:00Z"), age("2026-07-01T00:00:00Z"),
      age("2023-01-01T00:00:00Z"), age(null), age("nonsense")])
      .toEqual(["out today", "1 day old", "7 days old", "4 months old", "3 years old", "", ""])
    const env = JSON.parse(JSON.stringify(fixture("list-markers")))
    const bar = firstWaiting(env)
    bar.latest.published_at = "2026-11-06T12:00:00Z"
    const rows = M.updateRows(M.reduceList(env), "retail", [], { nowMs: at })
    expect(rows.find((r) => r.name === bar.name).meta).toBe("2 days old")
  })

  test("a row a change just updated stays a moment, said so, in its update's place", () => {
    const env = JSON.parse(JSON.stringify(fixture("list-markers")))
    const bar = firstWaiting(env)
    bar.latest = { version: bar.version, status: "current" }
    const key = "retail/" + bar.name
    expect(M.updateRows(M.reduceList(env), "retail", []).some((r) => r.key === key)).toBe(false)
    const done = M.updateRows(M.reduceList(env), "retail", [], { done: { [key]: "Updated" } })
    const row = done.find((r) => r.key === key)
    expect([row.kind, row.meta, row.fromVersion, row.toVersion, row.actions]).toEqual(["done", "Updated", "", bar.version, []])
    expect(M.updateCount(done)).toBe(M.updateCount(M.updateRows(M.reduceList(env), "retail", [])))
  })

  test("a client header updates that client only, by its player name", () => {
    const lists = M.reduceList(fixture("list"))
    const targets = M.reduceStatus(fixture("status-updates"), NOW).targets
    const rows = M.updateRows(lists, "", targets)
    const header = rows.find((r) => r.groupTarget === "retail")
    expect([header.group, header.groupUpdates > 0]).toEqual(["Retail", true])
    const request = M.groupUpdateRequest("retail", rows, targets)
    expect([request.template, request.slots, request.label]).toEqual(["update_target", { target: "retail" }, "Update Retail"])
    expect(request.shown.every((s) => s.target_id === "retail")).toBe(true)
    expect(request.shown.length).toBe(header.groupUpdates)
  })

  test("every pending release note, how far behind, and a safe link only", () => {
    const row = { target: "retail", name: "Foo" }
    const at = Date.parse("2026-11-08T12:00:00Z")
    const data = { changelog: { source: "github", truncated: false, more_url: "https://github.com/o/foo/releases",
      entries: [{ version: "v3", published_at: "2026-11-07T12:00:00Z", text: "Third" }, { version: "v2", published_at: null, text: "" }] } }
    const notes = M.releaseNotes({ selection: { target: "retail", addon: "Foo" }, data }, row, at)
    expect([notes.state, notes.behind, notes.link]).toEqual(["ready", "2 releases behind", "More on GitHub"])
    expect(notes.entries).toEqual([{ version: "v3", date: "1 day old", text: "Third" }, { version: "v2", date: "", text: "No release notes" }])
    expect(M.releaseNotes({ selection: { target: "retail", addon: "Bar" }, data }, row, at).state).toBe("reading")
    expect(M.releaseNotes(null, row, at).state).toBe("reading")
    const failed = M.releaseNotes({ selection: { target: "retail", addon: "Foo" }, data: null, failed: "timeout" }, row, at)
    expect([failed.state, failed.failed]).toEqual(["failed", "The release notes could not be read: timeout"])
    const cut = M.releaseNotes({ selection: { target: "retail", addon: "Foo" },
      data: { changelog: { truncated: true, more_url: "http://evil.example/x", entries: data.changelog.entries } } }, row, at)
    expect([cut.behind, cut.url, cut.link]).toEqual(["2+ releases behind", "", ""])
  })

  test("nothing to update says when it was checked", () => {
    const targets = M.reduceStatus(fixture("status-running"), NOW).targets
    const oldest = M.oldestCheck(targets, "")
    const at = Date.parse(oldest) + 25 * 60000
    const state = M.viewState("updates", { service: { lists: M.reduceList(fixture("list")), targets: targets }, rows: 0, allRows: 0, nowMs: at })
    expect([state.state, state.sentence]).toEqual(["empty", "All up to date. Checked 25 min ago."])
    expect(M.oldestCheck(targets, "nosuch")).toBeNull()
  })

  test("installed rows carry their markers and Enter's action", () => {
    const env = JSON.parse(JSON.stringify(fixture("list-markers")))
    env.data.targets[0].addons[1].reason = "dependency"
    env.data.targets[0].addons[1].orphan = true
    const rows = M.installedRows(M.reduceList(env), "")
    // Problems first (its folder gone, its files changed), then the rest.
    expect(rows.map((r) => [r.group, r.name, r.source, r.markers, r.primary])).toEqual([
      ["retail", "Bar", "gh", ["missing"], "reinstall"],
      ["", "Foo", "gh", ["modified"], "reinstall"],
      ["", "Baz", "gh", ["dep", "orphan", "pinned"], ""]])
    // The state in words: chips at a glance, the most urgent first; the meta line says
    // its source and what the chips cannot (design doc §2.3, §6.3).
    expect(rows.map((r) => [r.chips.map((c) => (c.urgent ? "!" : "") + c.text), r.meta, r.band])).toEqual([
      [["!folder gone", "update ready"], "GitHub · its folder is gone", "urgent"],
      [["!changed on disk", "update ready"], "GitHub · 1 file changed on disk", "urgent"],
      [["pinned", "not needed"], "GitHub · nothing needs it any more", ""]])
    // A problem's first button repairs; every row pins, restores and removes.
    expect(rows[0].actions.map((a) => a.name)).toEqual(["repair", "update", "pin", "restore", "remove"])
    // Enter opens the addon's page (.23), so Repair is a button of its own.
    expect(rows[0].actions[0].tooltip).toBe("Repair")
    expect(rows[2].actions.map((a) => [a.name, a.tooltip])).toEqual([["pin", "Unpin (p)"],
      ["restore", "Restore its settings (r)"], ["remove", "Remove (x)"]])
    const plain = M.installedRows(M.reduceList(fixture("list")), "retail")
    // Waiting updates before the rest.
    expect(plain.map((r) => [r.name, r.primary, r.band])).toEqual([["Bar", "update", "accent"], ["Foo", "update", "accent"], ["Baz", "", ""]])
    // No command line: the folders omaddons does not manage, in words.
    const loose = M.unmanagedRows(M.reduceList(fixture("list")), "", M.reduceStatus(fixture("status-updates"), NOW).targets)
    expect(loose).toEqual([{ target: "retail", client: "Retail", count: 1, text: "1 folder on Retail omaddons doesn't manage" }])
    expect(M.unmanagedRows(M.reduceList(fixture("list")), "retail", [])[0].text).toBe("1 folder omaddons doesn't manage")
    expect(M.unmanagedRows(M.reduceList(fixture("list")), "era", [])).toEqual([])
  })

  // The Addons view's order, nesting, filter and sort (1n2p.22), over a list the binary
  // wrote, edited where a scene needs it: LibA needed by WeakAuras and by Zed, LibB by
  // nothing here, Old installed first.
  test("installed rows nest each dependency under the first addon that needs it", () => {
    const env = JSON.parse(JSON.stringify(fixture("list-markers")))
    const template = env.data.targets[0].addons[1]
    const addon = (name, extra) => Object.assign(JSON.parse(JSON.stringify(template)), { name, pinned: false,
      reason: "explicit", orphan: false, required_by: [], latest: { version: null, status: "current", source: null, release_id: null, asset: null },
      ref: "gh:o/" + name.toLowerCase(), installed_at: "2026-11-01T00:00:00Z" }, extra || {})
    env.data.targets[0].addons = [
      addon("LibA", { reason: "dependency", required_by: ["WeakAuras", "Zed"] }),
      addon("LibB", { reason: "dependency", required_by: ["Gone"] }),
      addon("Old", { installed_at: "2026-10-01T00:00:00Z", ref: "wowi:1" }),
      addon("WeakAuras", { installed_at: "2026-11-05T00:00:00Z" }),
      addon("Zed", { installed_at: "2026-11-03T00:00:00Z", ref: "tukui:zed" })]
    const lists = M.reduceList(env)
    const shape = (rows) => rows.map((r) => (r.depth ? "  " : "") + r.name)
    expect(shape(M.installedRows(lists, "retail"))).toEqual(["LibB", "Old", "WeakAuras", "  LibA", "Zed"])
    expect(M.installedRows(lists, "retail").find((r) => r.name === "LibA").meta).toBe("GitHub · needed by WeakAuras and Zed")
    // Recently updated: the newest install first; by source: GitHub, Tukui, WoWInterface.
    expect(shape(M.installedRows(lists, "retail", [], { sort: "recent" }))).toEqual(["WeakAuras", "  LibA", "Zed", "LibB", "Old"])
    expect(shape(M.installedRows(lists, "retail", [], { sort: "source" }))).toEqual(["LibB", "WeakAuras", "  LibA", "Zed", "Old"])
    expect(M.INSTALLED_SORTS.map(M.nextSort)).toEqual(["recent", "source", "name"])
    expect(M.nextSort("nonsense")).toBe("name")
    // A filter keeps what it matches; a dependency whose requester it drops stands alone.
    expect(shape(M.installedRows(lists, "retail", [], { query: "lba" }))).toEqual(["LibA"])
    // "weak" also matches what WeakAuras needs ("needed by WeakAuras and Zed"): it stays
    // under it.
    expect(shape(M.installedRows(lists, "retail", [], { query: "weak" }))).toEqual(["WeakAuras", "  LibA"])
    expect(shape(M.installedRows(lists, "retail", [], { query: "needed weak" }))).toEqual(["LibA"])
    expect(shape(M.installedRows(lists, "retail", [], { query: "tukui" }))).toEqual(["Zed"])
    expect(shape(M.installedRows(lists, "retail", [], { query: "nothing like this" }))).toEqual([])
    expect(M.installedCount(lists, "retail", "lib")).toEqual({ shown: 2, total: 5, text: "2 of 5", none: false })
    expect(M.installedCount(lists, "retail", "zzz")).toEqual({ shown: 0, total: 5, text: "0 of 5", none: true })
    expect(M.installedCount(lists, "retail", "")).toEqual({ shown: 5, total: 5, text: "5 of 5", none: false })
  })

  test("a filter word is a subsequence of one field, ignoring case", () => {
    expect(M.filterMatches("plt", ["Plater"])).toBe(true)
    expect(M.filterMatches("PLT", ["plater"])).toBe(true)
    expect(M.filterMatches("chg disk", ["Foo", "changed on disk"])).toBe(true)
    // Each word in one field: never stitched across two.
    expect(M.filterMatches("pg", ["Plater", "GitHub"])).toBe(false)
    expect(M.filterMatches("tlp", ["Plater"])).toBe(false)
    expect(M.filterMatches("", ["x"])).toBe(true)
    expect(M.filterMatches("  ", [])).toBe(true)
    expect(M.filterMatches("x", null)).toBe(false)
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
    expect([foo.addon, foo.updateAvailable, foo.latest, foo.confidence, foo.lastSeen]).toEqual(["Foo", true, "v2", "high", "2026-10-15T18:29:00Z"])
    expect(foo.errors.map(M.errorMessageText)).toEqual([
      "attempt to index a nil value (field 'x')",
      "a message from the addon (25 characters)"])
    expect(foo.errors.map((e) => [e.method, e.lastSeen, e.sessions])).toEqual([
      ["first_addon_frame", "2026-10-15T18:29:00Z", 1], ["first_addon_frame", "2026-10-15T18:28:00Z", 1]])
    expect(foo.errors.map((e) => e.raw)).toEqual([null, null])
  })

  // The Errors view (1n2p.27, design doc §2.3, §6.6), over `errors` as the binary wrote it:
  // era without BugGrabber, retail with Foo's two errors (one an addon's own message),
  // one report sent by another player.
  test("the Errors view: a card where nothing records, each addon's errors in words", () => {
    const status = M.reduceStatus(fixture("status-errors"), NOW)
    const at = Date.parse("2026-10-15T18:29:00Z") + 3 * 3600000
    const sections = M.errorSections(ERRORS(), status.targets, "", at)
    expect(sections.map((s) => [s.target, s.title, s.rows.length, !!s.capture, s.empty])).toEqual([
      ["era", "Classic Era", 0, true, false], ["retail", "Retail", 1, false, false]])
    // Era: a card with the CLI's own install, never a row among errors.
    expect(sections[0].capture.text).toBe("Lua errors aren't recorded on Classic Era: install BugGrabber to record them.")
    expect(sections[0].capture.action.argv).toEqual(["omaddons", "install", "buggrabber", "-t", "era"])
    // Retail's header: its last session and new errors; a report from another player is
    // a count.
    expect(sections[1].count).toMatch(/^last session .* · 2 new$/)
    expect(sections[1].lines).toEqual(["1 error sent by other players (not counted)"])
    const foo = M.errorRows(sections)[0]
    expect([foo.index, foo.addon, foo.band, foo.meta, foo.message]).toEqual([0, "Foo", "urgent",
      "probably caused by it · 5 times · 2 new · last 3 hours ago", "attempt to index a nil value (field 'x')"])
    expect(foo.chips).toEqual([{ text: "update available (may fix it)", urgent: false }])
    expect(foo.actions.map((a) => [a.name, a.tooltip])).toEqual([["update", "Update Foo (u)"], ["copy", "Copy report (y)"],
      ["restore", "Restore its settings (r)"]])
    // One client: its section only.
    expect(M.errorSections(ERRORS(), status.targets, "retail", at).map((s) => s.target)).toEqual(["retail"])
    expect(M.errorSections(null, [], "", at)).toEqual([])
  })

  test("who most likely threw them, in words, from typed fields only", () => {
    const words = (a) => M.errorSourceText(Object.assign({ addon: "DBM", kind: "addon", confidence: "high", errors: [] }, a))
    expect(words({})).toBe("probably caused by it")
    expect(words({ confidence: "medium" })).toBe("maybe caused by it")
    expect(words({ confidence: "low" })).toBe("possibly caused by it")
    expect(words({ kind: "library" })).toBe("in a library DBM bundles")
    expect(words({ kind: "blizzard" })).toBe("in the game's own code")
    expect(words({ kind: "unknown" })).toBe("source unknown")
    expect(words({ errors: [{ method: "protected_call" }] })).toBe("blocked call from DBM")
    expect(M.errorMessageText({ messageKind: "custom", message: null, messageChars: 1 })).toBe("a message from the addon (1 character)")
    expect(M.errorMessageText({ messageKind: "lua_runtime", message: null, messageChars: null })).toBe("a message from the addon (? characters)")
  })

  test("the error page: every error, its stack with the addon's own frames marked", () => {
    const status = M.reduceStatus(fixture("status-errors"), NOW)
    const at = Date.parse("2026-10-15T18:29:00Z") + 3 * 3600000
    const foo = M.errorRows(M.errorSections(ERRORS(), status.targets, "", at))[0]
    const page = M.errorPage(foo, at)
    expect([page.title, page.subtitle]).toEqual(["Foo", "probably caused by it · 5 times · Retail"])
    expect(page.errors.map((e) => [e.message, e.location, e.meta])).toEqual([
      ["attempt to index a nil value (field 'x')", "Foo/core.lua:12", "4 times · new · last 3 hours ago"],
      ["a message from the addon (25 characters)", "Foo/boss.lua:3", "1 time · new · last 3 hours ago"]])
    expect(page.errors[0].stack).toEqual([{ text: "Foo/core.lua:12: in function 'Refresh'", own: true }, { text: "… (1 C frame)", own: false }])
    expect(M.errorPage(null, at)).toEqual({ title: "", subtitle: "", errors: [] })
  })

  // `y`: the report pasted for an author: strict text, never raw details, never a name.
  test("the report holds the addon, the client and each error's strict text, nothing raw", () => {
    const status = M.reduceStatus(fixture("status-errors"), NOW)
    const raw = M.reduceErrors(fixture("errors-raw"))
    const foo = M.errorRows(M.errorSections(raw, status.targets, "retail", NOW))[0]
    const report = M.errorReport(foo, status.targets)
    const version = M.targetById(status.targets, "retail").version
    expect(report.split("\n")).toEqual([
      "Foo v1 on Retail " + version, "",
      "attempt to index a nil value (field 'x')", "  at Foo/core.lua:12 · 4 times", "  Foo/core.lua:12: in function 'Refresh'", "  … (1 C frame)", "",
      "a message from the addon (25 characters)", "  at Foo/boss.lua:3 · 1 time"])
    // The raw read holds a character's name and a sender's; the report holds neither.
    expect(JSON.stringify(raw)).toContain("FakeChar")
    for (const planted of ["FakeChar", "FakeSender", "FAKEACCT"]) expect(report).not.toContain(planted)
    expect(M.errorReport(null, [])).toBe("")
  })

  test("the bar's middle click opens the most urgent view", () => {
    expect(M.urgentView({ errors: { new: 2 }, issuesUrgent: 1 })).toBe("errors")
    expect(M.urgentView({ errors: { new: 0 }, issuesUrgent: 1 })).toBe("issues")
    expect(M.urgentView({ errors: null, issuesUrgent: 0 })).toBe("updates")
    expect(M.urgentView(null)).toBe("updates")
  })

  test("raw details only from their own read, and a partial read says so", () => {
    const raw = M.reduceErrors(fixture("errors-raw"))
    expect(raw.raw).toBe(true)
    const retail = M.targetById(raw.targets, "retail")
    expect(retail.addons[0].errors[1].raw.message).toContain("FakeChar")
    const partial = M.targetById(M.reduceErrors(fixture("errors-partial")).targets, "retail")
    expect(partial.partial).toBe(true)
  })

  // The binary's status-attention envelope (Baz's folder deleted on retail), with the
  // warnings `status` can give besides (hand-added: the fixture home has none of them).
  const attentionWithWarnings = () => {
    const env = fixture("status-attention")
    env.warnings = [
      { code: "journal_pending", message: "transaction t on target k did not finish", fix: "omaddons doctor explain fm-x", fix_argv: ["omaddons", "doctor", "explain", "fm-x"] },
      { code: "unknown_flavor", message: "xyz: an unknown flavor folder", fix: "set its family: omaddons config set targets.xyz.family <family>", fix_argv: null },
      { code: "running_unknown", message: "/proc could not be read", fix: null, fix_argv: null },
      { code: "a_code_from_a_newer_omaddons", message: "Something a newer omaddons says", fix: null, fix_argv: null },
    ]
    return M.reduceStatus(env, NOW)
  }

  test("the Issues sections: each client's problems under its name, then the notes; no Lua errors", () => {
    const status = attentionWithWarnings()
    const sections = M.issueSections(status.targets, status.warnings, "", status.issues, status.commands)
    expect(sections.map((s) => [s.key, s.title, s.count])).toEqual([["client/retail", "Retail", "1 problem"], ["notes", "Notes", ""]])
    const rows = M.issueRows(sections)
    expect(rows.map((r) => [r.index, r.kind])).toEqual([[0, "problem"], [1, "note"], [2, "note"]])
    // The problem in words: its severity, its sentence, its client, its named fix.
    const baz = rows[0]
    expect([baz.severityWord, baz.title, baz.client, baz.band, baz.glyph]).toEqual(["Needs attention", "Baz's folder is gone", "Retail", "urgent", 0xF0026])
    // Its reinstall previews through the template, never the remediation argv run as is.
    expect([baz.fix, baz.action, baz.go, baz.primary]).toEqual([{ template: "reinstall", slots: { target: "retail", addon: "Baz" }, label: "Repair Baz" }, null, null, "fix"])
    expect(baz.buttons.map((b) => [b.name, b.text])).toEqual([["fix", "Repair Baz"], ["explain", "Explain"]])
    expect(baz.buttons[1].tooltip).toBe("Read why: the problem's page, with the doctor's explanation (e)")
    // `status`'s own reinstall recommendation is that row's fix: no second row.
    expect(status.commands.map((c) => c.action)).toEqual(["reinstall"])
    expect(rows.filter((r) => r.kind === "suggestion")).toEqual([])
    // Notes, by code: words of the widget's own, the prose fix (it names a config key)
    // left out; a newer omaddons's code keeps its message; the hero's and the problems'
    // codes are not repeated.
    expect(rows.slice(1).map((r) => [r.title, r.prose, r.buttons.length, r.text])).toEqual([
      ["A client's game version isn't recognised, so omaddons treats it carefully.", "", 0, ""],
      ["Something a newer omaddons says", "", 0, ""],
    ])
    // Narrowed to a client: its problems only; one with none has no section at all.
    expect(M.issueSections(status.targets, status.warnings, "retail", status.issues, status.commands).map((s) => s.key)).toEqual(["client/retail"])
    expect(M.issueSections(status.targets, status.warnings, "era", status.issues, status.commands)).toEqual([])
  })

  test("the clients with the most severe problems come first, the machine before them all", () => {
    const env = fixture("status-clients")
    const issue = env.data.targets.find((t) => t.id === "anniversary").issues[0]
    env.data.targets.find((t) => t.id === "classic").issues = [Object.assign({}, issue, { severity: "P0", title: "broken" })]
    env.data.targets.find((t) => t.id === "wotlk").issues = [Object.assign({}, issue, { severity: "P3", title: "minor" })]
    env.data.issues = [Object.assign({}, issue, { severity: "P3", title: "the machine's" })]
    const status = M.reduceStatus(env, NOW)
    const sections = M.issueSections(status.targets, [], "", status.issues, status.commands)
    expect(sections.map((s) => s.title)).toEqual(["This computer", "Mists of Pandaria Classic", "Burning Crusade Anniversary",
      "Wrath 3.3.5a private server", "Suggested"])
    expect(M.issueRows(sections).filter((r) => r.kind === "problem").map((r) => r.severityWord)).toEqual(["Minor", "Broken", "Worth fixing", "Minor"])
    expect(M.problemCount(sections)).toBe(4)
  })

  test("status's recommendations are suggestions in words, each with its named fix", () => {
    const due = M.reduceStatus(fixture("status-check-due"), NOW)
    const rows = M.issueRows(M.issueSections(due.targets, due.warnings, "", due.issues, due.commands))
    expect(rows.map((r) => [r.kind, r.title, r.buttons.map((b) => b.text), r.target])).toEqual([
      ["suggestion", "1 addon on Retail was installed by hand or by another addon manager.", ["Take over Retail's addons…"], "retail"],
      ["suggestion", "Not checked for updates lately.", ["Check for updates"], ""],
      ["suggestion", "Retail has no recent backup of its settings.", ["Back up Retail"], "retail"],
    ])
    // Through the frame's own ways, or the template `b` runs.
    expect(rows.map((r) => [r.go, r.fix])).toEqual([
      [{ kind: "adopt", view: "installed", target: "retail" }, null],
      [{ kind: "check", view: "", target: "" }, null],
      [null, { template: "backup_target", slots: { target: "retail" }, label: "Back up Retail" }],
    ])
    expect(M.problemCount(M.issueSections(due.targets, due.warnings, "", due.issues, due.commands))).toBe(0)
    // Narrowed to a client: the suggestions for that client only.
    expect(M.issueRows(M.issueSections(due.targets, due.warnings, "retail", due.issues, due.commands)).map((r) => r.go ? r.go.kind : r.fix.template))
      .toEqual(["adopt", "backup_target"])
    expect(M.issueSections(due.targets, due.warnings, "era", due.issues, due.commands)).toEqual([])
    // A missing client: its problem forgets it (a plan sheet first: targets rm is
    // destructive); the same forget_target recommendation is not listed twice; the update
    // of every client previews through its template.
    const clients = M.reduceStatus(fixture("status-clients"), NOW)
    const listed = M.issueRows(M.issueSections(clients.targets, clients.warnings, "", clients.issues, clients.commands))
    expect(clients.commands.map((c) => c.action)).toEqual(["update", "forget_target"])
    expect(listed.map((r) => [r.kind, r.title, r.buttons.map((b) => b.text)])).toEqual([
      ["problem", "The client is gone or no longer installed", ["Forget Burning Crusade Anniversary…", "Explain"]],
      ["suggestion", "2 updates are waiting on 2 clients.", ["Update all"]],
    ])
    expect([listed[0].action.argv, listed[0].action.destructive]).toEqual([["omaddons", "targets", "rm", "anniversary"], true])
    expect(listed[1].fix).toEqual({ template: "update_all", slots: {}, label: "Update all" })
  })

  test("every recommendation the CLI makes has its own words", () => {
    // ubs:ignore — action names, not secrets
    const actions = ["reinstall", "update", "adopt", "check_updates", "check_errors", "backup_create", "refresh_backups", "reinstall_widget",
      "remove_orphans", "forget_target", "inspect", "list_unmanaged", "status"]
    for (const action of actions) {
      expect([action, M.suggestionText(action, M.argvParts(["omaddons", "status"]), [])]).not.toEqual([action, "omaddons suggests this next step."])
    }
    const parts = M.argvParts(["omaddons", "remove", "LibA", "LibB", "-t", "retail"])
    expect(parts).toEqual({ path: "remove", target: "retail", flags: [], operands: ["LibA", "LibB"] })
    expect(M.suggestionText("remove_orphans", parts, [{ id: "retail", displayName: "Retail" }])).toBe("Nothing on Retail needs LibA and LibB any more.")
    expect(M.suggestionText("something_new", parts, [])).toBe("omaddons suggests this next step.")
  })

  test("a CLI command runs through a template, a view of the panel's own, or the classifier", () => {
    const targets = [{ id: "retail", displayName: "Retail", issues: [] }]
    const way = (argv) => {
      const found = M.issueFix(M.cliAction(argv.join(" "), argv, false), targets)
      return found && [found.fix ? found.fix.template : found.go ? found.go.kind + ":" + found.go.view + ":" + found.go.target : found.action.argv.join(" "), found.label]
    }
    table([
      ["a_reinstall", ["omaddons", "install", "Plater", "-t", "retail", "--reinstall"], ["reinstall", "Repair Plater"]],
      ["an_update_of_one_addon", ["omaddons", "update", "DBM", "-t", "retail"], ["update_addon", "Update DBM"]],
      ["an_update_of_one_client", ["omaddons", "update", "-t", "retail"], ["update_target", "Update Retail"]],
      ["every_update", ["omaddons", "update"], ["update_all", "Update all"]],
      ["a_backup", ["omaddons", "backup", "create", "-t", "retail"], ["backup_target", "Back up Retail"]],
      ["every_backup", ["omaddons", "backup", "create"], ["backup_all", "Back up every client"]],
      ["a_check", ["omaddons", "outdated"], ["check::", "Check for updates"]],
      ["a_refresh", ["omaddons", "status"], ["refresh::", "Refresh"]],
      ["the_panel", ["omaddons", "omarchy", "install"], ["widget::", "Reinstall panel"]],
      ["the_lua_errors", ["omaddons", "errors"], ["view:errors:", "Read Lua errors"]],
      ["the_backups", ["omaddons", "backup", "list", "-t", "retail"], ["view:backups:retail", "Refresh backups"]],
      ["a_look", ["omaddons", "list", "-t", "retail", "-v"], ["view:installed:retail", "Show Retail's addons"]],
      ["unmanaged_folders", ["omaddons", "list", "-t", "retail", "--unmanaged"], ["view:installed:retail", "Show Retail's addons"]],
      ["the_adopt_report", ["omaddons", "adopt", "-t", "retail"], ["adopt:installed:retail", "Take over Retail's addons…"]],
      ["a_forget", ["omaddons", "targets", "rm", "retail"], ["omaddons targets rm retail", "Forget Retail…"]],
      ["a_removal", ["omaddons", "remove", "LibA", "-t", "retail"], ["omaddons remove LibA -t retail", "Remove LibA"]],
      // Any other shape runs as itself, through the classifier.
      ["an_install_with_more", ["omaddons", "install", "Plater", "-t", "retail", "--reinstall", "--channel", "beta"],
        ["omaddons install Plater -t retail --reinstall --channel beta", "Run the fix"]],
      ["a_pin", ["omaddons", "pin", "DBM", "-t", "retail"], ["omaddons pin DBM -t retail", "Run the fix"]],
      // Nothing for what the panel never runs.
      ["a_forced_command", ["omaddons", "install", "Foo", "-t", "retail", "--force"], null],
      ["a_repair_outside_its_flow", ["omaddons", "doctor", "fix", "--only", "fm-x"], null],
    ], way)
    expect(M.issueFix(null, targets)).toBeNull()
    // A long name: the button cuts it, the recorded reason keeps it whole.
    const long = "Details_DamageMeter_StreamerModeAndPluginsCollection_Classic"
    const repair = M.issueFix(M.cliAction("x", ["omaddons", "install", long, "-t", "retail", "--reinstall"], false), targets)
    expect([repair.label, repair.fix.label]).toEqual(["Repair Details_DamageMeter_StreamerMod…", "Repair " + long])
    expect(M.buttonName("Plater")).toBe("Plater")
    // A client named at length: the run's label is one line within reasonOf's bound.
    const named = [{ id: "retail", displayName: "R".repeat(200), issues: [] }]
    const update = M.issueFix(M.cliAction("x", ["omaddons", "update", "-t", "retail"], false), named)
    expect([update.fix.label.length, update.label]).toEqual([120, "Update " + "R".repeat(31) + "…"])
    expect(M.buildArgv(update.fix.template, update.fix.slots, opts({ label: update.fix.label, preview: true })).argv).toContain("update")
  })

  test("problems keep status's order within a severity, whatever the engine's sort", () => {
    const env = JSON.parse(JSON.stringify(fixture("status-attention")))
    const issue = env.data.targets[0].issues[0]
    const severities = ["P2", "P1", "P2", "P1", "P2", "P1"]
    env.data.targets[0].issues = severities.map((severity, i) =>
      Object.assign(JSON.parse(JSON.stringify(issue)), { severity, title: "issue " + i }))
    env.data.issues = [{ id: "fm-a", severity: "P1", title: "machine", remediation: { command: "omaddons status", argv: ["omaddons", "status"], destructive: false } }]
    const status = M.reduceStatus(env, NOW)
    const rows = withTiesReversed(() => M.issueRows(M.issueSections(status.targets, [], "", status.issues)))
    expect(rows.filter((r) => r.kind === "problem").map((r) => r.title))
      .toEqual(["machine", "issue 1", "issue 3", "issue 5", "issue 0", "issue 2", "issue 4"])
  })

  test("a problem of no client is listed first among the problems, unless narrowed", () => {
    const env = JSON.parse(JSON.stringify(fixture("status-attention")))
    env.data.issues = [{ id: "fm-journal-pending", severity: "P0", title: "An interrupted change needs manual repair",
      remediation: { command: "omaddons status", argv: ["omaddons", "status"], destructive: false } }]
    const status = M.reduceStatus(env, NOW)
    expect(status.issues.map((i) => [i.severity, i.urgent])).toEqual([["P0", true]])
    const rows = M.issueRows(M.issueSections(status.targets, [], "", status.issues))
    expect([rows[0].target, rows[0].title]).toEqual(["", "An interrupted change needs manual repair"])
    const narrowed = M.issueRows(M.issueSections(status.targets, [], "retail", status.issues))
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

  test("cliAction keeps an argv without a consent flag, but a repair runs only through its own flow", () => {
    const reinstall = ["omaddons", "install", "Plater", "-t", "retail", "--reinstall"]
    expect([remediated(reinstall).runnable, remediated(reinstall).argv]).toEqual([true, reinstall])
    // G2: `doctor fix` from CLI output is text; the bound doctor_fix template runs it.
    const fix = remediated(FIX)
    expect([fix.runnable, fix.argv, fix.command]).toEqual([false, null, FIX.join(" ")])
  })

  test("a consent flag past -- is refused too", () => {
    const install = remediated(["omaddons", "install", "--", "--force"])
    expect([install.runnable, install.argv]).toEqual([false, null])
  })

  test("a prose remediation is a sentence with nothing to copy; a refused command is text to copy", () => {
    const env = fixture("status-attention")
    const prose = "quit the game, then run any change"
    env.data.targets[0].issues.push(Object.assign({}, env.data.targets[0].issues[0], { remediation: { command: prose, argv: null, destructive: false } }))
    env.data.targets[0].issues.push(Object.assign({}, env.data.targets[0].issues[0], { remediation: { command: "rm -rf ~", argv: ["rm", "-rf", "~"], destructive: true } }))
    env.data.targets[0].issues.push(Object.assign({}, env.data.targets[0].issues[0], { remediation: { command: "omaddons install Foo --force", argv: ["omaddons", "install", "Foo", "--force"], destructive: false } }))
    const rows = M.issueRows(M.issueSections(M.reduceStatus(env, NOW).targets, [], ""))
    const pick = (r) => [r.action, r.fix, r.prose, r.text, r.note, r.buttons.map((b) => b.name), M.copyText(r)]
    // The same problem three times, each with its own key.
    expect(rows.map((r) => r.key)).toEqual(["p/retail/fm-state_files-record-folder-missing/Baz", "p/retail/fm-state_files-record-folder-missing/Baz#2",
      "p/retail/fm-state_files-record-folder-missing/Baz#3", "p/retail/fm-state_files-record-folder-missing/Baz#4"])
    expect(pick(rows[1])).toEqual([null, null, prose, "", "", ["explain"], ""])
    // An argv that is no omaddons command line is neither run nor offered to copy.
    expect(pick(rows[2])).toEqual([null, null, "", "", "", ["explain"], ""])
    expect(pick(rows[3])).toEqual([null, null, "", "omaddons install Foo --force",
      "This needs your go-ahead, which the panel never gives: read the explanation, then run it in a terminal.",
      ["explain", "copy"], "omaddons install Foo --force"])
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
    const targets = [{ id: "retail", displayName: "Retail" }, { id: "era", displayName: "Classic Era" }]
    expect(M.installTargets(result, targets)).toEqual([
      { id: "retail", client: "Retail", hint: "likely", installed: false, words: "Probably works on Retail", eligible: true },
      { id: "era", client: "Classic Era", hint: "unknown", installed: false, words: "Not known yet for Classic Era", eligible: false }])
    expect(M.installTargets(Object.assign({}, result, { hints: { retail: "no" } }), targets)[0].eligible).toBe(false)
  })

  test("raw lines only for the row that asked; popularity", () => {
    const raw = { selection: { target: "retail", addon: "Foo" }, data: M.reduceErrors(fixture("errors-raw")) }
    const row = { target: "retail", addon: "Foo" }
    const lines = M.rawLines(raw, row)
    expect(lines[0]).toContain("attempt to index a nil value")
    expect(lines.join("\n")).toContain("FakeChar")
    expect(M.rawLines(raw, { target: "retail", addon: "Bar" })).toBeNull()
    expect(M.rawLines(null, row)).toBeNull()
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

  test("the dry runs read as plan sheets, the version note included", () => {
    const targets = [{ id: "retail", key: "", displayName: "Retail" }]
    const addon = M.confirmSheet("restore_addon", fixture("restore-addon-preview"), CONTEXT, targets)
    expect(addon.title).toMatch(/^Restore WeakAuras' settings on Retail from \d{4}-\d\d-\d\d \d\d:\d\d\?$/)
    expect(addon.confirmLabel).toBe("Restore WeakAuras' settings")
    expect(addon.items.map((i) => [i.name, i.detail])).toEqual([["WeakAuras", "2 files replaced, 0 back, 0 added since (moved aside)"]])
    expect(addon.effects.map((e) => e.text)).toEqual(["Settings written by WeakAuras 5.19.2; installed now: 5.20.0."])
    const notIn = JSON.parse(JSON.stringify(fixture("restore-addon-preview")))
    notIn.data.addons[0].new_folders = ["WeakAurasOptions"]
    expect(M.dialogText(M.confirmSheet("restore_addon", notIn))).toContain("WeakAurasOptions isn't in this backup and stays as it is.")
    const whole = M.confirmSheet("restore", fixture("restore-preview"), CONTEXT, targets)
    expect(whole.title).toMatch(/^Restore all of Retail's settings from \d{4}-\d\d-\d\d \d\d:\d\d\?$/)
    expect(whole.items.map((i) => i.text)).toEqual(["5 files back, 2 differ now, 0 added since (moved aside)"])
    // What else a whole restore changes is urgent, so it comes first.
    expect(whole.effects.map((e) => [e.kind, e.severity, e.text])).toEqual([
      ["restore_scope", "urgent", "Also restores keybindings, macros, chat and every other addon's settings."],
      ["kept_backup", "normal", "Your current settings are kept as a backup first."],
      ["note", "normal", "Then restart the game."]])
    const skipped = JSON.parse(JSON.stringify(fixture("restore-preview")))
    skipped.warnings = [{ code: "before_restore_skipped", message: "this filesystem cannot clone, so the current WTF/ is kept only in the undo snapshot", fix: null }]
    const skippedText = M.dialogText(M.confirmSheet("restore", skipped))
    expect(skippedText).toContain("Today's settings won't be backed up first (this drive can't share files); undo still brings them back.")
    // The warning's code decides the line, never its text.
    expect(skippedText).not.toContain("undo snapshot")
    const verified = JSON.parse(JSON.stringify(fixture("restore-preview")))
    verified.data.backup.integrity = "verified"
    expect(M.dialogText(M.confirmSheet("restore", verified))).toContain("Checked against the backup's checksums.")
    const prune = M.confirmSheet("prune_backup", fixture("prune-preview"), CONTEXT, targets)
    expect([prune.title, prune.confirmLabel, prune.items.map((i) => i.text)]).toEqual(["Delete a backup on Retail?", "Delete", ["made by you, 1.2 KiB"]])
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
    const retail = status.targets.find((t) => t.id === "retail").displayName
    expect(facts).toEqual({ names: ["Foo"], counts: [2], targets: [retail] })
    const argv = M.errorToastArgv(facts, "a44.omaddons-fixture")
    expect(argv.slice(0, 7)).toEqual(["omarchy-notification-send", "--app-name", "omaddons", "-g", "\u{F0297}", "-u", "normal"])
    expect(argv.slice(7, 9)).toEqual(["Lua errors in your last session on " + retail, "Foo 2"])
    // A click opens the panel at Errors, through the widget's own IPC (1n2p.33).
    expect(argv.slice(-5)).toEqual(["--exec", "omarchy-shell", "a44.omaddons-fixture", "show", "errors"])
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

  test("update and result toasts: clients by name, each click to its view", () => {
    const targets = [{ id: "era", displayName: "Classic Era" }, { id: "retail", displayName: "Retail" }]
    const facts = M.updateFacts(targets, { updates: { available: 3, targets: ["era", "retail"] } })
    expect(facts).toEqual({ count: 3, targets: ["Classic Era", "Retail"] })
    const update = M.updateToastArgv(facts, "id")
    expect(update.slice(5, 9)).toEqual(["-u", "low", "Addon updates", "3 updates on Classic Era and Retail"])
    expect(update.slice(-4)).toEqual(["omarchy-shell", "id", "show", "updates"])
    expect(M.updateToastArgv({ count: 1, targets: ["Retail"] }, "id")[8]).toBe("1 update on Retail")
    const done = M.resultToastArgv("Updated 3 addons", true, "id")
    expect(done.slice(5, 9)).toEqual(["-u", "low", "omaddons", "Updated 3 addons"])
    expect(done.slice(-4)).toEqual(["omarchy-shell", "id", "show", ""])
    // A change the player asked for that failed is worth more than a low toast.
    expect(M.resultToastArgv("Couldn't update DBM", false, "id")[6]).toBe("normal")
    // Nothing is ever critical: a game is never more urgent than Do Not Disturb.
    for (const argv of [update, done, M.errorToastArgv({ names: ["A"], counts: [1], targets: [] }, "id")]) {
      expect(argv[6]).not.toBe("critical")
      expect(argv.slice(1, 3)).toEqual(["--app-name", "omaddons"])
    }
  })

  test("the session digest: names and counts only, Errors first", () => {
    const facts = { names: ["DBM", "WeakAuras"], counts: [2, 1], targets: ["Retail"], message: "secret" }
    const both = M.digestToastArgv(facts, { count: 2, targets: ["Retail"] }, { errors: true, updates: true }, "id")
    expect(both.slice(5, 9)).toEqual(["-u", "normal", "After your session", "3 new Lua errors in DBM and WeakAuras · 2 updates"])
    expect(both.slice(-2)).toEqual(["show", "errors"])
    expect(both.join(" ")).not.toContain("secret")
    const updates = M.digestToastArgv(facts, { count: 1, targets: ["Retail"] }, { errors: false, updates: true, session: true }, "id")
    expect(updates.slice(5, 9)).toEqual(["-u", "low", "After your session", "1 update"])
    expect(updates.slice(-2)).toEqual(["show", "updates"])
    // Both kinds at once with no session behind them: one toast, not titled for a session.
    const now = M.digestToastArgv(facts, { count: 2 }, { errors: true, updates: true, session: false }, "id")
    expect(now[7]).toBe("Lua errors and addon updates")
  })

  describe("toastPlan (1n2p.33)", () => {
    const GAP = M.TOAST_MIN_GAP_MS
    const on = { notifyErrors: true, notifyUpdates: true, whilePlaying: false }
    // One status after another from `state`, each `{errorsNew, updates, playing, nowMs}`.
    function run(steps, switches) {
      let state = M.emptyToastState()
      let before = { errorsNew: 0, updates: 0 }
      const out = []
      steps.forEach((step, i) => {
        const plan = M.toastPlan(state, { first: i === 0, before, ...on, ...switches, ...step })
        state = plan.state
        before = { errorsNew: step.errorsNew, updates: step.updates }
        out.push(plan.toast === "digest" ? "digest:" + Object.keys(plan.kinds).filter((k) => plan.kinds[k]).join("+") : plan.toast)
      })
      return out
    }

    test("never on the first status; a rise speaks once; the same counts say nothing", () => {
      expect(run([
        { errorsNew: 3, updates: 2, playing: false, nowMs: 0 },
        { errorsNew: 4, updates: 2, playing: false, nowMs: GAP },
        { errorsNew: 4, updates: 2, playing: false, nowMs: 2 * GAP },
      ])).toEqual([null, "errors", null])
    })

    test("nothing while a game runs, then one digest when the last client stops", () => {
      expect(run([
        { errorsNew: 0, updates: 0, playing: false, nowMs: 0 },
        { errorsNew: 2, updates: 0, playing: true, nowMs: GAP },
        { errorsNew: 3, updates: 1, playing: true, nowMs: GAP + 60000 },
        { errorsNew: 3, updates: 1, playing: false, nowMs: GAP + 120000 },
        { errorsNew: 3, updates: 1, playing: false, nowMs: 3 * GAP },
      ])).toEqual([null, null, null, "digest:errors+updates+session", null])
      // One kind held through a session is still the session's digest.
      expect(run([
        { errorsNew: 0, updates: 0, playing: false, nowMs: 0 },
        { errorsNew: 1, updates: 0, playing: true, nowMs: GAP },
        { errorsNew: 1, updates: 0, playing: false, nowMs: GAP + 1 },
      ])).toEqual([null, null, "digest:errors+session"])
    })

    test("a client whose state is unknown counts as playing", () => {
      const status = { targets: [{ id: "retail", running: false, runningUnknown: true }] }
      expect(M.playing(status.targets)).toBe(true)
      expect(M.playing([{ id: "retail", running: false, runningUnknown: false }])).toBe(false)
      expect(M.playing([{ id: "retail", running: true }])).toBe(true)
    })

    test("notifyWhilePlaying On speaks at once", () => {
      expect(run([
        { errorsNew: 0, updates: 0, playing: true, nowMs: 0 },
        { errorsNew: 1, updates: 0, playing: true, nowMs: GAP },
      ], { whilePlaying: true })).toEqual([null, "errors"])
    })

    test("one toast per kind per gap: a rise inside it waits, a rise of two kinds is one digest", () => {
      expect(run([
        { errorsNew: 0, updates: 0, playing: false, nowMs: 0 },
        { errorsNew: 1, updates: 0, playing: false, nowMs: GAP },
        { errorsNew: 2, updates: 0, playing: false, nowMs: GAP + 60000 },
        { errorsNew: 2, updates: 0, playing: false, nowMs: 2 * GAP },
        { errorsNew: 3, updates: 4, playing: false, nowMs: 4 * GAP },
      ])).toEqual([null, "errors", null, "errors", "digest:errors+updates"])
    })

    test("a held kind lets go when answered or turned off", () => {
      expect(run([
        { errorsNew: 0, updates: 0, playing: false, nowMs: 0 },
        { errorsNew: 2, updates: 0, playing: true, nowMs: GAP },
        { errorsNew: 0, updates: 0, playing: false, nowMs: GAP + 1 },
      ])).toEqual([null, null, null])
      let state = M.emptyToastState()
      state = M.toastPlan(state, { first: false, before: { errorsNew: 0, updates: 0 }, errorsNew: 1, updates: 0, playing: true, ...on, nowMs: GAP }).state
      const off = M.toastPlan(state, { first: false, before: { errorsNew: 1, updates: 0 }, errorsNew: 1, updates: 0, playing: false, ...on, notifyErrors: false, nowMs: GAP + 1 })
      expect(off.toast).toBe(null)
      expect(off.state.held.errors).toBe(false)
    })
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
    // Every client's hint by its id, never one client's verdict standing for all (bmcx.6).
    for (const row of model.rows) expect([row.name, Object.keys(row.hints)]).toEqual([row.name, ["retail"]])
    expect(model.rows.some((r) => "compat" in r)).toBe(false)
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

  test("plan sheets and stale banners", () => {
    const targets = [{ id: "era", key: "", displayName: "Classic Era" }, { id: "retail", key: "", displayName: "Retail" }]
    // A plan path previews through its approval (previewDecision); confirmSheet reads
    // the command's own data.
    expect(M.dialogText(M.confirmSheet("undo", fixture("undo-preview"), CONTEXT, targets)))
      .toMatch(/^Undo the update on Classic Era and Retail from \d{4}-\d\d-\d\d \d\d:\d\d\?\nBar back as it was\nFoo back as it was$/)
    // An id no client has is shown as it is.
    expect(M.confirmSheet("undo", fixture("undo-preview")).title).toMatch(/^Undo the update on era and retail from /)
    expect(M.staleText(fixture("remove-stale"))).toBe("Changed since you looked: the command is different; another change landed on a client; "
      + "a newer version came out, or the change is different now; files it would change were changed. Nothing ran; here is what would happen now.")
    // The undo of a whole restore says it reverts the play since, and that comes first.
    const shape = { schema_version: "1.0", tool_version: "0.1.0", ok: true }
    const undoRestore = { ...shape, command: "undo", data: { reverted: { kind: "restore", at: "2026-10-22T20:46:00Z" },
      transactions: [{ target: { id: "retail" } }], folders: { restored: [], removed: [] }, changed_since: 3 } }
    const reverts = M.confirmSheet("undo", undoRestore, CONTEXT, targets)
    expect(reverts.title).toMatch(/^Undo the restore on Retail from /)
    expect(reverts.effects.map((e) => [e.severity, e.text])).toEqual([
      ["urgent", "Also undoes every play session since the restore: 3 files the game changed (undo data keeps them)."]])
    const uncounted = { ...undoRestore, data: { ...undoRestore.data, changed_since: null } }
    expect(M.confirmSheet("undo", uncounted).effects.map((e) => e.text))
      .toEqual(["Also undoes every play session since the restore (the files it changed can't be counted)."])
    // An undo of an undo is a redo.
    const redo = { ...undoRestore, data: { ...undoRestore.data, reverted: { kind: "undo", at: "" }, changed_since: 0 } }
    expect([M.confirmSheet("undo", redo).title, M.confirmSheet("undo", redo).confirmLabel])
      .toEqual(["Undo the undo on retail from an earlier time?", "Redo"])
    // Forgetting a client says what it does, never "0 addons", and is destructive.
    const rm = { ...shape, command: "targets rm", data: { id: "wotlk", managed_addons: 2, was_default: false, dry_run: true } }
    const forget = M.confirmSheet("targets_rm", rm, CONTEXT, [{ id: "wotlk", key: "", displayName: "Wrath 3.3.5a private server" }])
    expect([forget.title, forget.confirmLabel, forget.destructive, forget.glyph, M.dialogText(forget)]).toEqual([
      "Forget Wrath 3.3.5a private server?", "Forget Wrath 3.3.5a private server", true, 0xF01B4,
      "Forget Wrath 3.3.5a private server?\nNothing in its folder is touched.\nomaddons keeps its record of 2 managed addons."])
    expect(M.dialogText(M.confirmSheet("targets_rm", { ...rm, data: { ...rm.data, managed_addons: 0 } })))
      .toBe("Forget wotlk?\nNothing in its folder is touched.")
    // A preview that failed says so, and lists nothing.
    expect(M.confirmSheet("remove", { ...shape, ok: false, command: "remove", exit_code: 4, error: { code: "game_running", message: "x" } }).title)
      .toBe("omaddons couldn't say what this would do")
  })

  test("only https links to known hosts open", () => {
    expect(M.isSafeUrl("https://github.com/o/r")).toBe(true)
    expect(M.isSafeUrl("https://www.wowinterface.com/downloads/info1.html")).toBe(true)
    expect(M.isSafeUrl("https://tukui.org/elvui")).toBe(true)
    // Every shape of `info`'s `about.homepage` (src/sources/about.rs, Project::homepage).
    for (const url of ["https://github.com/WeakAuras/WeakAuras2", "https://www.wowinterface.com/downloads/info7032",
      "https://tukui.org/elvui", "https://www.wowace.com/projects/bug-grabber"]) {
      expect([url, M.isSafeUrl(url)]).toEqual([url, true])
    }
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

// The clients the plan sheet tests name: Retail by its key, Classic Era running.
const SHEET_TARGETS = [{ id: "retail", key: KEY_RETAIL, displayName: "Retail", running: false },
  { id: "era", key: KEY_ERA, displayName: "Classic Era", running: true }]

describe("copy", () => {
  test("the table", () => {
    table([
      ["slots", ["sheet.title.update.one", { addon: "DBM", client: "Retail" }], "Update DBM on Retail?"],
      ["pair_one", ["sheet.item.remove", { n: 1, version: "v1" }], "v1 (1 folder)"],
      ["pair_other", ["sheet.item.remove", { n: 3, version: "v1" }], "v1 (3 folders)"],
      ["pair_zero_is_other", ["sheet.item.remove", { n: 0, version: "" }], " (0 folders)"],
      ["missing_slot_is_empty", ["sheet.title.forget", {}], "Forget ?"],
      ["null_slot_is_empty", ["sheet.title.forget", { client: null }], "Forget ?"],
      ["no_slots", ["sheet.cancel", undefined], "Cancel"],
      ["unknown_id_shows_itself", ["sheet.no.such.id", {}], "sheet.no.such.id"],
    ], (args) => M.copy(args[0], args[1]))
    table([["s", "WeakAuras", "WeakAuras'"], ["no_s", "Retail", "Retail's"], ["capital_s", "DBMS", "DBMS'"], ["empty", "", ""]],
      M.possessive)
  })

  test("every template's slots are named in snake case, and every pair has both forms", () => {
    for (const [id, entry] of Object.entries(M.COPY)) {
      const forms = typeof entry === "string" ? [entry] : [entry.one, entry.other]
      expect([id, forms.every((f) => typeof f === "string" && f !== "")]).toEqual([id, true])
      for (const form of forms) expect([id, (form.match(/\{[^}]*\}/g) || []).every((s) => /^\{[a-z_]+\}$/.test(s))]).toEqual([id, true])
    }
  })
})

describe("selectionItem", () => {
  test("the table", () => {
    const pick = (i) => [i.client, i.name, i.detail, i.text]
    table([
      ["update", [kept(), {}], ["Retail", "Foo", "v1 → v2", "Foo v1 → v2"]],
      ["update_same_version", [kept({ to_version: "v1" }), {}], ["Retail", "Foo", "v1", "Foo v1"]],
      ["install", [kept({ op: "install", addon: "WeakAuras", target_key: KEY_ERA, target_id: "era", from_version: null, to_version: "5.22.1" }), {}],
        ["Classic Era", "WeakAuras", "5.22.1 (new)", "WeakAuras 5.22.1 (new)"]],
      ["install_no_version", [kept({ op: "install", from_version: null, to_version: null }), {}], ["Retail", "Foo", "(new)", "Foo (new)"]],
      ["remove_with_folders", [kept({ op: "remove", addon: "Baz", to_version: null }), { baz: 2 }], ["Retail", "Baz", "v1 (2 folders)", "Baz v1 (2 folders)"]],
      ["remove_one_folder", [kept({ op: "remove", addon: "Baz", to_version: null }), { baz: 1 }], ["Retail", "Baz", "v1 (1 folder)", "Baz v1 (1 folder)"]],
      ["remove_unknown_folders", [kept({ op: "remove", addon: "Baz", to_version: null }), {}], ["Retail", "Baz", "v1", "Baz v1"]],
      ["edit", [Object.assign(kept({ op: "edit" }), { fields: ["any_flavor", "channel"] }), {}], ["Retail", "Foo", "changes its any flavor and channel", "Foo changes its any flavor and channel"]],
      ["adopt", [kept({ op: "adopt", from_version: null }), {}], ["Retail", "Foo", "v2", "Foo v2"]],
      // A client the view does not know shows by its id; one that names neither, as nothing.
      ["unknown_client", [kept({ target_key: "01M8Q2XXXXXXXXXXXXXXXXXXXX", target_id: "ptr" }), {}], ["ptr", "Foo", "v1 → v2", "Foo v1 → v2"]],
      ["no_versions", [kept({ from_version: null, to_version: null }), {}], ["Retail", "Foo", "", "Foo"]],
    ], (args) => pick(M.selectionItem(args[0], SHEET_TARGETS, args[1])))
  })
})

describe("reasonEffects", () => {
  // `reasons` for the one item `items` (or none), each effect as `[kind, severity, text]`.
  const effects = (reasons, scope, items, data) => {
    const approval = { routine: false, reasons: reasons, selection: items || [kept()], scope: scope || scopeOf() }
    const sheetItems = (items || [kept()]).map((i) => M.selectionItem(i, SHEET_TARGETS, {}))
    return M.reasonEffects(approval, sheetItems, SHEET_TARGETS, data || {}).map((e) => [e.kind, e.severity, e.text])
  }
  const lib = kept({ op: "install", addon: "LibFoo", from_version: null, to_version: "1.0" })
  const two = [kept(), kept({ addon: "Bar" })]

  test("the table", () => {
    const cases = [
      // Several addons asked for: which one needs it is not said, so none is named.
      ["dependency_added_for_several_addons", effects(["dependency_added"], scopeOf({ dependencies_added: ["LibFoo"] }), two.concat([lib])),
        [["dependency_added", "normal", "Also installs LibFoo 1.0, a new dependency."]]],
      ["dependency_added_names_its_addon", (() => {
        const approval = { routine: false, reasons: ["dependency_added"], selection: [kept(), lib], scope: scopeOf({ dependencies_added: ["LibFoo"] }) }
        return M.reasonEffects(approval, [M.selectionItem(kept(), SHEET_TARGETS, {})], SHEET_TARGETS, {}).map((e) => e.text)
      })(), ["Also installs LibFoo 1.0, which Foo needs."]],
      ["dependency_added_version_from_scope", effects(["dependency_added"], scopeOf({ dependencies_added: [{ addon: "LibFoo", version: "1.2" }] }), two),
        [["dependency_added", "normal", "Also installs LibFoo 1.2, a new dependency."]]],
      ["dependency_added_unnamed", effects(["dependency_added"]), [["dependency_added", "normal", "Also installs a new dependency."]]],
      ["folders_removed", effects(["folders_removed"], scopeOf({ folders_removed: ["Foo_Options", { folder: "Foo_Extra" }] })),
        [["folders_removed", "urgent", "Removes the folder Foo_Options. Undo brings it back."],
          ["folders_removed", "urgent", "Removes the folder Foo_Extra. Undo brings it back."]]],
      ["folders_removed_unnamed", effects(["folders_removed"]), [["folders_removed", "urgent", "Removes folders. Undo brings them back."]]],
      ["local_changes_replaced_two_folders", effects(["local_changes_replaced"], scopeOf({ local_changes_replaced: { count: 3, folders: ["DBM-Core", "DBM-GUI"] } })),
        [["local_changes_replaced", "urgent", "Replaces 3 files you changed in DBM-Core and DBM-GUI. Undo brings them back."]]],
      ["local_changes_replaced_one", effects(["local_changes_replaced"], scopeOf({ local_changes_replaced: { count: 1, folders: ["Foo"] } })),
        [["local_changes_replaced", "urgent", "Replaces 1 file you changed in Foo. Undo brings it back."]]],
      ["local_changes_replaced_no_folders", effects(["local_changes_replaced"], scopeOf({ local_changes_replaced: { count: 2, folders: [] } })),
        [["local_changes_replaced", "urgent", "Replaces 2 files you changed. Undo brings them back."]]],
      ["local_changes_replaced_uncounted", effects(["local_changes_replaced"]),
        [["local_changes_replaced", "urgent", "Replaces files you changed. Undo brings them back."]]],
      ["source_changed", effects(["source_changed"], scopeOf({ source_changed: ["Foo", { addon: "Baz" }] }), [kept({ source: "wowi" })]),
        [["source_changed", "normal", "Foo will come from WoWInterface now."], ["source_changed", "normal", "Baz will come from another source now."]]],
      ["source_changed_unnamed", effects(["source_changed"]), [["source_changed", "normal", "An addon here will come from another source now."]]],
      ["channel_changed", effects(["channel_changed"], scopeOf(), [kept({ channel: "beta" })]), [["channel_changed", "normal", "Foo switches to beta releases."]]],
      ["channel_changed_two_addons", effects(["channel_changed"], scopeOf(), two), [["channel_changed", "normal", "An addon here switches to another release channel."]]],
      ["while_running_names_the_running_client", effects(["while_running"], scopeOf({ while_running: true }), [kept({ target_key: KEY_ERA, target_id: "era" })]),
        [["while_running", "normal", "Classic Era is running: then /reload in game."]]],
      ["while_running_restart", effects(["while_running"], scopeOf({ while_running: true }), [kept({ target_key: KEY_ERA, target_id: "era" })], { reload_required: "restart" }),
        [["while_running", "normal", "Classic Era is running: then restart the game."]]],
      ["while_running_unknown_client", effects(["while_running"], scopeOf({ while_running: true })),
        [["while_running", "normal", "The game is running: then /reload in game."]]],
      ["reinstall", effects(["reinstall"]), [["reinstall", "normal", "Puts back Foo's files exactly as released."]]],
      ["reinstall_two", effects(["reinstall"], scopeOf(), two), [["reinstall", "normal", "Puts back the files exactly as released."]]],
      ["force", effects(["force"]), [["force", "urgent", "Takes over folders another addon owns."]]],
      ["downgrade", effects(["downgrade"], scopeOf(), [kept({ from_version: "v2", to_version: "v1" })]),
        [["downgrade", "normal", "Goes back to an older version (v2 → v1)."]]],
      ["downgrade_two", effects(["downgrade"], scopeOf(), two), [["downgrade", "normal", "Goes back to an older version."]]],
      // The title's verb and the urgent button say it: no line.
      ["destructive", effects(["destructive"]), []],
      ["unknown_reason", effects(["some_new_thing"]), [["note", "normal", "Also: some new thing."]]],
      ["no_approval", M.reasonEffects(null, [], [], {}), []],
    ]
    for (const [name, got, expected] of cases) expect([name, got]).toEqual([name, expected])
  })

  test("every effect kind has a glyph, and the urgent ones are the costly ones", () => {
    for (const kind of M.SHEET_URGENT) expect([kind, typeof M.SHEET_GLYPHS[kind]]).toEqual([kind, "number"])
    expect(M.SHEET_URGENT.slice().sort()).toEqual(["changed_since", "folders_removed", "force", "local_changes_replaced", "restore_scope", "still_required"])
  })
})

describe("previewDecision", () => {
  // A decision as the table compares it: its plan sheet as text, with its verb and
  // whether it is destructive.
  const pick = (d) => ({ kind: d.kind, heading: d.heading, text: M.dialogText(d.sheet), confirm: d.sheet ? d.sheet.confirmLabel : "",
    destructive: d.sheet ? d.sheet.destructive : false, line: d.line, fixText: d.fixText, planId: d.plan ? d.plan.planId : null })
  const decision = (fields) => Object.assign({ kind: "dialog", heading: "", text: "", confirm: "", destructive: false, line: "", fixText: "",
    planId: PLAN_ID }, fields)
  const update = (text, extra) => decision(Object.assign({ text: text, confirm: "Update Foo" }, extra || {}))
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
        text: "Remove Baz from retail?\nBaz v1 (1 folder)\nRemoves the folder Baz. Undo brings it back.", confirm: "Remove Baz", destructive: true })],
      ["match_equal_routine", match([shownItem()]), previewOf([item()]), decision({ kind: "direct" })],
      // The dependency it adds is an effect, never an item.
      ["match_equal_not_routine", match([shownItem()]), withDependency, update("Update Foo on retail?\nFoo v1 → v2\nAlso installs LibFoo 1.0, which Foo needs.")],
      ["match_different", match([shownItem()]), previewOf([item({ to_version: "v3", release_id: "gh:o/foo@v3", asset: "Foo-v3.zip" })]), update(
        "Update Foo on retail?\nChanged since you looked: Foo v3 is now the newest\nFoo v1 → v3", { heading: "Changed since you looked: Foo v3 is now the newest" })],
      ["match_empty_shown", match([]), previewOf([item()]), update("Update Foo on retail?\nFoo v1 → v2")],
      ["match_no_shown", { mode: "match", path: "update" }, previewOf([item()]), update("Update Foo on retail?\nFoo v1 → v2")],
      ["match_equal_routine_again", match([shownItem()], { again: true }), previewOf([item()]), update("Update Foo on retail?\nFoo v1 → v2")],
      // The offer's own `destructive` makes the verb urgent; the path table never does.
      ["match_equal_routine_destructive_offer", match([shownItem()]), previewOf([item()], {}, [offer(PLAN_ID, { destructive: true })]),
        update("Update Foo on retail?\nFoo v1 → v2", { destructive: true })],
      ["unknown_mode_is_never_direct", { mode: "auto", shown: [shownItem()], path: "update" }, previewOf([item()]), update("Update Foo on retail?\nFoo v1 → v2")],
    ]
    for (const [name, spec, env, expected] of cases) expect([name, pick(M.previewDecision(spec, env))]).toEqual([name, expected])
  })

  test("the sheet names clients by their player names and groups items by client", () => {
    const era = (fields) => item(Object.assign({ target_key: KEY_ERA, target_id: "era" }, fields))
    const two = previewOf([era(), era({ addon: "Bar", release_id: "gh:o/bar@v2", asset: "Bar-v2.zip" }), item({ addon: "Bar", release_id: "gh:o/bar@v2", asset: "Bar-v2.zip" })],
      { routine: false, reasons: ["while_running"], scope: scopeOf({ while_running: true }) })
    const sheet = M.previewDecision(Object.assign(match([]), { targets: SHEET_TARGETS }), two).sheet
    expect([sheet.title, sheet.confirmLabel]).toEqual(["Update 3 addons on 2 clients?", "Update 3"])
    expect(sheet.items.map((i) => [i.header, i.text])).toEqual([["Classic Era", "Foo v1 → v2"], ["", "Bar v1 → v2"], ["Retail", "Bar v1 → v2"]])
    expect(sheet.effects.map((e) => e.text)).toEqual(["Classic Era is running: then /reload in game."])
    // One client: no header, its name in the title.
    const one = M.previewDecision(Object.assign(match([]), { targets: SHEET_TARGETS }), previewOf([item(), item({ addon: "Bar" })])).sheet
    expect([one.title, one.items.map((i) => i.header)]).toEqual(["Update 2 addons on Retail?", ["", ""]])
  })

  test("the item list has no cap", () => {
    const forty = Array.from({ length: 40 }, (_, i) => item({ addon: "Addon" + String(i).padStart(2, "0"), release_id: "gh:o/a" + i + "@v2" }))
    const sheet = M.previewDecision(match([]), previewOf(forty)).sheet
    expect([sheet.items.length, sheet.items[39].text, sheet.title]).toEqual([40, "Addon39 v1 → v2", "Update 40 addons on retail?"])
    expect(M.dialogText(sheet).split("\n").length).toBe(41)
  })

  test("a reinstall is a repair of files, and the costly effects come first", () => {
    const reinstall = previewOf([item({ to_version: "v1" })], { routine: false, reasons: ["reinstall", "local_changes_replaced"],
      scope: scopeOf({ local_changes_replaced: { count: 2, folders: ["Foo"] } }) }, null, {})
    reinstall.command = "install"
    const sheet = M.previewDecision({ mode: "confirm", path: "install", shown: [] }, reinstall).sheet
    expect([sheet.title, sheet.confirmLabel, sheet.destructive]).toEqual(["Repair Foo on retail?", "Repair Foo", false])
    expect(sheet.effects.map((e) => [e.severity, e.text])).toEqual([
      ["urgent", "Replaces 2 files you changed in Foo. Undo brings them back."],
      ["normal", "Puts back Foo's files exactly as released."]])
  })

  test("a take-over lists what it adopts and says nothing moves, over the binary's preview (1n2p.32)", () => {
    const env = fixture("adopt-preview")
    const targets = M.reduceStatus(fixture("status-adoptable")).targets
    const decision = M.previewDecision({ mode: "confirm", path: "adopt", shown: [], targets }, env)
    expect(decision.kind).toBe("dialog")
    const sheet = decision.sheet
    expect([sheet.title, sheet.confirmLabel, sheet.destructive]).toEqual(["Take over 2 addons on Retail?", "Take over 2", false])
    expect(sheet.items.map((i) => [i.op, i.addon])).toEqual([["adopt", "Bar"], ["adopt", "Foo"]])
    expect(sheet.effects.slice(0, 2).map((e) => e.text)).toEqual([
      "Nothing is reinstalled or moved: omaddons starts keeping track of these folders as they are, and can undo that.",
      "Folders nothing identifies stay as they are, unmanaged."])
    // Its Yes is the stored plan's apply.
    expect(decision.plan.argv).toEqual(["omaddons", "apply", env.data.plan_id])
    // The take-over is a confirmed preview, never run without its sheet.
    expect(reasonOf(() => M.buildArgv("adopt_target", { target: "retail" }, opts()))).toBe("preview_required")
    expect(M.buildArgv("adopt_target", { target: "retail" }, opts({ preview: true })).argv.slice(-5))
      .toEqual(["--dry-run", "adopt", "--apply", "-t", "retail"])
  })

  test("a removal another addon needs says so, by the warning's code alone", () => {
    const needed = JSON.parse(JSON.stringify(removal))
    needed.warnings = [{ code: "still_required", message: "LittleWigs still requires Baz", fix: null, fix_argv: null }]
    const sheet = M.previewDecision({ mode: "confirm", path: "remove", shown: [] }, needed).sheet
    expect(sheet.effects.map((e) => [e.kind, e.severity, e.text])).toEqual([
      ["folders_removed", "urgent", "Removes the folder Baz. Undo brings it back."],
      ["still_required", "urgent", "Other addons still need Baz and won't load without it."]])
    expect(M.dialogText(sheet)).not.toContain("LittleWigs")
  })

  test("dialogText writes the sheet a line each", () => {
    expect(M.dialogText(M.previewDecision(match([shownItem()]), withDependency)))
      .toBe("Update Foo on retail?\nFoo v1 → v2\nAlso installs LibFoo 1.0, which Foo needs.")
    expect(M.dialogText(null)).toBe("")
    expect(M.dialogText({ kind: "refuse", sheet: null })).toBe("")
    // The banner of a preview made again, under the title, unless the sheet says what changed.
    const sheet = M.previewDecision(match([]), previewOf([item()])).sheet
    expect(M.dialogText(M.withStale(sheet, "Changed since you looked: it already ran. Nothing ran; here is what would happen now.")).split("\n")[1])
      .toBe("Changed since you looked: it already ran. Nothing ran; here is what would happen now.")
    const changed = M.previewDecision(match([shownItem()]), previewOf([item({ to_version: "v3", release_id: "gh:o/foo@v3", asset: "Foo-v3.zip" })])).sheet
    expect(M.withStale(changed, "another banner").stale).toBe("Changed since you looked: Foo v3 is now the newest")
  })

  test("no runtime file uses qs.Ui's ConfirmDialog: every confirmation is the plan sheet", () => {
    const root = path.join(__dirname, "..")
    const runtime = ["PanelView.qml", "Service.qml", "BarWidget.qml", "RestoreChooser.qml"]
      .concat(fs.readdirSync(path.join(root, "views")).map((f) => "views/" + f), fs.readdirSync(path.join(root, "ui")).map((f) => "ui/" + f))
    for (const name of runtime) expect([name, /\bConfirmDialog\b/.test(fs.readFileSync(path.join(root, name), "utf8"))]).toEqual([name, false])
    expect(fs.readFileSync(path.join(root, "PanelView.qml"), "utf8")).toContain("PlanSheet {")
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
    // The banner over the new preview: what changed, in the player's words (design doc
    // §6.5.2); never "plan" or "preview" (§6.3).
    const banner = (what) => "Changed since you looked: " + what + ". Nothing ran; here is what would happen now."
    const cases = [
      ["reason_expired", refusal("plan_stale", { reason: "expired" }), banner("that was more than a day ago")],
      ["reason_binary_changed", refusal("plan_stale", { reason: "binary_changed" }), banner("omaddons was updated since")],
      ["reason_input_missing", refusal("plan_stale", { reason: "input_missing", path: "/x" }), banner("the file it installs from is gone")],
      ["reason_already_applied", refusal("plan_stale", { reason: "already_applied", txs: [] }), banner("it already ran")],
      ["changed", refusal("plan_stale", { changed: ["selection", "observed"] }),
        banner("a newer version came out, or the change is different now; files it would change were changed")],
      // A part this build does not know is no word to show.
      ["changed_unknown_part", refusal("plan_stale", { changed: ["Baz"] }), banner("something it depends on")],
      ["plan_not_found", refusal("plan_not_found", { prefix: PLAN_ID }), banner("what you saw was cleared")],
      ["outcome_stale", applied(fixture("update"), someStale, { partial: true }),
        "Changed since you looked: another change landed on retail. Nothing ran there; here is what would happen now."],
      ["bound_unchanged", fixture("remove-stale"), banner("the command is different; another change landed on a client; "
        + "a newer version came out, or the change is different now; files it would change were changed")],
    ]
    for (const [name, env, expected] of cases) {
      expect([name, M.staleText(env)]).toEqual([name, expected])
      expect([name, /\b(plan|preview|token)\b/i.test(M.staleText(env))]).toEqual([name, false])
    }
    // The clients by their player names.
    expect(M.staleText(applied(fixture("update"), someStale, { partial: true }), [{ id: "retail", key: KEY_RETAIL, displayName: "Retail" }]))
      .toBe("Changed since you looked: another change landed on Retail. Nothing ran there; here is what would happen now.")
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
    const installed = M.installedRows(lists(), "retail").find((r) => r.name === "Baz")
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

// A purge (`targets rm --purge`) deletes omaddons' backups and undo snapshots for good:
// the widget never runs one, not even behind a Yes (1n2p.43).
describe("an irreversible purge is never a button", () => {
  const purge = ["omaddons", "targets", "rm", "classic", "--purge"]

  test("the classifier refuses --purge before --, in every position", () => {
    table([
      ["after the id", purge, "irreversible"],
      ["after the path", ["omaddons", "targets", "rm", "--purge", "classic"], "irreversible"],
      ["before the path", ["omaddons", "--purge", "targets", "rm", "classic"], "irreversible"],
      ["with a value", ["omaddons", "targets", "rm", "classic", "--purge=yes"], "irreversible"],
      ["with --dry-run", ["omaddons", "targets", "rm", "classic", "--purge", "--dry-run"], "irreversible"],
      ["after --, an operand", ["omaddons", "remove", "-t", "retail", "--", "--purge"], ""],
      ["plain targets rm", ["omaddons", "targets", "rm", "classic"], ""],
    ], (argv) => M.classifyCliArgv(argv).reason)
    for (const confirmed of [{ confirmed: true }, { preview: true }]) {
      expect(reasonOf(() => M.cliArgv(purge, opts(confirmed)))).toBe("irreversible")
    }
  })

  test("no template holds a purge", () => {
    for (const [name, template] of Object.entries(M.COMMANDS)) {
      const purges = template.argv.filter((arg) => M.IRREVERSIBLE_FLAGS.includes(arg.split("=")[0]))
      expect([name, purges]).toEqual([name, []])
    }
  })

  // Hand-built: the binary's status-attention envelope with its first issue's remediation
  // replaced by a purge, until a fixture home with an uninstalled client writes the real
  // one (the purge itself is omad-launch-week-fixes-bmcx.2).
  test("a purge remediation is text with what it deletes, to copy", () => {
    const env = fixture("status-attention")
    const plain = ["omaddons", "targets", "rm", "retail"]
    env.data.targets[0].issues = [
      { ...env.data.targets[0].issues[0], "remediation": { "command": purge.join(" "), "argv": purge, "destructive": true } },
      { ...env.data.targets[0].issues[0], "remediation": { "command": plain.join(" "), "argv": plain, "destructive": true } },
    ]
    const status = M.reduceStatus(env)
    const [purging, forgetting] = status.targets[0].issues.map((i) => i.remediation)
    expect([purging.runnable, purging.argv, purging.irreversible]).toEqual([false, null, true])
    expect([forgetting.runnable, forgetting.destructive, forgetting.irreversible]).toEqual([true, true, false])
    const rows = M.issueRows(M.issueSections(status.targets, [], "", []))
    const purgeRow = rows.find((row) => row.text === purge.join(" ")) // ubs:ignore — a command's text, not a secret
    expect([purgeRow.action, purgeRow.note]).toEqual([null, M.IRREVERSIBLE_NOTE])
    expect(M.copyText(purgeRow)).toBe(purge.join(" "))
    expect(M.copyArgv(M.copyText(purgeRow))).toEqual(["wl-copy", "--", purge.join(" ")])
    const plainRow = rows.find((row) => row.action && row.action.argv.join(" ") === plain.join(" ")) // ubs:ignore — a command's text, not a secret
    expect([plainRow.note, M.copyText(plainRow)]).toEqual(["", ""])
  })

  test("the clipboard gets one argument, never a shell", () => {
    table([
      ["a command", "omaddons targets rm classic --purge", ["wl-copy", "--", "omaddons targets rm classic --purge"]],
      ["shell characters stay text", "a; rm -rf ~ | x", ["wl-copy", "--", "a; rm -rf ~ | x"]],
      ["nothing", "", null],
      ["a NUL", "a\u0000b", null],
      ["not a string", 7, null],
    ], (text) => M.copyArgv(text))
    expect(M.panelKey("y", "issues")).toEqual({ "action": "copy", "step": 0 })
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
  const texts = (d) => d.sheet.effects.map((e) => e.text)
  const after = ". Nothing ran; here is what would happen now."

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
    expect(["one row fewer", fewer.kind, fewer.heading, fewer.sheet.stale, fewer.sheet.title]).toEqual(["one row fewer", "dialog",
      "Changed since you looked: Also: Update Bar v1 → v2 on era (not in the view)",
      "Changed since you looked: Also: Update Bar v1 → v2 on era (not in the view)", "Update 3 addons on 2 clients?"])
    // Every item, each client's under its name.
    expect(fewer.sheet.items.map((i) => [i.header, i.text])).toEqual([["era", "Bar v1 → v2"], ["", "Foo v1 → v2"], ["retail", "Bar v1 → v2"]])
  })

  test("a moved release opens the dialog as changed", () => {
    const d = match("plan-moved-list", "plan-update-moved")
    expect(["plan-update-moved", d.kind, d.sheet.stale, d.sheet.items.map((i) => i.text)])
      .toEqual(["plan-update-moved", "dialog", "Changed since you looked: Foo v3 is now the newest", ["Foo v1 → v3"]])
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
    expect(["plan-update-dependency", d.kind, d.heading, d.sheet.items.map((i) => i.text), texts(d)])
      .toEqual(["plan-update-dependency", "dialog", "", ["Foo v1 → v2"], ["Also installs LibFoo 1.0, which Foo needs."]])
  })

  test("local edits replaced open the dialog", () => {
    const d = match("plan-local-edits-list", "plan-update-local-edits")
    expect(["plan-update-local-edits", d.kind, d.heading, texts(d)])
      .toEqual(["plan-update-local-edits", "dialog", "", ["Replaces 1 file you changed in Foo. Undo brings it back."]])
  })

  test("extra removals open the dialog", () => {
    const d = match("plan-removal-list", "plan-update-removal")
    expect(["plan-update-removal", d.kind, d.heading, texts(d)])
      .toEqual(["plan-update-removal", "dialog", "", ["Removes the folder Foo_Options. Undo brings it back."]])
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
      ["plan-apply-stale", "plan-apply-stale", [true, "Changed since you looked: a newer version came out, or the change is different now" + after]],
      ["plan-apply-expired", "plan-apply-expired", [true, "Changed since you looked: that was more than a day ago" + after]],
      ["plan-apply-not-found", "plan-apply-not-found", [true, "Changed since you looked: what you saw was cleared" + after]],
    ], (name) => [M.needsPreviewAgain(fixture(name)), M.staleText(fixture(name))])
  })

  test("a removal and an undo confirm through apply", () => {
    const removal = decide({ mode: "confirm", shown: [], path: "remove" }, "plan-remove")
    expect(["plan-remove", removal.kind, M.dialogText(removal), removal.sheet.confirmLabel, removal.sheet.destructive, removal.plan.destructive])
      .toEqual(["plan-remove", "dialog", "Remove Bar from retail?\nBar v1 (1 folder)\nRemoves the folder Bar. Undo brings it back.",
        "Remove Bar", true, true])
    const undo = decide({ mode: "confirm", shown: [], path: "undo" }, "plan-undo")
    expect(["plan-undo", undo.kind, undo.sheet.title.indexOf("Undo the update on era and retail from ") === 0, undo.sheet.confirmLabel,
      undo.sheet.items.map((i) => i.text), undo.sheet.destructive, undo.plan.destructive])
      .toEqual(["plan-undo", "dialog", true, "Undo", ["Bar back as it was", "Foo back as it was"], true, true])
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

// --------------------------------------------------- repairs from the binary (G2)

// The doctor's repairs as the real binary answered the widget's own argv (step 1's
// repair_fixtures): the credential repair previewed, bound, undone through its stored
// plan, gone stale; a set-aside row and its explanation. A renamed key in the doctor's
// report, the undo's data or status's issues fails here.
describe("repairs from the binary", () => {
  // ubs:ignore — failure-mode ids, not secrets
  const CREDENTIAL = "fm-secrets-credential-file-mode"
  const SET_ASIDE = "fm-concurrency_primitives-journal-too-new"
  const context = fixture("repair-context")
  const targets = M.reduceStatus(fixture("repair-status")).targets
  const credentialRow = () => {
    const status = M.reduceStatus(fixture("repair-status"))
    const rows = M.issueRows(M.issueSections(status.targets, [], "", status.issues))
    return rows.find((r) => r.kind === "problem" && r.fix && r.fix.slots.fm === CREDENTIAL) // ubs:ignore — a failure-mode id, not a secret
  }
  const rawIssue = () => fixture("repair-status").data.issues.find((i) => i.id === CREDENTIAL)
  const setAsideIssue = () => fixture("setaside-status").data.targets.find((t) => t.id === "retail").issues.find((i) => i.id === SET_ASIDE)

  test("the credential issue reduces to a Fix", () => {
    const row = credentialRow()
    expect(row && { fix: row.fix, action: row.action, hint: row.hint, forced: row.forced })
      .toEqual({ fix: { template: "doctor_fix", slots: { fm: CREDENTIAL }, label: "Repair: " + rawIssue().title }, action: null,
        hint: "Enter previews the repair", forced: false })
    expect(M.classifyCliArgv(rawIssue().remediation.argv).reason).toBe("own_flow_only")
    expect(row.explain.runnable).toBe(true)
  })

  test("the argv bun builds is the one the scene ran", () => {
    const fix = credentialRow().fix
    const ran = fixture("repair-argv")
    const build = (extra) => M.buildArgv(fix.template, fix.slots, opts(Object.assign({ label: "Fixture" }, extra))).argv.slice(1)
    expect(build({ preview: true })).toEqual(ran.preview)
    expect(build({ token: fixture("repair-preview").precondition })).toEqual(ran.bound)
  })

  test("the preview is a dialog with the chmod line", () => {
    const fix = credentialRow().fix
    const decision = M.doctorPreviewDecision(fix, fixture("repair-preview"), context, targets)
    const sheet = decision.sheet
    expect([decision.kind, sheet.title, sheet.items.map((i) => i.text), sheet.effects.map((e) => e.text), sheet.confirmLabel, sheet.destructive,
      decision.token]).toEqual(["dialog", fix.label + "?", ["chmod 0600 <config>/credentials/github (was 0644)"],
      ["Undo last reverts it."], "Repair", false, fixture("repair-preview").precondition])
  })

  test("the run is a repair the footer can undo", () => {
    const run = fixture("repair-run")
    const result = M.doctorResult(run)
    expect([result.repaired, result.undoable, result.report]).toEqual([true, true, true])
    expect(M.resultLine(run, context, targets)).toBe("Repaired: chmod 0600 <config>/credentials/github (was 0644)")
    // Its own undo hint is the doctor's flow, never a button: the Undo is Undo last's.
    expect(M.classifyCliArgv(run.undo_argv).reason).toBe("own_flow_only")
    expect(run.commands.map((c) => M.classifyCliArgv(c.argv).reason)).toEqual(["own_flow_only"])
  })

  test("status after names the repair", () => {
    const change = M.reduceStatus(fixture("repair-status-after")).lastChange
    const argv = fixture("repair-status-after").data.last_change.undo.argv
    expect([change.kind, change.label, change.undo.runnable, change.undo.destructive]).toEqual(["repair", "Undo repair", true, true])
    expect(M.resultUndo({ repair: true, statusSeen: true }, change).argv).toEqual(argv)
    expect(argv.slice(0, 3)).toEqual(["omaddons", "undo", "--run"])
    expect(M.resultUndo({ repair: true, statusSeen: false }, change)).toBeNull()
  })

  test("the undo preview confirms through its plan", () => {
    const env = fixture("repair-undo-preview")
    const decision = M.previewDecision({ mode: "confirm", shown: [], again: false, path: "undo", context, targets }, env)
    expect([decision.kind, decision.plan.planId]).toEqual(["dialog", env.data.plan_id])
    expect([decision.sheet.title.indexOf("Undo the repair from ") === 0, decision.sheet.items.map((i) => i.text), decision.sheet.confirmLabel,
      decision.sheet.destructive]).toEqual([true, ["chmod 0644 <config>/credentials/github (readable by other users again)"], "Undo", true])
  })

  test("the undo's apply reads as an undo", () => {
    expect(M.resultLine(M.applyResult(fixture("repair-undo-apply")))).toBe("Undid repair (1 change)")
    expect(M.needsPreviewAgain(fixture("repair-undo-apply"))).toBe(false)
  })

  test("a stale bound run previews again", () => {
    const stale = fixture("repair-stale")
    expect([stale.error.code, M.doctorResult(stale).error.code]).toEqual(["plan_stale", "plan_stale"])
    expect(M.staleText(stale)).toBe("Changed since you looked: the files this repair checks. Nothing ran; here is what would happen now.")
  })

  test("the set-aside row is text with its explanation", () => {
    const status = M.reduceStatus(fixture("setaside-status"))
    const row = M.issueRows(M.issueSections(status.targets, [], "", status.issues))
      .find((r) => r.kind === "problem" && r.severity === "P0")
    expect(row && { fix: row.fix, action: row.action, forced: row.forced, text: row.text, hint: row.hint })
      .toEqual({ fix: null, action: null, forced: true, text: setAsideIssue().remediation.command, hint: "Enter explains it" })
    expect(M.classifyCliArgv(setAsideIssue().remediation.argv).reason).toBe("consent_flag")
    const explain = fixture("setaside-explain")
    const lines = M.explainLines(explain, context, M.reduceStatus(fixture("setaside-status")).targets)
    expect(lines[0]).toBe(explain.failure_mode.title)
    expect(lines).toContain("Its repair needs your deliberate consent: read this, then run the command below in a terminal")
    expect(lines).toContain("  In a terminal: " + setAsideIssue().remediation.command)
    // No path below a WTF/ (the failure mode's prose may name the folder itself).
    expect(lines.filter((l) => /WTF\/[^\s]/.test(l))).toEqual([])
    // The journal's path reads below omaddons' own state folder.
    expect(lines.some((l) => l.startsWith("  journal: <state>/journal/"))).toBe(true)
  })

  test("a doctor gc remediation is text", () => {
    // Derived: a gc row needs over 200 fix runs to appear, and only the verdict is tested.
    const env = fixture("repair-status")
    const gc = ["omaddons", "doctor", "gc", "--before", "30d", "--dry-run"]
    env.data.issues[0].remediation = { command: gc.join(" "), argv: gc, destructive: true }
    env.data.issues[0].auto_fixable = false
    const status = M.reduceStatus(env)
    const row = M.issueRows(M.issueSections(status.targets, [], "", status.issues))[0]
    expect([row.text, row.action, row.fix, M.classifyCliArgv(gc).reason]).toEqual([gc.join(" "), null, null, "doctor_gc"])
  })
})

// --------------------------------------------------- the addon page (1n2p.23)

// The page over what the binary wrote: `list -v` (Foo on era and retail, one release
// behind), `info --changelog` of Foo on retail (its about, TOC and notes), `errors`
// (Foo's errors on retail) and `status`'s clients.
describe("the addon page", () => {
  const status = () => M.reduceStatus(fixture("status-updates"), NOW)
  const page = (extra) => M.addonPage(Object.assign({ name: "Foo", ref: "", lists: M.reduceList(fixture("list")),
    info: M.reduceInfo(fixture("changelog")), errors: M.reduceErrors(fixture("errors")), statusTargets: status().targets, nowMs: NOW }, extra || {}))

  test("info reduces to what the page reads, every link checked", () => {
    const info = M.reduceInfo(fixture("changelog"))
    expect([info.name, info.ref, info.about.homepage, info.aboutWhy, info.latest.version]).toEqual(["Foo", "gh:o/foo", "https://github.com/o/foo", "fetch_failed", "v2"])
    expect(info.installed.map((r) => [r.target, r.version, r.folders, r.toc.title])).toEqual([["retail", "v1", ["Foo"], "edited by hand"]])
    expect([info.changelog.entries.map((e) => e.version), info.changelog.moreUrl]).toEqual([["v2"], "https://github.com/o/foo/releases"])
    // A page link that is not https on a known host is no link.
    const env = fixture("changelog")
    env.data.about.homepage = "http://example.com/foo"
    env.data.changelog.more_url = "https://example.com/releases"
    const unsafe = M.reduceInfo(env)
    expect([unsafe.about.homepage, unsafe.changelog.moreUrl]).toEqual(["", ""])
    expect(M.reduceInfo(fixture("failure"))).toBeNull()
  })

  test("the page: what it is, each client, its notes, folders and errors", () => {
    const p = page()
    expect([p.title, p.name, p.ref, p.source, p.homepage, p.state, p.why])
      .toEqual(["edited by hand", "Foo", "gh:o/foo", "GitHub", "https://github.com/o/foo", "", "Its source didn't answer."])
    // Where it is installed first, each as an Addons row, so its actions run as there.
    expect(p.clients.filter((c) => c.installed).map((c) => [c.client, c.meta, c.primary, c.actions.map((a) => a.name)])).toEqual([
      ["Classic Era", "v1 installed · v2 available", "update", ["update", "pin", "restore", "remove"]],
      ["Retail", "v1 installed · v2 available", "update", ["update", "pin", "restore", "remove"]],
    ])
    expect(M.rowAction(p.clients[1].row, "update")).toMatchObject({ template: "update_addon", slots: { target: "retail", addon: "Foo" } })
    expect(p.clients[0].actions[0].tooltip).toBe("Update (u)")
    expect(p.notes.map((n) => [n.version, n.text])).toEqual([["v2", "Fixes the pull timer."]])
    expect(p.folders.map((f) => [f.client, f.folders])).toEqual([["Classic Era", ["Foo"]], ["Retail", ["Foo"]]])
    expect(p.errors.map((e) => [e.key, e.text])).toEqual([["a/retail/Foo", "5 errors on Retail · 2 new"]])
  })

  test("before its read answers, the local facts; a failed read says so", () => {
    const reading = page({ info: null, reading: true })
    expect([reading.title, reading.ref, reading.state, reading.notes, reading.clients.filter((c) => c.installed).length])
      .toEqual(["Foo", "gh:o/foo", "reading", [], 2])
    const failed = page({ info: null, failed: "omaddons info: network" })
    expect([failed.state, failed.failed]).toEqual(["failed", "omaddons info: network"])
  })

  // A page opened from a ref or link pasted into Search (1n2p.25): until its read answers
  // it names nothing and offers no install; then its name and the ref an install takes
  // are the read's, never the text typed (M3-D2).
  test("a pasted link's page takes its name and ref from its read, never from the text", () => {
    const link = "https://github.com/o/foo/releases/tag/v1"
    const before = page({ name: "", link, info: null, reading: true })
    expect([before.title, before.name, before.ref, before.link, before.state]).toEqual([link, "", "", link, "reading"])
    expect(before.clients.filter((c) => c.installed || c.primary === "install")).toEqual([])
    const after = page({ name: "", link })
    expect([after.title, after.name, after.ref, after.link]).toEqual(["edited by hand", "Foo", "gh:o/foo", link])
    expect(after.clients.filter((c) => c.installed).map((c) => c.client)).toEqual(["Classic Era", "Retail"])
    // A read that failed (a link the CLI refuses) names nothing either.
    const failed = page({ name: "", link: "url:https://example.com/x.zip", info: null, failed: "host_blocked" })
    expect([failed.name, failed.ref, failed.state]).toEqual(["", "", "failed"])
  })

  test("an addon installed nowhere: each client's likelihood, and Install here where it likely loads", () => {
    const info = M.reduceInfo(fixture("changelog"))
    info.installed = []
    info.latest.compat = { "retail": "likely", "era": "unlikely" }
    const p = page({ name: "Skada", ref: "gh:skada/skada", info: info, errors: null })
    expect(p.clients.map((c) => [c.client, c.meta, c.primary])).toEqual([
      ["Classic Era", "not installed · not made for this client", ""],
      ["Retail", "not installed · probably works here", "install"],
    ])
    expect(p.clients[1].actions.map((a) => [a.name, a.tooltip])).toEqual([["install", "Install here (i)"]])
  })

  test("what it needs and what needs it, each a page of its own", () => {
    const lists = M.reduceList(fixture("list"))
    lists.targets[1].addons.find((a) => a.name === "Foo").requiredBy = ["Bar"]
    const info = M.reduceInfo(fixture("changelog"))
    info.installed[0].toc.dependencies = ["baz", "LibNowhere"]
    const p = page({ lists: lists, info: info })
    expect([p.needs, p.neededBy]).toEqual([[{ name: "Baz", installed: true }, { name: "LibNowhere", installed: false }], ["Bar"]])
  })

  test("the page's read is info of its ref, on the network lane", () => {
    const built = M.buildArgv("addon_info", { ref: "gh:o/foo" }, opts({ label: "About gh:o/foo" })).argv
    expect(built.slice(built.indexOf("info"))).toEqual(["info", "--changelog", "--", "gh:o/foo"])
    expect(built).toContain("--progress")
    expect(M.activityHeadline({ kind: "read", resource: "addon_info", label: "x" }, [])).toBe("Reading about the addon")
  })
})

// --------------------------------------------------------- History (1n2p.30)

describe("History", () => {
  // The fixture's scene (tests/omarchy_widget.rs history_fixtures), newest first: a
  // person's whole restore, the agent's removal of Foo on era, a setting changed, a
  // backup, the undo of a removal, that removal of Bar on retail (the agent's, with its
  // reason), an update of era and retail from the panel, and four installs.
  const history = () => M.reduceHistory(fixture("history"))
  const runs = () => M.historyRuns(history(), [])
  const byTitle = (title) => runs().find((r) => r.title === title)
  const byKind = (kind) => runs().find((r) => r.kind === kind)
  // Half an hour after the newest change.
  const soon = () => Date.parse(fixture("history").data.entries[0].at) + 30 * 60000

  test("the read names no actor, so it lists every change (an explicit --actor filters history)", () => {
    const argv = M.buildArgv("history", {}, opts()).argv
    expect(argv).not.toContain("--actor")
    expect(argv.slice(argv.indexOf("history"))).toEqual(["history", "-v", "--limit", String(M.HISTORY_LIMIT)])
    // Every other template still says who acts.
    expect(M.buildArgv("list", {}, opts()).argv.slice(1, 4)).toEqual(["--json", "--actor", "widget"])
    expect(() => M.templateShape("x", { x: { argv: ["update"], mutates: true, actor: false } })).toThrow(/names no actor but mutates/)
  })

  test("the envelope as History's model", () => {
    const h = history()
    expect([h.failed, h.truncated, h.skipped, h.entries.length]).toEqual([false, false, false, 12])
    expect(h.entries[2]).toMatchObject({ kind: "config", actor: "human", targets: [] })
    // A restore names the backup it put files back from (its `wtf` op's).
    expect(h.entries[0].kind).toBe("restore")
    expect(h.entries[0].restoredFrom).toMatch(M.RUN_PATTERN)
    expect(M.reduceHistory(fixture("failure")).failed).toBe(true)
    expect(M.reduceHistory(null).failed).toBe(false)
    expect(M.reduceHistory(fixture("history-empty")).entries).toEqual([])
    // Lines `history` could not read are said, never guessed at.
    const skipped = Object.assign({}, fixture("history"), { warnings: [{ code: "history_lines_skipped", message: "m", fix: null, fix_argv: null }] })
    expect(M.reduceHistory(skipped).skipped).toBe(true)
  })

  test("each run in words, with its undo state", () => {
    table([
      ["a_whole_restore", "restore", ["Restored the settings", "undoable", ""]],
      ["config_is_fixed", "config", ["Changed a setting", "fixed", "A settings change: undo doesn't revert it."]],
      ["a_backup_is_fixed", "backup", ["Backed up the settings", "fixed", "A backup is no change to undo."]],
      ["the_undo_stands_so_it_redoes", "undo", ["Undid the removal", "redo", ""]],
      ["the_update_of_two_clients", "update", ["Updated 2 addons", "undoable", ""]],
    ], (kind) => { const r = byKind(kind); return [r.title, r.state, r.why] })
    table([
      ["the_removal_undone_since", "Removed Bar", ["undone", "It was already undone."]],
      ["the_removal_in_effect", "Removed Foo", ["undoable", ""]],
    ], (title) => { const r = byTitle(title); return [r.state, r.why] })
    const update = byKind("update")
    expect([update.targets, update.items.length, update.actor]).toEqual([["era", "retail"], 4, "widget"])
    // Each install was a command of its own: four runs.
    expect(runs().filter((r) => r.kind === "install").map((r) => r.title).sort())
      .toEqual(["Installed Bar", "Installed Bar", "Installed Foo", "Installed Foo"])
  })

  test("an undo of an undo puts the change back in effect", () => {
    const line = (tx, kind, reverts) => ({ tx, run: tx + "R", kind, at: "2026-11-01T00:00:00Z", actor: "human", reason: null, reverts: reverts || null,
      plan: null, targets: [{ id: "retail", key: "K" }], items: [], restoredFrom: "" })
    const entries = [line("C", "undo", "B"), line("B", "undo", "A"), line("A", "update")]
    expect(M.historyStanding(entries)).toEqual({ C: true, B: false, A: true })
    const states = M.historyRuns({ entries }, []).map((r) => [r.tx, r.state])
    expect(states).toEqual([["C", "redo"], ["B", "undone"], ["A", "undoable"]])
    // A line from before runs: nothing to name, so undo it from a terminal.
    const old = M.historyRuns({ entries: [Object.assign(line("D", "update"), { run: null })] }, [])[0]
    expect([old.state, old.why, M.historyUndo(old, "")]).toEqual(["old", "Made by an older omaddons, before changes were grouped: undo it from a terminal.", null])
  })

  test("the rows: day headings, clients under All, the addons once, who and why", () => {
    const rows = M.historyRows(history(), "", [{ id: "retail", displayName: "Retail" }, { id: "era", displayName: "Classic Era" }], { nowMs: soon() })
    expect(rows[0].group).toBe("Today")
    expect(rows.slice(1).every((r) => r.group === "")).toBe(true)
    const update = rows.find((r) => r.kind === "update")
    expect(update.chips.map((c) => c.text)).toEqual(["Classic Era", "Retail"])
    // An update of two clients names each addon once.
    expect(update.detail).toBe("Bar v1 → v2 · Foo v1 → v2")
    // The panel's own reason (its button's label) says nothing more; an agent's is shown.
    expect(update.meta).toMatch(/^\d\d:\d\d · you, in the panel$/)
    const removal = rows.find((r) => r.title === "Removed Bar")
    expect(removal.meta).toMatch(/ · an agent · “Bar conflicts with ElvUI”$/)
    expect(removal.chips.map((c) => c.text)).toEqual(["Retail", "undone"])
    expect([removal.glyph, update.glyph]).toEqual([0xF06A9, 0xF02DA])
    // A whole restore names no addon: "every setting".
    expect(rows[0].detail).toBe("every setting")
    // Undo where it can: the cursored row's action; none on an undone or fixed one.
    expect(rows.map((r) => [r.title, r.actions.map((a) => a.name)]).slice(0, 7)).toEqual([
      ["Restored the settings", ["undo"]], ["Removed Foo", ["undo"]], ["Changed a setting", []], ["Backed up the settings", []],
      ["Undid the removal", ["undo"]], ["Removed Bar", []], ["Updated 2 addons", ["undo"]]])
  })

  test("the scope keeps its own changes, and the filter matches what a row says", () => {
    const era = M.historyRows(history(), "era", [], { nowMs: soon() })
    expect(era.map((r) => r.title)).toEqual(["Removed Foo", "Updated 2 addons", "Installed Bar", "Installed Foo"])
    // Under one client, no client chips.
    expect(era[0].chips).toEqual([])
    table([
      ["a_reason", "elvui", ["Removed Bar"]],
      ["who", "agent", ["Removed Foo", "Removed Bar", "Installed Bar", "Installed Bar", "Installed Foo", "Installed Foo"]],
      ["an_addon_and_a_word", "foo inst", ["Installed Foo", "Installed Foo"]],
      ["nothing", "zzz", []],
    ], (query) => M.historyRows(history(), "", [], { query, nowMs: soon() }).map((r) => r.title))
    expect(M.historyCount(history(), "", [], "elvui", soon())).toMatchObject({ shown: 1, total: 11, text: "1 of 11", none: false })
  })

  test("undo names the run, whole or on one client, and previews first", () => {
    const update = byKind("update")
    expect(M.historyUndo(update, "")).toEqual({ template: "undo_run", slots: { run: update.run }, label: "Undo: Updated 2 addons" })
    expect(M.historyUndo(update, "era")).toEqual({ template: "undo_run_target", slots: { run: update.run, target: "era" }, label: "Undo: Updated 2 addons" })
    expect(M.historyUndo(byKind("undo"), "").label).toBe("Redo: Undid the removal")
    expect([M.historyUndo(byTitle("Removed Bar"), ""), M.historyUndo(byKind("config"), "")]).toEqual([null, null])
    // Its real run is refused: Yes applies the preview's plan.
    expect(reasonOf(() => M.buildArgv("undo_run", { run: update.run }, opts()))).toBe("preview_required")
    const preview = M.buildArgv("undo_run", { run: update.run }, opts({ preview: true })).argv
    expect(preview.slice(preview.indexOf("--dry-run"))).toEqual(["--dry-run", "undo", "--run=" + update.run])
    // The CLI's preview, in the plan sheet: the update it reverts, on both clients.
    const targets = [{ id: "retail", displayName: "Retail" }, { id: "era", displayName: "Classic Era" }]
    expect(M.confirmSheet("undo", fixture("history-undo-preview"), CONTEXT, targets).title).toMatch(/^Undo the update on Classic Era and Retail from /)
    expect(M.confirmSheet("undo", fixture("history-undo-target-preview"), CONTEXT, targets).title).toMatch(/^Undo the update on Classic Era from /)
  })

  test("a change's page: each client's addons, and an undo for all of it and per client", () => {
    const update = byKind("update")
    const page = M.historyPage(history(), update.key, [{ id: "retail", displayName: "Retail" }, { id: "era", displayName: "Classic Era" }], soon())
    expect(page.header).toBe("Updated 2 addons · 30 min ago · you, in the panel")
    expect(page.lines.map((l) => [l.kind, l.text])).toEqual([
      ["client", "Classic Era"], ["item", "Updated Bar v1 → v2"], ["item", "Updated Foo v1 → v2"],
      ["client", "Retail"], ["item", "Updated Bar v1 → v2"], ["item", "Updated Foo v1 → v2"]])
    expect(page.items.map((i) => [i.index, i.addon, i.target])).toEqual([[0, "Bar", "era"], [1, "Foo", "era"], [2, "Bar", "retail"], [3, "Foo", "retail"]])
    expect(page.undos.map((u) => [u.text, u.request.template])).toEqual([
      ["Undo all of it", "undo_run"], ["Undo on Classic Era only", "undo_run_target"], ["Undo on Retail only", "undo_run_target"]])
    // One client: one undo, and the client's name only when it changed no addon.
    const removal = M.historyPage(history(), byTitle("Removed Bar").key, [], soon())
    expect([removal.lines.map((l) => l.text), removal.undos, removal.why]).toEqual([["Removed Bar v2"], [], "It was already undone."])
    expect(removal.notes).toEqual(["“Bar conflicts with ElvUI”"])
    // A whole restore: every setting, and the backup it came from, which its last line
    // shows in Backups.
    const restore = M.historyPage(history(), byKind("restore").key, [], soon())
    expect([restore.lines.map((l) => l.text), restore.undos.map((u) => u.text)]).toEqual([["Restored every setting"], ["Undo"]])
    expect(restore.backup).toEqual({ id: history().entries[0].restoredFrom, target: "retail" })
    const config = M.historyPage(history(), byKind("config").key, [], soon())
    expect([config.lines, config.undos.length, config.why]).toEqual([[], 0, "A settings change: undo doesn't revert it."])
    expect(M.historyPage(history(), "run:nosuch", [], soon())).toBeNull()
  })

  test("an addon's page lists its newest changes, each opening its change's page", () => {
    const page = M.addonPage({ name: "Bar", history: history(), statusTargets: [], nowMs: soon() })
    expect(page.changes.map((c) => c.text.replace(/ · 30 min ago/, ""))).toEqual([
      "Undid the removal", "Removed Bar · undone", "Updated 2 addons", "Installed Bar", "Installed Bar"])
    expect(page.changes[0].key).toBe(byKind("undo").key)
    // At most five, and none without History's read.
    expect(M.addonPage({ name: "Foo", history: history(), statusTargets: [], nowMs: soon() }).changes.length).toBe(4)
    expect(M.addonPage({ name: "Bar", statusTargets: [], nowMs: soon() }).changes).toEqual([])
  })

  test("times and days in a player's words (design doc §6.9)", () => {
    const at = (y, mo, d, h, mi) => new Date(y, mo - 1, d, h, mi).toISOString()
    const now = new Date(2026, 10, 10, 12, 0).getTime()
    table([
      ["just_now", at(2026, 11, 10, 12, 0), "just now"],
      ["a_minute", at(2026, 11, 10, 11, 59), "1 min ago"],
      ["minutes", at(2026, 11, 10, 11, 35), "25 min ago"],
      ["hours_today", at(2026, 11, 10, 8, 30), "3 h ago"],
      ["yesterday", at(2026, 11, 9, 21, 40), "yesterday 21:40"],
      ["this_week", at(2026, 11, 6, 18, 2), "Fri 18:02"],
      ["older", at(2026, 10, 9, 18, 2), "9 Oct"],
      ["another_year", at(2025, 12, 31, 18, 2), "31 Dec 2025"],
      ["no_time", "", ""],
    ], (iso) => M.whenText(iso, now))
    table([
      ["today", at(2026, 11, 10, 0, 1), "Today"],
      ["yesterday", at(2026, 11, 9, 23, 59), "Yesterday"],
      ["this_week", at(2026, 11, 6, 9, 0), "Friday"],
      ["older", at(2026, 10, 9, 9, 0), "9 October"],
      ["another_year", at(2025, 12, 31, 9, 0), "31 December 2025"],
    ], (iso) => M.dayText(iso, now))
  })

  test("the view's states: reading, failed, no changes, a filter on a client with none", () => {
    const base = { targets: [{ id: "retail", displayName: "Retail" }], summary: { targets: 1 }, reading: [], outcomes: {} }
    const state = (service, filter, rows) => M.viewState("history", { service: Object.assign({}, base, service), filter: filter || "", rows: rows || 0, allRows: rows || 0, nowMs: NOW })
    expect([state({ reading: ["history"] }).state, state({ reading: ["history"] }).sentence]).toEqual(["firstLoad", "Reading history…"])
    expect([state({ outcomes: { history: "failed" } }).state, state({ outcomes: { history: "failed" } }).sentence]).toEqual(["failed", "Couldn't read the history."])
    const empty = M.reduceHistory(fixture("history-empty"))
    expect(state({ history: empty }).sentence).toBe("No changes yet.")
    expect(state({ history: empty }, "retail").sentence).toBe("No changes on Retail yet.")
    // Lines it could not read, and a list cut at its limit, are notices over the rows.
    const cut = Object.assign({}, history(), { skipped: true, truncated: true })
    expect(state({ history: cut }, "", 9).notices.map((n) => n.sentence)).toEqual([
      "Some history can't be read and isn't listed.", "Only the newest 100 changes are listed."])
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
    const bound = M.COMMANDS[name].preview === "bind"
    const extra = M.COMMANDS[name].preview ? { preview: true } : { token: runOpts(name).token }
    record(name, base, extra)
    // A bound preview (G2) is recorded bound too: the same argv with its token.
    if (bound) record(name, base, { token: TOKEN })
    if (M.COMMANDS[name].preview) realForms.push([name, reasonOf(() => M.buildArgv(name, base, opts()))])
    for (const slot of M.templateShape(name).slots) {
      // A hostile failure-mode id is refused (`bad_fm`, buildArgv's table above), and so
      // is a hostile run id (`bad_run`); a hostile client folder too, unless it is
      // absolute: those are parsed by clap.
      if (slot === "target" || slot === "fm" || slot === "run") continue
      if (slot === "path") {
        for (const value of HOSTILE) record(name, Object.assign({}, base, { path: "/" + value }), extra)
        continue
      }
      for (const value of HOSTILE) record(name, Object.assign({}, base, { [slot]: value }), extra)
    }
  }
  expect(refused).toEqual([])
  expect(realForms.sort((a, b) => (a[0] < b[0] ? -1 : 1))).toEqual(PREVIEWED.concat(BOUND_PREVIEWS).sort()
    .map((name) => [name, BOUND_PREVIEWS.includes(name) ? "token_required" : "preview_required"]))

  const classified = []
  const real = [
    M.reduceStatus(fixture("status-attention")).targets[0].issues[0].remediation.argv,
    fixture("status-after-update").data.last_change.undo.argv,
    fixture("failure").error.fix_argv,
    fixture("remove-stale").error.fix_argv,
    // The doctor's own argv, which the widget refuses (G2): a repair's remediation and its
    // run's undo hint (`own_flow_only`), and a set-aside's forced one (`consent_flag`).
    fixture("repair-status").data.issues[0].remediation.argv,
    fixture("repair-run").undo_argv,
    fixture("setaside-status").data.targets.find((t) => t.id === "retail").issues[0].remediation.argv,
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

// ---------------------------------------------------------------- the doctor's repairs (G2)

// ubs:ignore — a failure-mode id (the doctor's name for an exposed token file), not a secret
const CREDENTIAL_FM = "fm-secrets-credential-file-mode"
const CREDENTIAL_PATH = CONTEXT.XDG_CONFIG_HOME + "/omaddons/credentials/github"

// A doctor envelope (M4-D34): the report's keys at the top level, no `data`.
function doctorEnvelope(command, exitCode, report) {
  return Object.assign({ ok: exitCode === 0, schema_version: "1.1", tool_version: "0.1.0", command: command, exit_code: exitCode },
    report, { warnings: [], commands: [] })
}

// One planned chmod of the credential (E6's ActionPlanned, flattened primitive).
function chmodAction(extra) {
  return Object.assign({ seq: 0, fix: 0, fm: CREDENTIAL_FM, op: "chmod", path: CREDENTIAL_PATH, mode: "0600",
    pre: { kind: "file", mode: "0644" }, post: { kind: "file", mode: "0600" }, guards: [], bytes: 0 }, extra || {})
}

// A `doctor fix` report: a dry run when `dryRun`, its actions planned, else taken.
function fixEnvelope(o) {
  const dryRun = o.dryRun === true
  const state = o.state || (dryRun ? "DONE_FINDINGS" : "DONE_OK")
  const exit = o.exit !== undefined ? o.exit : ({ DONE_OK: 0, DONE_FINDINGS: 1, DONE_PARTIAL: 2, DONE_FAILED: 3 })[state]
  const actions = o.actions || [chmodAction()]
  const report = {
    doctor_contract_version: "1.0", run_id: "2026-10-31T07-23-32Z__01M68DF7000000000000000000", run_dir: "/r",
    started_at: "2026-10-31T07:23:32Z", finished_at: "2026-10-31T07:23:33Z", state: state, dry_run: dryRun,
    tx: dryRun ? null : "01M68DFDPC0000000000000000",
    summary: { total_findings: 1, findings_before: 1, findings_after: o.findingsAfter === undefined ? (dryRun ? null : 0) : o.findingsAfter,
      actions_taken: dryRun ? null : (o.taken !== undefined ? o.taken : actions.length), actions_failed: dryRun ? null : 0,
      bytes_to_back_up: o.bytes || 0, bytes_backed_up: dryRun ? null : 0, backups_restored: dryRun ? null : 0 },
    findings: [], skipped: [],
    actions: dryRun ? null : actions.map((a) => Object.assign({}, a, { ok: true, bytes: undefined })),
    delegated: dryRun ? null : [],
    prunes: dryRun ? null : (o.prunes || []),
    actions_planned: dryRun ? actions : null,
    delegated_planned: dryRun ? (o.delegated || []) : null,
    prunes_planned: dryRun ? (o.prunes || []) : null,
    precondition: dryRun ? (o.token === undefined ? TOKEN : o.token) : null,
    undo_command: dryRun ? null : "omaddons doctor undo r", undo_argv: dryRun ? null : ["omaddons", "doctor", "undo", "r"],
  }
  if (o.error) report.error = o.error
  return doctorEnvelope("doctor fix", exit, report)
}

describe("the fm slot", () => {
  test("the table", () => {
    table([
      ["credential", CREDENTIAL_FM, CREDENTIAL_FM],
      ["underscore_subsystem", "fm-concurrency_primitives-journal-too-new", "fm-concurrency_primitives-journal-too-new"],
      ["a_list", "fm-a,fm-b", "bad_fm"],
      ["a_flag", "--force", "bad_fm"],
      ["upper_case", "fm-Secrets", "bad_fm"],
      ["a_subsystem_name", "concurrency_primitives", "bad_fm"],
    ], (fm) => {
      const reason = reasonOf(() => M.buildArgv("doctor_fix", { fm: fm }, opts({ preview: true })))
      if (reason) return reason
      const argv = M.buildArgv("doctor_fix", { fm: fm }, opts({ preview: true })).argv
      expect(argv[argv.length - 1]).toBe("--only=" + fm)
      return fm
    })
  })
})

describe("the doctor templates", () => {
  const flags = [BINARY, "--json", "--actor", "widget", "--reason", "Test (bar widget)", "--progress", "json"]
  test("the table", () => {
    table([
      ["machine_preview", ["doctor_fix", { fm: CREDENTIAL_FM }, { preview: true }],
        flags.concat(["--dry-run", "doctor", "fix", "--only=" + CREDENTIAL_FM])],
      ["client_preview", ["doctor_fix_target", { target: "retail", fm: CREDENTIAL_FM }, { preview: true }],
        flags.concat(["--dry-run", "doctor", "fix", "-t", "retail", "--only=" + CREDENTIAL_FM])],
      ["bound", ["doctor_fix", { fm: CREDENTIAL_FM }, { token: TOKEN }],
        flags.concat(["--if-match", TOKEN, "doctor", "fix", "--only=" + CREDENTIAL_FM])],
      ["real_without_a_token", ["doctor_fix", { fm: CREDENTIAL_FM }, {}], "token_required"],
      ["preview_with_a_token", ["doctor_fix", { fm: CREDENTIAL_FM }, { preview: true, token: TOKEN }], "token_in_preview"],
    ], ([name, slots, extra]) => reasonOf(() => M.buildArgv(name, slots, opts(extra))) || M.buildArgv(name, slots, opts(extra)).argv)
  })
})

// A status envelope with these issues on retail, and these of no client.
function statusWithIssues(issues, machine) {
  return { ok: true, schema_version: "1.1", tool_version: "0.1.0", command: "status", exit_code: 0, partial: false,
    data: { summary: { state: "attention", targets: 1, running: [], updates: { available: 0, targets: [] }, issues: { p0: 0, p1: 0, p2: 1, p3: 0 } },
      targets: [{ id: "retail", key: "k", display_name: "Retail", issues: issues }], issues: machine || [] },
    warnings: [], commands: [] }
}

function issueJson(extra) {
  return Object.assign({ id: CREDENTIAL_FM, severity: "P2", title: "the GitHub token file is readable by other users", addon: null,
    remediation: { command: "omaddons doctor fix --only " + CREDENTIAL_FM, argv: ["omaddons", "doctor", "fix", "--only", CREDENTIAL_FM], destructive: false },
    auto_fixable: true,
    explain: { command: "omaddons doctor explain " + CREDENTIAL_FM, argv: ["omaddons", "doctor", "explain", CREDENTIAL_FM] } }, extra || {})
}

describe("repairRequest", () => {
  test("the table", () => {
    const issue = (extra) => M.reduceStatus(statusWithIssues([issueJson(extra)])).targets[0].issues[0]
    const long = "x".repeat(150) + "\nsecond line " + "y".repeat(50)
    table([
      ["machine", [issue(), ""], { template: "doctor_fix", slots: { fm: CREDENTIAL_FM }, label: "Repair: the GitHub token file is readable by other users" }],
      ["client", [issue(), "retail"], { template: "doctor_fix_target", slots: { target: "retail", fm: CREDENTIAL_FM }, label: "Repair: the GitHub token file is readable by other users" }],
      ["not_auto_fixable", [issue({ auto_fixable: false }), ""], null],
      ["an_m3_issue", [issue({ auto_fixable: undefined }), ""], null],
      ["an_id_the_pattern_refuses", [issue({ id: "fm-Bad,list" }), ""], null],
    ], ([i, t]) => M.repairRequest(i, t))
    const longRequest = M.repairRequest(issue({ title: long }), "")
    expect([longRequest.label.length, longRequest.label.endsWith("…"), longRequest.label.indexOf("\n")]).toEqual([120, true, -1])
    expect(M.buildArgv(longRequest.template, longRequest.slots, opts({ preview: true, label: longRequest.label })).argv).toContain("--dry-run")
  })
})

describe("issue rows", () => {
  const rowsOf = (issues, machine) => {
    const status = M.reduceStatus(statusWithIssues(issues, machine))
    return M.issueRows(M.issueSections(status.targets, [], "", status.issues))
  }
  test("issueOf keeps auto_fixable and explain, and an M3 issue gets none", () => {
    const reduced = M.reduceStatus(statusWithIssues([issueJson(), { id: "drift", severity: "P2", title: "t", remediation: { command: "x", argv: null } }]))
    const [repair, old] = reduced.targets[0].issues
    expect([repair.autoFixable, repair.explain.runnable, repair.explain.argv, repair.forced])
      .toEqual([true, true, ["omaddons", "doctor", "explain", CREDENTIAL_FM], false])
    expect([old.autoFixable, old.explain, old.forced]).toEqual([false, null, false])
  })
  test("the table", () => {
    const forcedArgv = ["omaddons", "doctor", "fix", "--only", "fm-concurrency_primitives-journal-too-new", "-t", "retail", "--force", "--yes"]
    const setAside = issueJson({ id: "fm-concurrency_primitives-journal-too-new", severity: "P0", auto_fixable: false,
      remediation: { command: forcedArgv.join(" "), argv: forcedArgv, destructive: true } })
    const explainOnly = issueJson({ auto_fixable: false,
      remediation: { command: "omaddons doctor explain fm-x -t retail", argv: ["omaddons", "doctor", "explain", "fm-x", "-t", "retail"], destructive: false } })
    const reinstall = issueJson({ id: "fm-external_artifacts-managed-files-modified", auto_fixable: false, explain: null,
      remediation: { command: "omaddons install Plater -t retail --reinstall", argv: ["omaddons", "install", "Plater", "-t", "retail", "--reinstall"], destructive: false } })
    const gc = issueJson({ auto_fixable: false,
      remediation: { command: "omaddons doctor gc --before 30d --dry-run", argv: ["omaddons", "doctor", "gc", "--before", "30d", "--dry-run"], destructive: true } })
    const pick = (r) => ({ fix: r.fix ? r.fix.template : null, action: r.action ? r.action.argv : null,
      explain: r.explain ? r.explain.argv : null, forced: r.forced, text: r.text, hint: r.hint,
      buttons: r.buttons.map((b) => b.text).join(", "), primary: r.primary })
    table([
      // A repair's remediation argv is never shown: its preview lists what it does.
      ["an_auto_fixable_row", [issueJson()], { fix: "doctor_fix_target", action: null, explain: ["omaddons", "doctor", "explain", CREDENTIAL_FM],
        forced: false, text: "", hint: "Enter previews the repair", buttons: "Repair, Explain", primary: "fix" }],
      ["a_set_aside_row", [setAside], { fix: null, action: null, explain: ["omaddons", "doctor", "explain", CREDENTIAL_FM],
        forced: true, text: forcedArgv.join(" "), hint: "Enter explains it", buttons: "Explain, Copy command", primary: "explain" }],
      ["a_doctor_explain_remediation", [explainOnly], { fix: null, action: null, explain: ["omaddons", "doctor", "explain", "fm-x", "-t", "retail"],
        forced: false, text: "", hint: "Enter explains it", buttons: "Explain", primary: "explain" }],
      // A reinstall previews through its template, as the Addons view's Repair does.
      ["a_runnable_reinstall", [reinstall], { fix: "reinstall", action: null,
        explain: null, forced: false, text: "", hint: "Enter runs the fix", buttons: "Repair Plater", primary: "fix" }],
      ["a_doctor_gc_remediation", [gc], { fix: null, action: null, explain: ["omaddons", "doctor", "explain", CREDENTIAL_FM],
        forced: false, text: "omaddons doctor gc --before 30d --dry-run", hint: "Enter explains it", buttons: "Explain, Copy command", primary: "explain" }],
    ], (issues) => pick(rowsOf(issues)[0]))
    // A machine issue's repair takes no -t.
    const machine = M.reduceStatus(statusWithIssues([], [issueJson()])).issues
    expect(M.issueRows(M.issueSections([], [], "", machine))[0].fix.template).toBe("doctor_fix")
    // A forced remediation is never runnable.
    expect(M.reduceStatus(statusWithIssues([setAside])).targets[0].issues[0].remediation.runnable).toBe(false)
  })
})

describe("explainArgv", () => {
  test("the table", () => {
    const explain = ["omaddons", "doctor", "explain", CREDENTIAL_FM, "-t", "retail"]
    table([
      ["an_explain_read", explain, explain],
      ["a_repair", ["omaddons", "doctor", "fix", "--only", CREDENTIAL_FM], null],
      ["another_read", ["omaddons", "status"], null],
      ["a_widget_flag", ["omaddons", "doctor", "explain", CREDENTIAL_FM, "--actor", "agent"], null],
      ["not_omaddons", ["sh", "doctor", "explain", CREDENTIAL_FM], null],
      ["no_array", "omaddons doctor explain " + CREDENTIAL_FM, null],
      ["null", null, null],
    ], (argv) => M.explainArgv(argv))
    // A copy: the service never holds the row's own array.
    expect(M.explainArgv(explain)).not.toBe(explain)
  })
})

describe("doctorResult", () => {
  test("the table", () => {
    const pick = (r) => r === null ? null
      : { dryRun: r.dryRun, actions: r.actions.length, token: r.precondition, report: r.report, repaired: r.repaired, undoable: r.undoable,
        tx: r.tx, error: r.error ? r.error.code : null }
    const stale = fixEnvelope({ dryRun: false, state: "REFUSING", exit: 4,
      error: { code: "plan_stale", message: "the repair changed", fix: null, fix_argv: null, alternatives: [], retryable: false, details: { changed: ["observed"] } } })
    const refused = fixEnvelope({ dryRun: true, state: "REFUSING", exit: 4,
      error: { code: "doctor_refused", message: "needs --force", fix: null, fix_argv: null, alternatives: [], retryable: false, details: { reason: "force_required" } } })
    const remove = { ok: true, schema_version: "1.1", tool_version: "0.1.0", command: "remove", exit_code: 0, partial: false, data: {}, warnings: [], commands: [] }
    const explain = doctorEnvelope("doctor explain", 0, { failure_mode: { id: CREDENTIAL_FM }, instances: [], skipped: [], incomplete: null }) // ubs:ignore — a failure-mode id, not a secret
    table([
      ["dry_run_with_a_chmod", fixEnvelope({ dryRun: true }), { dryRun: true, actions: 1, token: TOKEN, report: true, repaired: false, undoable: false, tx: null, error: null }],
      ["real_done_ok", fixEnvelope({}), { dryRun: false, actions: 1, token: "", report: true, repaired: true, undoable: true, tx: "01M68DFDPC0000000000000000", error: null }],
      ["done_partial", fixEnvelope({ state: "DONE_PARTIAL" }), { dryRun: false, actions: 1, token: "", report: true, repaired: true, undoable: true, tx: "01M68DFDPC0000000000000000", error: null }],
      ["done_failed", fixEnvelope({ state: "DONE_FAILED", taken: 0 }), { dryRun: false, actions: 1, token: "", report: true, repaired: false, undoable: true, tx: "01M68DFDPC0000000000000000", error: null }],
      ["plan_stale", stale, { dryRun: false, actions: 1, token: "", report: false, repaired: false, undoable: true, tx: "01M68DFDPC0000000000000000", error: "plan_stale" }],
      ["doctor_refused", refused, { dryRun: true, actions: 1, token: TOKEN, report: false, repaired: false, undoable: false, tx: null, error: "doctor_refused" }],
      ["a_remove_envelope", remove, null],
      ["a_doctor_explain_envelope", explain, null],
      ["a_malformed_token", fixEnvelope({ dryRun: true, token: "pc1:nope" }), { dryRun: true, actions: 1, token: "", report: true, repaired: false, undoable: false, tx: null, error: null }], // ubs:ignore — a malformed preview token, not a secret
    ], (env) => pick(M.doctorResult(env)))
  })
})

describe("displayPath", () => {
  const targets = [{ id: "retail", clientDir: CONTEXT.HOME + "/Games/wow/_retail_" }]
  test("the table", () => {
    table([
      ["config", [CREDENTIAL_PATH, CONTEXT], "<config>/credentials/github"],
      ["state", [CONTEXT.XDG_STATE_HOME + "/omaddons/history.jsonl", CONTEXT], "<state>/history.jsonl"],
      ["cache", [CONTEXT.XDG_CACHE_HOME + "/omaddons/blobs/ab", CONTEXT], "<cache>/blobs/ab"],
      ["a_client_dir", [CONTEXT.HOME + "/Games/wow/_retail_/Interface/AddOns/Foo", CONTEXT, targets], "retail:Interface/AddOns/Foo"],
      ["home", [CONTEXT.HOME + "/notes.txt", CONTEXT], "~/notes.txt"],
      ["outside_every_root", ["/etc/passwd", CONTEXT], "/etc/passwd"],
      ["not_a_component_boundary", [CONTEXT.XDG_CONFIG_HOME + "/omaddons2/x", CONTEXT], "~/.config/omaddons2/x"],
      ["below_wtf", [CONTEXT.HOME + "/Games/wow/_retail_/WTF/Account/ME/SavedVariables/x.lua", CONTEXT, targets], "(a path under WTF/, not shown)"],
      ["the_longest_prefix_wins", [CONTEXT.HOME + "/Games/wow/_retail_", CONTEXT, targets], "retail:"],
      ["no_context", [CREDENTIAL_PATH], CREDENTIAL_PATH],
    ], ([p, context, t]) => M.displayPath(p, context, t))
  })
})

describe("repairActionLine", () => {
  test("the table", () => {
    table([
      ["chmod", chmodAction(), "chmod 0600 <config>/credentials/github (was 0644)"],
      ["chmod_without_pre_mode", chmodAction({ pre: { kind: "file" } }), "chmod 0600 <config>/credentials/github"],
      ["chmod_that_exposes", chmodAction({ mode: "0644", pre: { kind: "file", mode: "0600" } }),
        "chmod 0644 <config>/credentials/github (was 0600) (readable by other users again)"],
      ["rewrite_with_a_size", { op: "write_file", path: CREDENTIAL_PATH, mode: "0600", backup: "<run>/backups/github", bytes: 7, pre: { kind: "file" } },
        "Rewrite <config>/credentials/github (7 B; the old file is kept for undo)"],
      ["create", { op: "write_file", path: CREDENTIAL_PATH, mode: "0600", bytes: 2048, pre: { kind: "absent" } }, "Create <config>/credentials/github (2.0 KiB)"],
      ["rewrite_in_a_real_run", { op: "write_file", path: CREDENTIAL_PATH, mode: "0600", backup: "<run>/b", ok: true, pre: { kind: "file" } },
        "Rewrite <config>/credentials/github (the old file is kept for undo)"],
      ["move", { op: "rename_noreplace", path: CONTEXT.XDG_CACHE_HOME + "/omaddons/blobs/ab", to: "<run>/quarantine/blobs/ab" },
        "Move <cache>/blobs/ab to <run>/quarantine/blobs/ab"],
      ["reactivation", { op: "rename_noreplace", path: "<run>/quarantine/j", to: CONTEXT.XDG_STATE_HOME + "/omaddons/journal/t.jsonl", activation: true },
        "Reactivate the interrupted change's journal (<state>/journal/t.jsonl); the next change finishes it"],
      ["an_unknown_op", { op: "set_xattr", path: "/x" }, "set xattr /x"],
    ], (action) => M.repairActionLine(action, CONTEXT, []))
  })
})

describe("doctorPreviewDecision", () => {
  const spec = { template: "doctor_fix", slots: { fm: CREDENTIAL_FM } }
  test("the table", () => {
    const thirteen = Array.from({ length: 13 }, (_, i) => chmodAction({ seq: i, path: CONTEXT.XDG_CONFIG_HOME + "/omaddons/credentials/k" + i }))
    // The decision with its sheet as text, after the title its spec's label gives.
    const pick = (d) => ({ kind: d.kind, token: d.token, line: d.line, lines: d.sheet ? M.dialogText(d.sheet).split("\n").slice(1) : [] })
    table([
      ["one_chmod", fixEnvelope({ dryRun: true }), { kind: "dialog", token: TOKEN, line: "",
        lines: ["chmod 0600 <config>/credentials/github (was 0644)", "Undo last reverts it."] }],
      ["bytes_to_back_up", fixEnvelope({ dryRun: true, bytes: 7 }), { kind: "dialog", token: TOKEN, line: "",
        lines: ["chmod 0600 <config>/credentials/github (was 0644)", "Keeps a copy of 7 B first, for undo.", "Undo last reverts it."] }],
      ["a_delegated_recovery_only", fixEnvelope({ dryRun: true, actions: [], delegated: [{ tx: "01TX", family: "folder", phase: "prepared", direction: "roll_back" }] }),
        { kind: "dialog", token: TOKEN, line: "", lines: ["Also finishes the interrupted change 01TX (roll back).", "Undo last reverts it."] }],
      ["nothing_planned", fixEnvelope({ dryRun: true, actions: [] }), { kind: "refresh", token: "", line: "Nothing to repair now; the view is refreshed", lines: [] }],
      ["a_prune_only", fixEnvelope({ dryRun: true, actions: [], prunes: [{ fm: CREDENTIAL_FM, target: "era", key: "01KEY", txs: ["01A", "01B"] }] }),
        { kind: "dialog", token: TOKEN, line: "", lines: ["Delete 2 undo snapshots on era",
          "Those changes can no longer be undone; omaddons would delete these snapshots after its next change anyway."] }],
      ["a_prune_of_another_failure_mode", fixEnvelope({ dryRun: true, prunes: [{ fm: "fm-other", target: "era", key: "01KEY", txs: ["01A"] }] }),
        { kind: "refuse", token: "", line: "The preview planned more than this repair; nothing ran", lines: [] }],
      ["another_failure_mode", fixEnvelope({ dryRun: true, actions: [chmodAction(), chmodAction({ fm: "fm-other" })] }),
        { kind: "refuse", token: "", line: "The preview planned more than this repair; nothing ran", lines: [] }],
      ["no_token", fixEnvelope({ dryRun: true, token: null }), { kind: "refuse", token: "", line: "The preview gave no token to bind; nothing ran", lines: [] }],
      ["doctor_refused", fixEnvelope({ dryRun: true, state: "REFUSING", exit: 4, error: { code: "doctor_refused", message: "no" } }),
        { kind: "failed", token: "", line: "", lines: [] }],
      ["not_a_doctor_envelope", statusWithIssues([]), { kind: "failed", token: "", line: "", lines: [] }],
    ], (env) => pick(M.doctorPreviewDecision(spec, env, CONTEXT, [])))
    // Every operation is listed: no cap.
    const many = M.doctorPreviewDecision(spec, fixEnvelope({ dryRun: true, actions: thirteen }), CONTEXT, [])
    expect([many.sheet.items.length, many.sheet.items[12].text]).toEqual([13, "chmod 0600 <config>/credentials/k12 (was 0644)"])
    expect(M.dialogText(many).split("\n").length).toBe(15)
    // The title is the problem's, from the request's label.
    expect([many.sheet.title, M.doctorPreviewDecision(Object.assign({ label: "Repair: The token file is readable" }, spec), fixEnvelope({ dryRun: true }),
      CONTEXT, []).sheet.title]).toEqual(["Repair this problem?", "Repair: The token file is readable?"])
  })
})

describe("doctorResultLine", () => {
  test("the table", () => {
    const three = [chmodAction(), chmodAction({ seq: 1 }), chmodAction({ seq: 2 })]
    table([
      ["repaired_one", fixEnvelope({}), "Repaired: chmod 0600 <config>/credentials/github (was 0644)"],
      ["repaired_three", fixEnvelope({ actions: three }), "Repaired: chmod 0600 <config>/credentials/github (was 0644) (and 2 more)"],
      ["nothing_repaired", fixEnvelope({ taken: 0, actions: [] }), "Nothing was repaired"],
      ["pruned", fixEnvelope({ taken: 0, actions: [], prunes: [{ fm: CREDENTIAL_FM, target: "era", key: "01KEY", outcome: "pruned", pruned: ["01A", "01B", "01C"] }] }),
        "Deleted 3 undo snapshots on era"],
      ["nothing_repaired_one_finding_left", fixEnvelope({ state: "DONE_FINDINGS", taken: 0, actions: [], findingsAfter: 1 }),
        "Nothing was repaired; 1 finding remains (see Issues)"],
      ["partial", fixEnvelope({ state: "DONE_PARTIAL", findingsAfter: 2 }), "Partly repaired (1 change); 2 findings remain"],
      ["rolled_back", fixEnvelope({ state: "DONE_FAILED", taken: 0 }), "The repair failed and was rolled back; nothing changed"],
      ["plan_stale", fixEnvelope({ state: "REFUSING", exit: 4, error: { code: "plan_stale", message: "the files changed" } }), "the files changed"],
    ], (env) => M.doctorResultLine(env, CONTEXT, []))
    // resultLine answers a repair with its own line, whatever its exit.
    expect(M.resultLine(fixEnvelope({ dryRun: true }), CONTEXT, [])).toBe("Nothing was repaired")
    expect(M.resultLine(fixEnvelope({}), CONTEXT, [])).toBe("Repaired: chmod 0600 <config>/credentials/github (was 0644)")
  })
})

describe("explainLines", () => {
  const fm = (extra) => Object.assign({ id: CREDENTIAL_FM, subsystem: "secrets", severity: "P2", title: "A credential file other users can read",
    description: "Its mode lets other users read it.", why: "Anyone on this machine can take the token.", tier: "cached",
    fixer: { ops: ["chmod"], scopes: ["config"], enabled: true, needs_force: false }, manual: null }, extra || {})
  const instance = (extra) => Object.assign({ id: CREDENTIAL_FM, instance: CREDENTIAL_FM + "@machine", label: "machine · <config>/credentials/github",
    evidence: { path: CREDENTIAL_PATH, mode: "0644", private: false, size: 41 }, remediation: { command: "omaddons doctor fix --only " + CREDENTIAL_FM } }, extra || {})
  const env = (f, instances, incomplete) => doctorEnvelope("doctor explain", 0, { failure_mode: f, instances: instances, skipped: [], incomplete: incomplete || null })
  test("the table", () => {
    const head = ["A credential file other users can read", "Its mode lets other users read it.", "Why it matters: Anyone on this machine can take the token."]
    const one = ["machine · <config>/credentials/github", "  mode: 0644", "  path: <config>/credentials/github", "  private: no", "  size: 41",
      "  In a terminal: omaddons doctor fix --only " + CREDENTIAL_FM]
    table([
      ["title_description_why", env(fm(), [instance()]), head.concat(one)],
      ["no_fixer", env(fm({ fixer: null }), []), head.concat(["No automatic repair", "No instance found now; it may be resolved"])],
      ["needs_force", env(fm({ fixer: { enabled: true, needs_force: true } }), []),
        head.concat(["Its repair needs your deliberate consent: read this, then run the command below in a terminal", "No instance found now; it may be resolved"])],
      ["off_by_default", env(fm({ fixer: { enabled: false, needs_force: false } }), []),
        head.concat(["Its repair is off by default: run the command below in a terminal", "No instance found now; it may be resolved"])],
      ["by_hand", env(fm({ manual: { instruction: "chmod 600 <path>", why: "it is outside <config>" } }), []),
        head.concat(["By hand: chmod 600 <path> (it is outside <config>)", "No instance found now; it may be resolved"])],
      ["no_instance_and_cut_short", env(fm(), [], { limit: "deadline", deadline_ms: 2000, max_entries: null }),
        head.concat(["No instance found now; it may be resolved", "The check was cut short (deadline); this may be incomplete"])],
      ["not_explain", fixEnvelope({ dryRun: true }), []],
    ], (e) => M.explainLines(e, CONTEXT, []))
  })
  test("evidence values, and the bounds", () => {
    const values = env(fm(), [instance({ evidence: { a_path: CREDENTIAL_PATH, b_wtf: CONTEXT.HOME + "/g/WTF/Account/ME/x", c_list: ["1", "2", "3", "4", "5", "6", "7"],
      d_object: { x: 1 }, e_null: null } })])
    expect(M.explainLines(values, CONTEXT, []).slice(4, 7)).toEqual(["  a path: <config>/credentials/github", "  b wtf: (a path under WTF/, not shown)",
      "  c list: 1, 2, 3, 4, 5 and 2 more"])
    const keys = {}
    for (let i = 0; i < 13; i++) keys["k" + String(i).padStart(2, "0")] = i
    const many = M.explainLines(env(fm(), [instance({ evidence: keys })]), CONTEXT, [])
    expect(many.filter((l) => l.startsWith("  k")).length).toBe(12)
    const six = M.explainLines(env(fm(), Array.from({ length: 6 }, (_, i) => instance({ label: "i" + i, evidence: {}, remediation: {} }))), CONTEXT, [])
    expect(six.slice(3)).toEqual(["i0", "i1", "i2", "i3", "i4", "(and 1 more)"])
  })
})

describe("explainFor", () => {
  test("the table", () => {
    const explanation = { selection: { key: "p/retail/0" }, lines: ["a"] }
    table([
      ["its_row", [explanation, { key: "p/retail/0" }], ["a"]],
      ["another_row", [explanation, { key: "p/retail/1" }], null],
      ["no_explanation", [null, { key: "p/retail/0" }], null],
    ], ([e, row]) => M.explainFor(e, row))
  })
})

describe("staleText for doctor fix", () => {
  const staleFix = (changed) => fixEnvelope({ state: "REFUSING", exit: 4,
    error: { code: "plan_stale", message: "m", details: changed === undefined ? {} : { changed: changed } } })
  test("the table", () => {
    const after = ". Nothing ran; here is what would happen now."
    table([
      ["observed", staleFix(["observed"]), "Changed since you looked: the files this repair checks" + after],
      ["selection_and_observed", staleFix(["selection", "observed"]), "Changed since you looked: what it would do; the files this repair checks" + after],
      ["no_parts", staleFix(undefined), "Changed since you looked: the repair" + after],
      ["a_remove", { ok: false, schema_version: "1.1", tool_version: "0.1.0", command: "remove", exit_code: 4,
        error: { code: "plan_stale", message: "m", details: { changed: ["observed"] } } }, "Changed since you looked: files it would change were changed" + after],
    ], (env) => M.staleText(env))
  })
})

describe("repair undo texts", () => {
  const undoEnv = (revertedKind, targets) => ({ ok: true, schema_version: "1.1", tool_version: "0.1.0", command: "undo", exit_code: 0, partial: false,
    data: { dry_run: true, kind: "repair", reverted: { tx: "01TX", kind: revertedKind, targets: targets }, folders: null,
      repairs: [{ seq: 0, fix: 0, fm: CREDENTIAL_FM, op: "chmod", path: CREDENTIAL_PATH, to_rel: null, octal_mode: "0644", activation: false }] },
    warnings: [], commands: [] })
  test("the table", () => {
    table([
      ["label_home_side", M.lastChangeOf({ kind: "repair", targets: [], undo: null }).label, "Undo repair"],
      ["label_on_a_client", M.lastChangeOf({ kind: "repair", targets: ["retail"], undo: null }).label, "Undo repair on retail"],
      ["undo_sheet", M.dialogText(M.confirmSheet("undo", undoEnv("repair", []), CONTEXT, [])),
        "Undo the repair from an earlier time?\nchmod 0644 <config>/credentials/github (readable by other users again)"],
      ["redo_sheet", (() => {
        const sheet = M.confirmSheet("undo", undoEnv("undo", [{ id: "retail", key: "k" }]), CONTEXT, [{ id: "retail", key: "k", displayName: "Retail" }])
        return [sheet.title, sheet.confirmLabel]
      })(), ["Undo the undo on Retail from an earlier time?", "Redo"]],
      ["result", M.resultLine(Object.assign(undoEnv("repair", []), { data: Object.assign(undoEnv("repair", []).data, { dry_run: false }) })), "Undid repair (1 change)"],
      ["a_folder_undo", M.dialogText(M.confirmSheet("undo", { ok: true, schema_version: "1.1", tool_version: "0.1.0", command: "undo", exit_code: 0,
        data: { kind: "folders", reverted: { kind: "update" }, transactions: [{ target: { id: "retail" } }], folders: { restored: ["A"], removed: ["B"] } } })),
        "Undo the update on retail from an earlier time?\nA back as it was\nB removed"],
    ], (value) => value)
  })
})

describe("resultUndo", () => {
  const undoArgv = ["omaddons", "undo", "--run", "01RUN"]
  const repairChange = (extra) => Object.assign({ unknown: false, kind: "repair", label: "Undo repair",
    undo: { command: undoArgv.join(" "), argv: undoArgv, runnable: true } }, extra || {})
  const updateResult = { ok: true, commands: [{ action: "undo_run", command: "omaddons undo --run 01U", argv: ["omaddons", "undo", "--run", "01U"], runnable: true }] }
  test("the table", () => {
    table([
      ["a_repair_status_seen", [{ repair: true, statusSeen: true }, repairChange()], { action: "undo_run", command: undoArgv.join(" "), argv: undoArgv, runnable: true }],
      ["status_not_seen_yet", [{ repair: true, statusSeen: false }, repairChange()], null],
      ["the_last_change_is_an_update", [{ repair: true, statusSeen: true }, repairChange({ kind: "update" })], null],
      ["unknown", [{ repair: true, statusSeen: true }, { unknown: true }], null],
      ["not_runnable", [{ repair: true, statusSeen: true }, repairChange({ undo: { command: "x", argv: null, runnable: false } })], null],
      ["an_updates_result", [updateResult, null], updateResult.commands[0]],
      ["a_repair_result_with_only_a_doctor_undo_hint", [{ repair: true, statusSeen: false,
        commands: [{ action: "undo", command: "omaddons doctor undo r", argv: ["omaddons", "doctor", "undo", "r"], runnable: true }] }, null], null],
    ], ([result, change]) => M.resultUndo(result, change))
  })
})

// ------------------------------------------------------------- the views' states (1n2p.19)

describe("reduceCheck and readingResources", () => {
  test("the table", () => {
    const pick = (c) => [c.trouble, c.failed, c.at !== ""]
    const failedItem = JSON.parse(JSON.stringify(fixture("check")))
    failedItem.data.targets[0].items[0].status = "failed"
    failedItem.data.targets[0].items[1].status = "failed"
    failedItem.partial = true
    const refused = { ok: false, schema_version: "1.1", tool_version: "0.1.0", command: "outdated", exit_code: 1,
      error: { code: "network", message: "x" }, warnings: [], commands: [] }
    table([
      ["every_source_answered", [fixture("check"), "ok"], [false, 0, true]],
      // GitHub answered 503 for Foo: the check showed what it remembered, and says so by code.
      ["a_source_did_not_answer", [fixture("check-unreachable"), "ok"], [true, 0, true]],
      ["items_that_failed", [failedItem, "ok"], [true, 2, true]],
      ["the_check_failed_on_the_network", [refused, "failed"], [true, 0, false]],
      ["no_answer", [null, "timeout"], [true, 0, false]],
    ], ([env, outcome]) => pick(M.reduceCheck(env, outcome)))
    // Another code never counts as a source not answering.
    const other = JSON.parse(JSON.stringify(fixture("check")))
    other.warnings = [{ code: "reload_required", message: "x", fix: null }]
    expect(M.reduceCheck(other, "ok").trouble).toBe(false)
    expect(M.SOURCE_TROUBLE.slice().sort()).toEqual(["http_status", "network", "offline", "rate_limited", "stale_check"])
    expect(M.readingResources([{ resource: "list" }, null], { local: [{ resource: "errors" }, { resource: "list" }], network: [{ resource: "search" }],
      mutation: [{ resource: "update_all" }] })).toEqual(["list", "errors", "search"])
    expect(M.readingResources(null, null)).toEqual([])
  })
})

describe("viewState", () => {
  const targets = () => M.reduceStatus(fixture("status-updates"), NOW).targets
  // A service as viewState reads one: the status's clients, nothing read, nothing running.
  const service = (fields) => Object.assign({ targets: targets(), reading: [], outcomes: {} }, fields || {})
  const lists = () => M.reduceList(fixture("list"))
  const age = (minutes) => Date.parse(M.oldestCheck(targets(), "")) + minutes * 60000
  // `[state, sentence, action, refreshing, notices as [kind, sentence, action]]`.
  const pick = (v) => [v.state, v.sentence, v.action ? v.action.name : null, v.refreshing,
    v.notices.map((n) => [n.kind, n.sentence, n.action ? n.action.name : null])]
  const state = (view, s, input) => pick(M.viewState(view, Object.assign({ service: s, filter: "", rows: 0, allRows: 0, nowMs: age(25) }, input || {})))

  test("first load, a failed read, and a read that runs again", () => {
    const cases = [
      ["updates_first_load", state("updates", service()), ["firstLoad", "Reading your addons…", null, false, []]],
      ["installed_first_load", state("installed", service()), ["firstLoad", "Reading your addons…", null, false, []]],
      ["errors_first_load", state("errors", service()), ["firstLoad", "Reading the Lua errors…", null, false, []]],
      ["issues_first_load", state("issues", service()), ["firstLoad", "Checking…", null, false, []]],
      ["backups_first_load", state("backups", service()), ["firstLoad", "Reading backups…", null, false, []]],
      ["search_first_load", state("search", service(), { query: "dbm" }), ["firstLoad", "Searching…", null, false, []]],
      ["updates_failed", state("updates", service({ outcomes: { list: "failed" } })), ["failed", "Couldn't read your addons.", "retry", false, []]],
      ["installed_failed", state("installed", service({ outcomes: { list: "timeout" } })), ["failed", "Couldn't read your addons.", "retry", false, []]],
      ["errors_failed", state("errors", service({ outcomes: { errors: "failed" } })), ["failed", "Couldn't read the Lua errors.", "retry", false, []]],
      // Issues is `status`'s problems: a failed `errors` read is the Errors view's.
      ["issues_failed", state("issues", service({ outcomes: { status: "failed" } })), ["failed", "Couldn't read the problems.", "retry", false, []]],
      ["issues_not_errors", state("issues", service({ outcomes: { errors: "failed" } })), ["firstLoad", "Checking…", null, false, []]],
      ["backups_failed", state("backups", service({ outcomes: { backups: "failed" } })), ["failed", "Couldn't read your backups.", "retry", false, []]],
      ["search_failed", state("search", service({ outcomes: { search: "failed" } }), { query: "dbm" }), ["failed", "The search didn't finish.", "retry", false, []]],
      // Asked again: loading, not the last failure.
      ["failed_then_reading", state("updates", service({ outcomes: { list: "failed" }, reading: ["list"] })), ["firstLoad", "Reading your addons…", null, false, []]],
    ]
    for (const [name, got, expected] of cases) expect([name, got]).toEqual([name, expected])
  })

  test("a refresh keeps the rows, and a re-read that failed keeps the last good data", () => {
    const listsAt = age(25) - 10 * 60000
    const cases = [
      ["refreshing_keeps_rows", state("updates", service({ lists: lists(), reading: ["list"] }), { rows: 4, allRows: 4 }), ["rows", "", null, true, []]],
      ["a_check_refreshes_updates", state("updates", service({ lists: lists(), checking: true }), { rows: 4, allRows: 4 }), ["rows", "", null, true, []]],
      ["another_resource_reading", state("updates", service({ lists: lists(), reading: ["errors"] }), { rows: 4, allRows: 4 }), ["rows", "", null, false, []]],
      ["failed_with_data", state("installed", service({ lists: lists(), listsAt: listsAt, outcomes: { list: "failed" } }), { rows: 5, allRows: 5 }),
        ["rows", "", null, false, [["failed", "Couldn't read your addons again; showing what was read 10 min ago.", "retry"]]]],
      ["failed_with_data_never_read_at", state("backups", service({ backups: { targets: [] }, outcomes: { backups: "failed" } }), { rows: 1, allRows: 1 }),
        ["rows", "", null, false, [["failed", "Couldn't read your backups again; showing what was read earlier.", "retry"]]]],
      // Issues lists status's problems: a status read refreshes them, an errors read not.
      ["issues_rows_refreshing", state("issues", service({ summary: {}, reading: ["status"] }), { rows: 2, allRows: 2 }), ["rows", "", null, true, []]],
      ["issues_rows_while_errors_read", state("issues", service({ summary: {}, reading: ["errors"] }), { rows: 2, allRows: 2 }), ["rows", "", null, false, []]],
      ["errors_rows_refreshing", state("errors", service({ errors: { targets: [] }, reading: ["errors"] }), { rows: 2, allRows: 2 }), ["rows", "", null, true, []]],
    ]
    for (const [name, got, expected] of cases) expect([name, got]).toEqual([name, expected])
  })

  test("empty and filtered empty, each with its sentence and next action", () => {
    const unchecked = targets().map((t) => Object.assign({}, t, { updates: Object.assign({}, t.updates, { checkedAt: null }) }))
    const cases = [
      ["updates_empty", state("updates", service({ lists: lists() })), ["empty", "All up to date. Checked 25 min ago.", null, false, []]],
      ["updates_filtered_empty", state("updates", service({ lists: lists() }), { filter: "retail", allRows: 2 }),
        ["filteredEmpty", "Retail is up to date. Checked 25 min ago.", null, false, []]],
      ["updates_client_empty_everywhere", state("updates", service({ lists: lists() }), { filter: "retail" }),
        ["empty", "Retail is up to date. Checked 25 min ago.", null, false, []]],
      ["updates_never_checked", state("updates", service({ lists: lists(), targets: unchecked })), ["empty", "Not checked for updates yet.", "check", false, []]],
      // No client: the frame's card says so, and the view nothing.
      ["updates_no_clients", state("updates", service({ lists: lists(), targets: [] })), ["empty", "", null, false, []]],
      ["installed_empty", state("installed", service({ lists: lists() })), ["empty", "No addons yet. Find some in Search.", "search", false, []]],
      ["installed_filtered_empty", state("installed", service({ lists: lists() }), { filter: "era", allRows: 3 }),
        ["filteredEmpty", "No addons on Classic Era yet. Find some in Search.", "search", false, []]],
      ["errors_empty", state("errors", service({ errors: { targets: [] } })), ["empty", "No client records Lua errors here.", null, false, []]],
      ["errors_filtered_empty", state("errors", service({ errors: { targets: [] } }), { filter: "retail", allRows: 1 }),
        ["filteredEmpty", "Retail records no Lua errors.", null, false, []]],
      ["issues_empty", state("issues", service({ summary: {} })), ["empty", "Nothing needs fixing.", null, false, []]],
      ["issues_filtered_empty", state("issues", service({ summary: {} }), { filter: "retail", allRows: 1 }),
        ["filteredEmpty", "Nothing needs fixing on Retail.", null, false, []]],
      ["backups_empty", state("backups", service({ backups: { targets: [] } })), ["empty", "No backups yet.", "backup", false, []]],
      ["backups_filtered_empty", state("backups", service({ backups: { targets: [] } }), { filter: "era", allRows: 1 }),
        ["filteredEmpty", "No backups of Classic Era yet.", "backup", false, []]],
      ["search_no_query", state("search", service()), ["empty", "Type a name, or paste a link from GitHub, WoWInterface, Tukui or WowAce.", null, false, []]],
      ["search_nothing_found", state("search", service({ search: { rows: [], skipped: [], selection: { query: "zzz", target: "" } } }), { query: "zzz" }),
        ["empty", "Nothing found for “zzz”.", null, false, []]],
      // An answer for another query is not this one's.
      ["search_another_query", state("search", service({ search: { rows: [], skipped: [], selection: { query: "dbm", target: "" } } }), { query: "zzz" }),
        ["firstLoad", "Searching…", null, false, []]],
    ]
    for (const [name, got, expected] of cases) expect([name, got]).toEqual([name, expected])
    // Each state has its glyph: loading, failed, a view's own when empty, a filter.
    expect(M.viewState("updates", { service: service() }).glyph).toBe(0xF04E6)
    expect(M.viewState("updates", { service: service({ outcomes: { list: "failed" } }) }).glyph).toBe(0xF0028)
    expect(M.viewState("backups", { service: service({ backups: { targets: [] } }) }).glyph).toBe(0xF003C)
    expect(M.viewState("updates", { service: service({ lists: lists() }), filter: "retail", allRows: 1 }).glyph).toBe(0xF0236)
  })

  test("offline, partial and stale qualify the rows they keep", () => {
    const stale = targets().map((t) => Object.assign({}, t, { updates: Object.assign({}, t.updates, { stale: true }) }))
    const backupsStale = targets().map((t) => Object.assign({}, t, { backups: Object.assign({}, t.backups, { stale: t.id === "era" }) }))
    const skipped = M.reduceSearch(fixture("search-skipped"))
    skipped.selection = { query: "damage meter", target: "retail" }
    const limited = { rows: [], selection: { query: "x", target: "" }, skipped: [
      { source: "github", reason: "rate_limited", reset_at: "2100-01-01T18:02:00Z" }, { source: "tukui", reason: "disabled" },
      { source: "wowi", reason: "no_index" }] }
    const cases = [
      ["check_unreachable", state("updates", service({ lists: lists(), lastCheck: M.reduceCheck(fixture("check-unreachable"), "ok") }), { rows: 4, allRows: 4 }),
        ["rows", "", null, false, [["offline", "Some sources didn't answer the last check; what they said before is shown.", "check"]]]],
      ["check_failed_items", state("updates", service({ lists: lists(), lastCheck: { trouble: true, failed: 2 } }), { rows: 4, allRows: 4 }),
        ["rows", "", null, false, [["offline", "2 addons couldn't be checked: their sources didn't answer.", "check"]]]],
      ["check_failed_one_item", state("updates", service({ lists: lists(), lastCheck: { trouble: true, failed: 1 } })),
        ["empty", "All up to date. Checked 25 min ago.", null, false, [["offline", "1 addon couldn't be checked: its source didn't answer.", "check"]]]],
      ["check_stale", state("updates", service({ lists: lists(), targets: stale }), { rows: 4, allRows: 4 }),
        ["rows", "", null, false, [["stale", "Last checked 25 min ago.", "check"]]]],
      // Empty: its sentence says the age already, and Check now is its action.
      ["check_stale_empty", state("updates", service({ lists: lists(), targets: stale })),
        ["empty", "All up to date. Checked 25 min ago.", "check", false, []]],
      // While a check runs, it says nothing about the last one.
      ["check_running", state("updates", service({ lists: lists(), targets: stale, checking: true, lastCheck: { trouble: true, failed: 0 } }), { rows: 4, allRows: 4 }),
        ["rows", "", null, true, []]],
      ["a_good_check", state("updates", service({ lists: lists(), lastCheck: M.reduceCheck(fixture("check"), "ok") }), { rows: 4, allRows: 4 }), ["rows", "", null, false, []]],
      ["errors_partial", state("errors", service({ errors: M.reduceErrors(fixture("errors-partial")) }), { rows: 3, allRows: 3 }),
        ["rows", "", null, false, [["partial", "Some errors couldn't be read yet: the game was writing. Read again after you quit.", "read_errors"]]]],
      // Another client's partial read is not this client's.
      ["errors_partial_elsewhere", state("errors", service({ errors: M.reduceErrors(fixture("errors-partial")) }), { filter: "era", rows: 1, allRows: 3 }),
        ["rows", "", null, false, []]],
      ["backups_stale", state("backups", service({ backups: { targets: [] }, targets: backupsStale }), { rows: 2, allRows: 2 }),
        ["rows", "", null, false, [["stale", "Backup details are out of date.", "refresh_backups"]]]],
      ["backups_stale_elsewhere", state("backups", service({ backups: { targets: [] }, targets: backupsStale }), { filter: "retail", rows: 1, allRows: 2 }),
        ["rows", "", null, false, []]],
      ["search_offline", state("search", service({ search: skipped }), { filter: "retail", query: "damage meter" }),
        ["empty", "Nothing found for “damage meter”.", null, false, [["offline", "GitHub, WoWInterface and Tukui couldn't be searched: offline.", null]]]],
    ]
    for (const [name, got, expected] of cases) expect([name, got]).toEqual([name, expected])
    // Skipped sources, one notice per reason; a time in local time.
    const notices = M.viewState("search", { service: service({ search: limited }), query: "x", rows: 1 }).notices.map((n) => n.sentence)
    const clock = (iso) => { const d = new Date(iso); const two = (n) => (n < 10 ? "0" : "") + n; return two(d.getHours()) + ":" + two(d.getMinutes()) }
    expect(notices).toEqual(["GitHub couldn't be searched: it asked to wait until " + clock("2100-01-01T18:02:00Z") + ".",
      "Tukui couldn't be searched: turned off.", "WoWInterface couldn't be searched."])
    // A rate limit without a time, and omaddons' own catalog by name.
    expect(M.viewState("search", { service: service({ search: { rows: [], selection: { query: "x", target: "" },
      skipped: [{ source: "catalog", reason: "rate_limited" }] } }), query: "x", rows: 1 }).notices[0].sentence)
      .toBe("omaddons' catalog couldn't be searched: it asked to wait.")
  })

  test("every word is the copy guide's: no CLI term, and every view has every cell", () => {
    const words = []
    for (const view of M.VIEWS) {
      for (const outcome of [{}, { [M.VIEW_RESOURCES[view]]: "failed" }]) {
        const v = M.viewState(view, { service: service({ outcomes: outcome }), query: "q" })
        words.push(v.sentence)
        expect([view, Object.keys(v)]).toEqual([view, ["view", "state", "glyph", "sentence", "detail", "action", "refreshing", "notices"]])
      }
    }
    for (const w of words) expect([w, /\b(plan|preview|token|precondition|target|ref)\b/i.test(w)]).toEqual([w, false])
    expect(M.viewState("nosuchview", {}).state).toBe("firstLoad")
  })
})

// ---------------------------------------------------------------- activity (1n2p.18)

describe("activity", () => {
  // The lines a fixture's run wrote to stderr, parsed.
  const events = (name) => fixtureText(name + ".stderr.ndjson").trim().split("\n").map((l) => JSON.parse(l))
  // The status fixture's clients, for their player names.
  const clients = () => M.reduceStatus(fixture("status-updates"), NOW).targets
  // The planned apply of "Update all" (the plan the routine preview stored), as the
  // service starts it.
  const planned = () => {
    const plan = M.planFrom(fixture("plan-update-all"))
    expect(plan).not.toBeNull()
    return { id: 9, kind: "mutation", lane: "mutation", priority: "user", resource: "update_all", template: "",
      cli: plan.argv.slice(), plan, label: "Update all", purpose: "planned", spec: { template: "update_all", slots: {} } }
  }
  const fold = (a, list, at) => list.reduce((acc, e) => M.activityProgress(acc, e, at), a)

  test("every phase the CLI publishes has words", () => {
    const schema = fixture("contract-schemas").data.events.progress
    const phase = schema.$defs.Phase
    const published = phase.enum || phase.oneOf.map((v) => v.const)
    expect([...published].sort()).toEqual([...M.PROGRESS_PHASES].sort())
    for (const p of published) {
      const word = M.phaseWord(p, "update")
      expect([p, word !== "" && word !== p && !/[{}]/.test(word)]).toEqual([p, true])
    }
    expect([M.phaseWord("copy", "update"), M.phaseWord("copy", "backup"), M.phaseWord("copy", "restore")])
      .toEqual(["backing up settings first", "copying settings", "copying settings"])
    expect([M.phaseWord("commit", "update"), M.phaseWord("commit", "install"), M.phaseWord("commit", "remove"), M.phaseWord("commit", "pin")])
      .toEqual(["installing", "installing", "finishing", "finishing"])
    expect(M.phaseWord("nosuch", "update")).toBe("")
    expect(schema.required).toEqual(expect.arrayContaining(["target", "ref"]))
  })

  test("what the player waits on has an activity; background work does not", () => {
    const kind = (r) => M.activityKind(r)
    expect([
      kind({ resource: "check", kind: "data", lane: "network", priority: "user" }),
      kind({ resource: "check", kind: "data", lane: "network", priority: "background" }),
      kind({ resource: "cli:outdated", kind: "data", lane: "network", priority: "user" }),
      kind({ resource: "search", kind: "view", lane: "network", priority: "user" }),
      kind({ resource: "changelog", kind: "view", lane: "network", priority: "user" }),
      kind({ resource: "update_all", kind: "mutation", lane: "mutation", priority: "user" }),
      kind({ resource: "backup_auto", kind: "mutation", lane: "mutation", priority: "background" }),
      kind({ resource: "errors_target", kind: "data", lane: "local", priority: "background" }),
      kind({ resource: "status", kind: "data", lane: "local", priority: "user" }),
      kind({ resource: "list", kind: "data", lane: "local", priority: "user" }),
    ]).toEqual(["check", "check", "check", "read", "read", "change", "", "", "", ""])
    expect(M.activityStart({ resource: "status", kind: "data" }, NOW, null)).toBeNull()
  })

  test("a check shows at once, then the count the CLI reports", () => {
    const a = M.activityStart({ id: 3, resource: "check", kind: "data", lane: "network", label: "Check now" }, NOW, null)
    expect([a.kind, a.lane, a.cancellable]).toEqual(["check", "network", true])
    const before = M.activityView(a, [], NOW)
    expect([before.text, before.fraction, before.cancellable, before.elapsed, before.stalled])
      .toEqual(["Checking for updates…", -1, true, "", ""])
    const list = events("check")
    expect(list.map((e) => e.phase)).toEqual(["fetch", "fetch"])
    const first = M.activityView(fold(a, list.slice(0, 1), NOW), [], NOW)
    expect([first.text, first.fraction]).toEqual(["Checking for updates (1 of 2)", 0.5])
    // A count that reaches its total is not the end: the answer is.
    expect(M.activityView(fold(a, list, NOW), [], NOW).text).toBe("Checking for updates (2 of 2)")
    // Anything but a progress event leaves it as it was.
    expect(M.activityProgress(a, { event: "log", message: "x" }, NOW)).toBe(a)
  })

  test("a search counts its sources, never their names", () => {
    const a = M.activityStart({ id: 4, resource: "search", kind: "view", lane: "network", label: "Search" }, NOW, null)
    const list = events("search")
    expect(M.activityView(a, [], NOW).text).toBe("Searching…")
    const view = M.activityView(fold(a, list, NOW), [], NOW)
    expect(view.text).toBe("Searching (" + list.length + " of " + list.length + ")")
    expect(view.text).not.toContain("WoWInterface")
  })

  test("an update's rows follow their own events, by target and ref", () => {
    const lists = M.reduceList(fixture("plan-list"))
    const a = M.activityStart(planned(), NOW, lists)
    expect([a.kind, a.op, a.cancellable, Object.keys(a.rows).sort()])
      .toEqual(["change", "update", false, ["era/Bar", "era/Foo", "retail/Bar"]])
    expect(a.rows["era/Bar"].ref).toBe("gh:o/bar")
    const targets = clients()
    const row = (act, key) => M.activityRow(act, key, null)
    expect(M.activityView(a, targets, NOW).text).toBe("Updating 3 addons on 2 clients…")
    expect(row(a, "era/Bar")).toEqual({ pending: true, text: "Updating…" })
    expect(row(a, "retail/Foo")).toBeNull()

    const list = events("plan-update-all-apply")
    const one = fold(a, list.slice(0, 1), NOW)
    expect([row(one, "era/Bar").text, row(one, "era/Foo").text, row(one, "retail/Bar").text])
      .toEqual(["Updating… checking files (1 of 3)", "Updating…", "Updating…"])
    expect(M.activityView(one, targets, NOW).text).toBe("Updating 3 addons on 2 clients: Bar, checking files (1 of 3)")
    expect(M.activityView(one, targets, NOW).fraction).toBeCloseTo(1 / 3)

    // A client's stage names its rows and, with several clients, the client.
    const staged = fold(a, list.slice(0, 4), NOW)
    expect(list[3]).toMatchObject({ phase: "stage", target: "retail", ref: null })
    expect([row(staged, "retail/Bar").text, row(staged, "era/Foo").text])
      .toEqual(["Updating… preparing (1 of 2)", "Updating… checking files (2 of 3)"])
    expect(M.activityView(staged, targets, NOW).text).toBe("Updating 3 addons on 2 clients: Retail, preparing (1 of 2)")
    const done = fold(a, list, NOW)
    expect([row(done, "era/Bar").text, row(done, "era/Foo").text, row(done, "retail/Bar").text])
      .toEqual(["Updating… installing (2 of 2)", "Updating… installing (2 of 2)", "Updating… installing (1 of 2)"])
  })

  test("an event moves rows by its typed fields, never by its message", () => {
    const a = M.activityStart(planned(), NOW, M.reduceList(fixture("plan-list")))
    const untyped = M.activityProgress(a, { event: "progress", phase: "download", message: "gh:o/bar@v2 on era",
      current: 1, total: 3, target: null, ref: null }, NOW)
    expect(M.activityRow(untyped, "era/Bar", null).text).toBe("Updating…")
    // GitHub refs compare without case, as the CLI's do.
    const cased = M.activityProgress(a, { event: "progress", phase: "download", message: "", current: 1, total: 3,
      target: "era", ref: "gh:O/Bar" }, NOW)
    expect(M.activityRow(cased, "era/Bar", null).text).toBe("Updating… downloading (1 of 3)")
    expect(M.activityRow(cased, "retail/Bar", null).text).toBe("Updating…")
  })

  test("the rows a change changed say so", () => {
    const done = M.changedRows(fixture("plan-update-all-apply"))
    expect(done).toEqual({ "era/Bar": "Updated", "era/Foo": "Updated", "retail/Bar": "Updated" })
    expect(M.activityRow(null, "era/Bar", done)).toEqual({ pending: false, text: "Updated" })
    expect(M.activityRow(null, "retail/Foo", done)).toBeNull()
    expect(M.changedRows(fixture("failure"))).toEqual({})
    expect(M.changedRows(null)).toEqual({})
  })

  test("one addon is named, with its client; a change without a plan names its slots' row", () => {
    const targets = clients()
    const plan = M.planFrom(fixture("plan-update-all"))
    plan.approval.selection = plan.approval.selection.filter((i) => i.target_id === "retail")
    const one = M.activityStart({ id: 1, kind: "mutation", lane: "mutation", priority: "user", plan, label: "Update Bar",
      purpose: "planned", spec: { template: "update_addon" } }, NOW, null)
    expect(M.activityView(one, targets, NOW).text).toBe("Updating Bar on Retail…")
    const pin = M.activityStart({ id: 2, kind: "mutation", lane: "mutation", priority: "user", template: "pin",
      slots: { target: "retail", addon: "Bar" }, label: "Pin Bar" }, NOW, M.reduceList(fixture("plan-list")))
    expect([pin.op, Object.keys(pin.rows)]).toEqual(["pin", ["retail/Bar"]])
    const pinning = M.activityProgress(pin, { event: "progress", phase: "commit", message: "retail", current: 1, total: 1,
      target: "retail", ref: null }, NOW)
    expect(M.activityView(pinning, targets, NOW).text).toBe("Pinning Bar on Retail: finishing (1 of 1)")
    expect(M.activityRow(pinning, "retail/Bar", null).text).toBe("Pinning… finishing (1 of 1)")
    // A settings checkpoint before a change is said as such.
    const update = M.activityStart({ id: 5, kind: "mutation", lane: "mutation", priority: "user", template: "update_target",
      slots: { target: "retail" }, label: "Update Retail" }, NOW, null)
    const copying = M.activityProgress(update, { event: "progress", phase: "copy", message: "WTF/ files", current: 120,
      total: 400, target: null, ref: null }, NOW)
    expect(M.activityView(copying, targets, NOW).text).toBe("Update Retail: backing up settings first (120 of 400)")
  })

  test("a preview moves no rows and says what it is checking", () => {
    const view = (mode, label) => M.activityView(M.activityStart({ id: 6, kind: "mutation", lane: "mutation", priority: "user",
      template: "remove", slots: { target: "retail", addon: "Bar" }, preview: true, mode, label }, NOW, null), [], NOW)
    expect([view("match", "Update all").text, view("confirm", "Remove Bar").text, view("bind", "Repair: the token").text])
      .toEqual(["Update all: preparing", "Remove Bar: checking what it would do", "Repair: the token: checking what it would repair"])
    expect(M.activityStart({ id: 6, kind: "mutation", template: "remove", slots: { target: "retail", addon: "Bar" }, preview: true },
      NOW, null).rows).toEqual({})
  })

  test("how long it has run after 5 s; still working after 30 s without news", () => {
    const a = M.activityStart({ id: 7, resource: "check", kind: "data", lane: "network", label: "Check now" }, NOW, null)
    const at = (ms) => M.activityView(a, [], NOW + ms)
    expect([at(4999).elapsed, at(5000).elapsed, at(65000).elapsed]).toEqual(["", "5 s", "1 min 5 s"])
    expect([at(29999).stalled, at(30000).stalled]).toEqual(["", "Still working (no news for 30 s)"])
    const heard = M.activityProgress(a, events("check")[0], NOW + 40000)
    expect([M.activityView(heard, [], NOW + 65000).stalled, M.activityView(heard, [], NOW + 70000).stalled])
      .toEqual(["", "Still working (no news for 30 s)"])
    expect(M.durationText(125000)).toBe("2 min 5 s")
  })

  test("the bar shows a change first, then a check or read; it spins for changes and checks only", () => {
    const change = { kind: "change" }
    const check = { kind: "check" }
    const read = { kind: "read" }
    expect(M.shownActivity({ mutation: change, network: check, local: null })).toBe(change)
    expect(M.shownActivity({ mutation: null, network: read, local: null })).toBe(read)
    expect(M.shownActivity({ mutation: null, network: null, local: null })).toBeNull()
    const empty = M.emptyQueues()
    const waiting = { mutation: [], local: [], network: [{ resource: "check", priority: "background" }] }
    expect([
      M.barWorking({ mutation: change }, empty), M.barWorking({ network: check }, empty), M.barWorking({ network: read }, empty),
      M.barWorking({}, waiting), M.barWorking({}, empty), M.barWorking(null, null),
    ]).toEqual([true, true, false, true, false, false])
  })

  test("what waits is named: a change first, else a check", () => {
    expect(M.queuedText({ mutation: [{ priority: "background", label: "Settings checkpoint" }, { priority: "user", label: "Update Retail" }],
      network: [{ resource: "check" }], local: [] })).toBe("Next: Update Retail")
    expect(M.queuedText({ mutation: [], network: [{ resource: "search" }, { resource: "check" }], local: [] })).toBe("Next: Checking for updates")
    expect(M.queuedText(M.emptyQueues())).toBe("")
  })
})

// The Search view (1n2p.25, design doc §2.3): live search, pasted refs and links, rows
// that say what fits each client, the installed marker, and what `i` may install. Over
// the envelopes the binary writes: `search` (one client), `search-all` (Questie, `yes`
// on Classic Era and `no` on Retail, and a GitHub hit `unknown` on both) and `list`.
describe("the Search view", () => {
  const TARGETS = [{ id: "era", displayName: "Classic Era" }, { id: "retail", displayName: "Retail" }]
  const SEARCH_NOW = Date.parse("2026-09-03T00:00:00Z")
  const all = () => M.reduceSearch(fixture("search-all"))
  const rows = (extra) => M.searchRows(Object.assign({ rows: all().rows, filter: "", statusTargets: TARGETS, lists: null, nowMs: SEARCH_NOW }, extra || {}))
  // `[title, meta, chips, action, the cursored row's actions]`.
  const pick = (r) => [r.title, r.meta, r.chips.map((c) => c.text), r.action, r.actions.map((a) => a.name)]

  test("a result keeps its summary, author, age and popularity", () => {
    const skada = M.reduceSearch(fixture("search")).rows.find((r) => r.ref === "gh:Skada-Damage-Meter/Skada")
    expect([skada.summary, skada.author, skada.updatedAt, skada.stars, skada.hints])
      .toEqual(["A modular damage meter for World of Warcraft", "Skada-Damage-Meter", "2026-09-01T00:00:00Z", 412, { retail: "unknown" }])
    const [questie, extras] = all().rows
    expect([questie.name, questie.source, questie.hints]).toEqual(["Questie", "catalog", { era: "yes", retail: "no" }])
    expect([extras.name, extras.source, extras.hints]).toEqual(["QuestieExtras", "github", { era: "unknown", retail: "unknown" }])
  })

  test("each row says, in words, what fits the scope's client, or how many clients under All", () => {
    const cases = [
      ["era", "", [
        ["Questie", "Works on Classic Era · GitHub", ["checked by omaddons"], "install", ["install"]],
        ["QuestieExtras", "Not known yet for Classic Era · GitHub · 12 stars · updated 2 days ago · by someone", [], "check", ["enter"]]]],
      ["retail", "", [
        ["Questie", "Not made for Retail · GitHub", ["checked by omaddons"], "", []],
        ["QuestieExtras", "Not known yet for Retail · GitHub · 12 stars · updated 2 days ago · by someone", [], "check", ["enter"]]]],
      ["all", "", [
        ["Questie", "Works on 1 of 2 clients · GitHub", ["checked by omaddons"], "install", ["install"]],
        ["QuestieExtras", "Not known yet where it works · GitHub · 12 stars · updated 2 days ago · by someone", [], "check", ["enter"]]]],
    ]
    for (const [name, , expected] of cases) {
      const got = rows({ filter: name === "all" ? "" : name }).map(pick)
      expect([name, got]).toEqual([name, expected])
    }
    // The row's glyph is its best verdict's (§5.4): works, not known, not made for.
    expect(rows({ filter: "era" }).map((r) => r.glyph.codePointAt(0))).toEqual([0xF05E0, 0xF0625])
    expect(rows({ filter: "retail" })[0].glyph.codePointAt(0)).toBe(0xF02FD)
    // The summary is its own line; a hit without one has none.
    expect(M.searchRows({ rows: M.reduceSearch(fixture("search")).rows, filter: "retail", statusTargets: TARGETS, nowMs: SEARCH_NOW })
      .map((r) => r.detail)).toEqual(["", "", "A modular damage meter for World of Warcraft"])
    // One client on this computer: its own words, also under All.
    expect(rows({ statusTargets: [TARGETS[1]] })[0].meta.split(" · ")[0]).toBe("Not made for Retail")
    // Two clients it works on, and one it probably does.
    const three = TARGETS.concat([{ id: "forever", displayName: "WoW: Forever" }, { id: "anniversary", displayName: "Anniversary" }])
    const mixed = Object.assign({}, all().rows[0], { hints: { era: "yes", retail: "yes", forever: "likely", anniversary: "no" } })
    expect(M.searchRows({ rows: [mixed], statusTargets: three, nowMs: SEARCH_NOW })[0].meta.split(" · ")[0])
      .toBe("Works on 2 of 4 clients, probably on 1 more")
  })

  // bmcx.6: a result nobody has evidence for offers no install, only its page; one with
  // evidence installs only where its hint allows; `i` says why it installs nothing.
  test("install only where a hint allows, and say why everywhere else", () => {
    const [questie, extras] = rows()
    expect(questie.targets.map((t) => [t.id, t.words, t.eligible])).toEqual([
      ["era", "Works on Classic Era", true], ["retail", "Not made for Retail", false]])
    expect(extras.targets.every((t) => !t.eligible)).toBe(true)
    expect([extras.action, extras.actions.map((a) => a.tooltip)]).toEqual(["check", ["Check where it works (Enter)"]])
    expect(extras.note).toBe("Not known yet where it works: Enter opens its page, which checks.")
    expect(rows({ filter: "retail" })[0].note).toBe("Not made for Retail: nothing to install there.")
    expect(questie.actions[0].tooltip).toBe("Install (i)")
    // A missing client is never offered.
    const gone = TARGETS.concat([{ id: "classic", displayName: "Classic", missing: true }])
    expect(rows({ statusTargets: gone })[0].targets.map((t) => t.id)).toEqual(["era", "retail"])
  })

  // The installed marker, from `list -v`: a record of the same ref, or of the same name
  // ignoring case; a client that has it is never offered for an install.
  test("installed: by ref or by name, with its version on the scope's client", () => {
    const lists = M.reduceList(fixture("list"))
    const byName = Object.assign({}, all().rows[0], { name: "foo", ref: "wowi:1", hints: { era: "yes", retail: "yes" } })
    const byRef = Object.assign({}, all().rows[0], { name: "Something else", ref: "gh:o/baz", hints: { era: "yes", retail: "likely" } })
    const nowhere = Object.assign({}, all().rows[0], { name: "Questie", ref: "gh:Questie/Questie" })
    expect(M.installedOf(byName, lists)).toEqual([{ id: "era", version: "v1" }, { id: "retail", version: "v1" }])
    expect(M.installedOf(byRef, lists)).toEqual([{ id: "retail", version: "v1" }])
    expect(M.installedOf(nowhere, lists)).toEqual([])
    const shown = (filter) => M.searchRows({ rows: [byName, byRef, nowhere], filter, statusTargets: TARGETS, lists, nowMs: SEARCH_NOW })
    // (All three are copies of Questie, a catalog hit: the catalog's chip comes second.)
    expect(shown("retail").map((r) => [r.chips.map((c) => c.text), r.action, r.note])).toEqual([
      [["installed v1", "checked by omaddons"], "", "Already installed on Retail."],
      [["installed v1", "checked by omaddons"], "", "Already installed on Retail."],
      [["checked by omaddons"], "", "Not made for Retail: nothing to install there."]])
    expect(shown("").map((r) => [r.meta.split(" · ")[0], r.chips.map((c) => c.text), r.action, r.note])).toEqual([
      ["Works on 2 of 2 clients", ["installed on Classic Era and Retail", "checked by omaddons"], "", "Already installed where it works."],
      ["Works on 1 of 2 clients, probably on 1 more", ["installed on Retail", "checked by omaddons"], "install", ""],
      ["Works on 1 of 2 clients", ["checked by omaddons"], "install", ""]])
    expect(shown("")[1].targets.map((t) => [t.id, t.words, t.eligible])).toEqual([
      ["era", "Works on Classic Era", true], ["retail", "Already on Retail", false]])
    // Three clients or more are counted.
    const three = TARGETS.concat([{ id: "forever", displayName: "WoW: Forever" }])
    const everywhere = { targets: lists.targets.concat([{ id: "forever", key: "k", addons: [{ name: "Foo", ref: "gh:o/foo", version: "v1" }] }]) }
    expect(M.searchRows({ rows: [byName], statusTargets: three, lists: everywhere, nowMs: SEARCH_NOW })[0].chips[0].text).toBe("installed on 3 clients")
  })

  // A pasted ref or link is one row whose page resolves it; other text never is.
  test("refs and links from the hosts omaddons reads become one row; other text is searched", () => {
    const links = [
      ["https://github.com/WeakAuras/WeakAuras2", "github"],
      ["https://www.github.com/WeakAuras/WeakAuras2/releases/tag/5.19.2", "github"],
      ["https://www.wowinterface.com/downloads/info7032-TomTom.html", "wowi"],
      ["https://cdn.wowinterface.com/downloads/getfile.php?id=7032&aid=1", "wowi"],
      ["https://tukui.org/elvui", "tukui"],
      ["https://www.wowace.com/projects/bug-grabber", "wowace"],
      ["https://repos.wowace.com/wow/bug-grabber/tags/v12.1.1/", "wowace"],
      ["  gh:tullamods/OmniCC  ", "github"],
      // A paste's trailing newline is no part of it.
      ["https://github.com/o/r\n", "github"],
      ["wowi:7032", "wowi"],
      ["tukui:elvui", "tukui"],
      ["wowace:bug-grabber#r152", "wowace"],
      ["git:https://codeberg.org/o/r#main", "git"],
      ["url:https://github.com/o/r/releases/download/v1/x.zip", "url"],
    ]
    for (const [text, source] of links) expect([text, M.searchLink(text)]).toEqual([text, { source, text: text.trim() }])
    const words = ["weak auras", "dbm", "https://github.com/WeakAuras", "https://github.com", "http://github.com/o/r",
      "https://evil.example/o/r", "https://user@github.com/o/r", "https://github.com:8080/o/r", "https://github.com/o/r x",
      "https://github.com/o/r\nrm -rf ~", "https://github.com/o/r\u0007", "gh:owner", "gh:o/r/x", "GH:o/r", "wowi:abc", "url:http://github.com/x.zip", "git:codeberg.org/o/r",
      "javascript:alert(1)", "file:///etc/passwd", "cf:12345", "https://www.curseforge.com/wow/addons/details", "",
      "https://github.com/o/" + "r".repeat(600)]
    for (const text of words) expect([text, M.searchLink(text)]).toEqual([text, null])
    const [row] = M.searchRows({ link: M.searchLink("https://tukui.org/elvui"), rows: all().rows, statusTargets: TARGETS })
    expect([row.kind, row.title, row.meta, row.link, row.ref, row.action, row.actions.map((a) => a.tooltip)])
      .toEqual(["link", "Install from Tukui", "https://tukui.org/elvui", "https://tukui.org/elvui", "", "open", ["See where it works, then install (Enter)"]])
    expect(M.searchRows({ link: M.searchLink("gh:o/r"), rows: all().rows, statusTargets: TARGETS }).length).toBe(1)
  })

  test("typing searches from three characters, never a link", () => {
    expect([M.SEARCH_DEBOUNCE_MS, M.SEARCH_MIN_CHARS]).toEqual([400, 3])
    const cases = [["", false], ["ab", false], ["  ab  ", false], ["abc", true], ["weak auras", true], ["gh:o/r", false],
      ["https://github.com/o/r", false], ["https://github.com/o", true]]
    for (const [text, live] of cases) expect([text, M.searchesLive(text)]).toEqual([text, live])
  })

  // The view's states (design doc §7): the last query's rows stay under "Searching…"
  // while the next runs, with none of their notices; Try again where asking again helps.
  test("a newer query keeps the rows it replaces, and skipped sources say whether to retry", () => {
    const answer = { rows: [{ name: "DBM" }], selection: { query: "dbm", target: "" }, query: "dbm",
      skipped: [{ source: "wowi", reason: "no_index" }, { source: "github", reason: "rate_limited", reset_at: "2100-01-01T18:02:00Z" },
        { source: "tukui", reason: "disabled" }] }
    const state = (query, reading) => M.viewState("search", { service: { targets: [], search: answer, reading, outcomes: {} }, filter: "", query, rows: 1, nowMs: NOW })
    const kept = state("dbm bigwigs", ["search"])
    expect([kept.state, kept.refreshing, kept.notices]).toEqual(["rows", true, []])
    expect(M.refreshingText(kept.view)).toBe("Searching…")
    expect(M.refreshingText("updates")).toBe("Updating…")
    const own = state("dbm", [])
    expect(own.notices.map((n) => [n.sentence.replace(/until \d\d:\d\d/, "until HH:MM"), n.action ? n.action.text : null])).toEqual([
      ["WoWInterface couldn't be searched.", "Try again"],
      ["GitHub couldn't be searched: it asked to wait until HH:MM.", null],
      ["Tukui couldn't be searched: turned off.", null]])
  })
})

// Search with an empty box (1n2p.26): the built-in catalog for the scope, over the
// envelopes `search --catalog` (All) and `search --catalog -t era` write on a retail and
// Classic Era home.
describe("the Search view's catalog", () => {
  const TARGETS = [{ id: "era", displayName: "Classic Era" }, { id: "retail", displayName: "Retail" }]
  const catalog = (name) => M.reduceSearch(fixture(name)).rows
  const rows = (name, filter, lists) => M.catalogRows({ rows: catalog(name), filter, statusTargets: TARGETS, lists: lists || null, nowMs: NOW })

  test("one local read per scope, asking no source", () => {
    expect(M.catalogRequest("")).toEqual({ template: "catalog", slots: {} })
    expect(M.catalogRequest("era")).toEqual({ template: "catalog_target", slots: { target: "era" } })
    for (const [template, slots] of [["catalog", {}], ["catalog_target", { target: "era" }]]) {
      expect(M.COMMANDS[template].lane).toBe("local")
      const built = M.buildArgv(template, slots, opts()).argv
      expect([template, built.indexOf("--catalog") > 0, built.includes("--progress")]).toEqual([template, true, false])
    }
  })

  test("every entry that fits, by name, each with its verdict; the scope's own builds", () => {
    const all = rows("search-catalog", "")
    const names = all.map((r) => r.name)
    expect(names.length).toBeGreaterThan(40)
    expect([...names].sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase()))).toEqual(names)
    const questie = all.find((r) => r.name === "Questie")
    // No catalog chip: the view's header says every row is the catalog's.
    expect([questie.meta.split(" · ")[0], questie.action, questie.chips.map((c) => c.text)])
      .toEqual(["Works on 1 of 2 clients", "install", []])
    // One client: only what may load there, each in its words; BugGrabber is its build.
    const era = rows("search-catalog-era", "era")
    expect(era.map((r) => r.targets.find((t) => t.id === "era").hint)).not.toContain("no")
    expect(era.find((r) => r.name === "Questie").meta.split(" · ")[0]).toBe("Works on Classic Era")
    expect(era.find((r) => r.name === "BugGrabber").ref).toBe("wowace:bug-grabber")
  })

  test("what is installed here comes last, each part kept in name order", () => {
    const lists = { targets: [{ id: "era", key: "k", addons: [{ name: "Questie", ref: "gh:Questie/Questie", version: "11.0" }] }] }
    const era = rows("search-catalog-era", "era", lists)
    const last = era[era.length - 1]
    expect([last.name, last.installed, last.chips[0].text, last.action]).toEqual(["Questie", true, "installed 11.0", ""])
    const rest = era.slice(0, -1).map((r) => r.name)
    expect(rest.every((n) => n !== "Questie")).toBe(true)
    expect([...rest].sort((a, b) => a.toLowerCase().localeCompare(b.toLowerCase()))).toEqual(rest)
    // Under All it is installed only where nothing is left to install: Questie works on
    // Classic Era alone, which has it.
    expect(rows("search-catalog", "", lists).slice(-1)[0].name).toBe("Questie")
  })

  test("an empty box shows the catalog's rows, or what to type, never a search's state", () => {
    const service = { targets: [], reading: ["search"], outcomes: { search: "failed" },
      search: { rows: [], skipped: [{ source: "wowi", reason: "no_index" }], selection: { query: "dbm", target: "" } } }
    const shown = M.viewState("search", { service, filter: "", query: "", rows: 12, nowMs: NOW })
    expect([shown.state, shown.refreshing, shown.notices, shown.sentence]).toEqual(["rows", false, [], ""])
    const none = M.viewState("search", { service, filter: "", query: "", rows: 0, nowMs: NOW })
    expect([none.state, none.sentence]).toEqual(["empty", "Type a name, or paste a link from GitHub, WoWInterface, Tukui or WowAce."])
  })
})
