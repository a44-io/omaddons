// The words guard (design doc §6.2, omad-e49b): every word a player reads lives in
// Model.js's COPY, or ERROR_WORDS and WARNING_WORDS for a code's words, formatted by
// copy(). The four rules are copy_guard.js's; this runs them over every runtime file
// under omarchy/ (tests/ and a generated Build.js aside) and over the binary's catalog of
// codes (contract-errors.json, from step 1 of the pipeline), and holds what they find to
// copy_allowlist.txt: what predates the guard, which only shrinks. Each rule also runs
// over an in-memory copy with one planted violation, which it must report.

const { describe, expect, test } = require("bun:test")
const fs = require("fs")
const path = require("path")
const G = require("./copy_guard.js")
const M = require("../Model.js")

const ROOT = path.join(__dirname, "..")
const ALLOWLIST = path.join(__dirname, "copy_allowlist.txt")
const CATALOG = path.join(process.env.OMADDONS_WIDGET_OUT || "", "fixtures", "contract-errors.json")

// The prefixes the code builds COPY ids from (`"card.action." + name`). A new one is
// added here on purpose: every id under it counts as used.
const DYNAMIC_PREFIXES = [
  "activity.done.", "activity.phase.", "activity.verb.", "backup.kind.", "backup.kind.auto.",
  "backup.kind.short.", "backup.proof.", "backups.page.", "card.", "card.action.", "channel.",
  "details.outcome.", "failure.", "history.actor.", "history.change.", "history.change.whole.",
  "history.title.", "installed.sort.", "issues.note.", "issues.severity.", "issues.suggest.",
  "page.section.", "page.versions.channel.", "page.versions.why.", "page.why.", "search.link.source.",
  "settings.family.", "settings.github.state.", "settings.omaddons.", "settings.panel.",
  "share.group.", "share.item.failed.", "share.paste.", "share.paste.invalid.",
  "sheet.adopt.identified.", "sheet.adopt.why.",
  "sheet.change.", "sheet.confirm.", "sheet.item.", "sheet.title.", "source.", "stale.changed.", "stale.repair.",
  "state.action.", "state.failed.", "state.loading.", "state.notice.skipped.", "state.what.",
  "updates.kind."
]

// Every runtime .qml and .js under omarchy/, as the binary embeds them.
function runtimeFiles() {
  const out = {}
  const walk = (dir) => {
    for (const entry of fs.readdirSync(path.join(ROOT, dir), { withFileTypes: true })) {
      const rel = dir ? dir + "/" + entry.name : entry.name
      if (entry.isDirectory()) {
        if (rel !== "tests") walk(rel)
      } else if (/\.(qml|js)$/.test(entry.name) && rel !== "Build.js") {
        out[rel] = fs.readFileSync(path.join(ROOT, rel), "utf8")
      }
    }
  }
  walk("")
  return out
}

const REAL = runtimeFiles()
const COMMAND_PATHS = Object.keys(M.CLI_PATHS)

function catalog() {
  return JSON.parse(fs.readFileSync(CATALOG, "utf8"))
}

// Every finding of the four rules over `files` and `codes`.
function findings(files, codes, errorWords, warningWords) {
  return [
    ...G.qmlFindings(files),
    ...G.modelFindings(files, COMMAND_PATHS),
    ...G.unusedFindings(files),
    ...G.codeFindings(codes, errorWords, warningWords)
  ]
}

function show(finding) {
  return finding.file + (finding.line ? ":" + finding.line : "") + " " + finding.rule + " " + JSON.stringify(finding.value)
}

// `files` with `insertion` put after the first `anchor` in `name`.
function plant(files, name, anchor, insertion) {
  const text = files[name]
  const at = text.indexOf(anchor)
  if (at < 0) throw new Error(name + " has no " + JSON.stringify(anchor) + " to plant after")
  return Object.assign({}, files, { [name]: text.slice(0, at + anchor.length) + insertion + text.slice(at + anchor.length) })
}

describe("the words guard over the real source", () => {
  const found = findings(REAL, catalog(), M.ERROR_WORDS, M.WARNING_WORDS)
  const listed = G.parseAllowlist(fs.readFileSync(ALLOWLIST, "utf8"))

  test("every finding predates the guard (copy_allowlist.txt)", () => {
    const allowed = new Set(listed)
    const fresh = found.filter((f) => !allowed.has(G.key(f))).map(show)
    // Put the words in COPY (copy(id, slots)), or a code's words in ERROR_WORDS or
    // WARNING_WORDS; never add a line to the allowlist.
    expect(fresh).toEqual([])
  })

  test("the allowlist only shrinks: each line still matches what it allows", () => {
    const now = new Set(found.map(G.key))
    // The words moved to COPY, or the id is used or gone: delete the line.
    expect(listed.filter((line) => !now.has(line))).toEqual([])
  })

  test("the allowlist is sorted, each line once", () => {
    expect(listed).toEqual([...new Set(listed)].sort())
  })

  test("the prefixes ids are built from are the listed ones", () => {
    expect([...G.namesOutsideCopy(REAL).prefixes].sort()).toEqual([...DYNAMIC_PREFIXES].sort())
  })

  test("the catalog of codes was read", () => {
    const codes = catalog().data
    expect(codes.error_codes.length).toBeGreaterThan(10)
    expect(codes.warning_codes.length).toBeGreaterThan(10)
  })

  test("COPY's ids are all read, and the word tables exist", () => {
    expect(G.copyIds(REAL).sort()).toEqual(Object.keys(M.COPY).sort())
    expect(typeof M.ERROR_WORDS).toBe("object")
    expect(typeof M.WARNING_WORDS).toBe("object")
  })
})

describe("what nothing ran says (previewDecision, doctorPreviewDecision)", () => {
  test("in the glossary's words: never plan, preview, token or precondition", () => {
    const ids = Object.keys(M.COPY).filter((id) => id.startsWith("decision."))
    // Five: finding nothing to do says nothing (omad-vqkd).
    expect(ids.length).toBe(5)
    for (const id of ids) {
      expect([id, /\b(plans?|previews?|tokens?|preconditions?)\b/i.test(M.copy(id))]).toEqual([id, false])
    }
  })

  test("no word of COPY says plan, preview or precondition (glossary §6.3)", () => {
    // "token" stays: the GitHub token is the player's own, set in Settings.
    const said = (entry) => (typeof entry === "string" ? [entry] : [entry.one, entry.other])
    const offending = Object.keys(M.COPY).filter((id) =>
      said(M.COPY[id]).some((text) => /\b(plans?|previews?|preconditions?)\b/i.test(String(text))))
    expect(offending).toEqual([])
  })

  test("the decisions read their lines from COPY", () => {
    const src = REAL["Model.js"]
    for (const name of ["previewDecision", "doctorPreviewDecision"]) {
      const start = src.indexOf("function " + name + "(")
      const body = src.slice(start, src.indexOf("\n}\n", start))
      const lines = [...body.matchAll(/decision\.line = ([^\n]+)/g)].map((m) => m[1])
      expect(lines.length).toBeGreaterThan(0)
      for (const line of lines) expect([name, line.startsWith("copy(")]).toEqual([name, true])
    }
  })
})

describe("each rule catches a planted violation", () => {
  const codes = { data: { error_codes: [{ code: "usage" }], warning_codes: [{ code: "plan_not_stored" }] } }
  const quiet = { usage: "not met" }
  const quietWarnings = { plan_not_stored: "not met" }
  const keys = (files, c, e, w) => new Set(findings(files, c || codes, e || quiet, w || quietWarnings).map(G.key))
  const before = keys(REAL)
  const added = (files, c, e, w) => [...keys(files, c, e, w)].filter((k) => !before.has(k))

  test("qml-literal: a literal a player reads, on one line or continued", () => {
    const item = (binding) => "  Text {\n    " + binding + "\n  }\n"
    const one = plant(REAL, "PanelView.qml", "\n", item('text: "Planted words"'))
    expect(added(one)).toEqual(['qml-literal\tPanelView.qml\t"Planted words"'])
    const capital = plant(REAL, "PanelView.qml", "\n", item('tooltipText: Model.withKey("Planted", "help")'))
    expect(added(capital)).toEqual(['qml-literal\tPanelView.qml\t"Planted"'])
    const continued = plant(REAL, "PanelView.qml", "\n", item('text: root.planted\n      ? "planted two"\n      : ""'))
    expect(added(continued)).toEqual(['qml-literal\tPanelView.qml\t"planted two"'])
    const heading = plant(REAL, "PanelView.qml", "\n", item('heading: root.planted ? "Planted" : "also planted"'))
    expect(added(heading).sort()).toEqual(['qml-literal\tPanelView.qml\t"Planted"', 'qml-literal\tPanelView.qml\t"also planted"'])
    const ids = plant(REAL, "PanelView.qml", "\n", item('text: Model.copy("planted.id", { "key": "x" }) + String.fromCodePoint(0xF0026)'))
    expect(added(ids)).toEqual([])
    // What a screen reader says is read too (1n2p.40).
    const spoken = plant(REAL, "PanelView.qml", "\n", item('Accessible.name: "Planted name"\n    Accessible.description: root.planted ? "Planted" : ""'))
    expect(added(spoken).sort()).toEqual(['qml-literal\tPanelView.qml\t"Planted name"', 'qml-literal\tPanelView.qml\t"Planted"'])
  })

  test("model-literal: two words outside COPY, but not a command path or a diagnostic", () => {
    const prose = plant(REAL, "Model.js", "\n", 'var PLANTED = "two planted words"\n')
    expect(added(prose)).toEqual(['model-literal\tModel.js\t"two planted words"'])
    const behindRegex = plant(REAL, "Model.js", "\n", 'var PLANTED_RE = /"[a b]/g; var PLANTED = "after a regex"\n')
    expect(added(behindRegex)).toEqual(['model-literal\tModel.js\t"after a regex"'])
    const exempt = plant(REAL, "Model.js", "\n",
      'var PLANTED_PATH = "backup restore"\nfunction planted() { throw refuse("planted", "a diagnostic sentence") }\n// "a comment sentence"\n')
    expect(added(exempt)).toEqual([])
    const inCopy = plant(REAL, "Model.js", "var COPY = {\n", '  "planted.words": "Planted words in COPY",\n')
    expect(added(inCopy)).toEqual(['unused-id\tModel.js\t"planted.words"'])
  })

  test("unused-id: an id nothing names, used once a file names it", () => {
    const unused = plant(REAL, "Model.js", "var COPY = {\n", '  "planted.unused": "Planted",\n')
    expect(added(unused)).toEqual(['unused-id\tModel.js\t"planted.unused"'])
    const used = plant(unused, "PanelView.qml", "\n", '  Text {\n    text: Model.copy("planted.unused")\n  }\n')
    expect(added(used)).toEqual([])
    const pair = plant(REAL, "Model.js", "var COPY = {\n", '  "planted.pair": {\n    "one": "1 planted",\n    "other": "{n} planted"\n  },\n')
    expect(added(pair)).toEqual(['unused-id\tModel.js\t"planted.pair"'])
  })

  test("code-without-words: a code of the catalog with no words", () => {
    const more = { data: { error_codes: [{ code: "usage" }, { code: "planted_error" }], warning_codes: [{ code: "planted_warning" }] } }
    expect(added(REAL, more).sort()).toEqual(['code-without-words\terror\t"planted_error"', 'code-without-words\twarning\t"planted_warning"'])
    expect(added(REAL, more, { usage: "not met", planted_error: () => ({}) }, { planted_warning: "not met" })).toEqual([])
  })
})

describe("the lexer", () => {
  test("strings, comments, regex literals and calls", () => {
    const src = [
      'var a = "one" // "not a string"',
      "var b = 'two' /* \"nor this\" */",
      'var c = x / 2 / y; var d = /"/g; var e = "three"',
      'refuse("code", "four " + Model.copy("five"))'
    ].join("\n")
    const seen = G.literals(src).map((l) => [l.value, l.line, l.calls.join(">")])
    expect(seen).toEqual([
      ["one", 1, ""],
      ["two", 2, ""],
      ["three", 3, ""],
      ["code", 4, "refuse"],
      ["four ", 4, "refuse"],
      ["five", 4, "refuse>copy"]
    ])
  })

  test("words: pieces with a letter", () => {
    for (const [text, n] of [["Update all", 2], ["--if-match", 1], [" · ", 0], ["12.1.15", 0], ["don't stop", 2], ["a\nb", 2]]) {
      expect([text, G.words(text)]).toEqual([text, n])
    }
  })
})
