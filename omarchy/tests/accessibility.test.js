// The accessibility pass (1n2p.40, design doc §5.1): text and marks meet WCAG AA on the
// polish pass's four themes, from their tokens; and no state is told by colour alone.
// The colour checks run over the colours as the widget composes them (Model.dimColor,
// tintedSurface, badgeColors) and over the QML that picks them; the colour-alone checks
// over the Model's rows, the bar's model and every state-switched colour binding of the
// runtime QML, each registered here with what tells its state besides the colour. The
// names and roles (every control a screen reader can reach) are the QML snapshot
// harness's walk (tst_snapshots.qml, unnamedControls).

const { describe, expect, test } = require("bun:test")
const fs = require("fs")
const path = require("path")
const M = require("../Model.js")

const ROOT = path.join(__dirname, "..")
const OUT = process.env.OMADDONS_WIDGET_OUT
if (!OUT) throw new Error("OMADDONS_WIDGET_OUT is unset: run the widget pipeline (argc widget-test)")
const FIXTURES = path.join(OUT, "fixtures")
const NOW = Date.parse("2100-01-01T00:00:00Z")

function fixture(name) {
  return JSON.parse(fs.readFileSync(path.join(FIXTURES, name + ".json"), "utf8"))
}

// Every runtime .qml under omarchy/ (tests/ aside), as the binary embeds them.
function qmlFiles() {
  const out = {}
  const walk = (dir) => {
    for (const entry of fs.readdirSync(path.join(ROOT, dir), { withFileTypes: true })) {
      const rel = dir ? dir + "/" + entry.name : entry.name
      if (entry.isDirectory()) {
        if (rel !== "tests") walk(rel)
      } else if (entry.name.endsWith(".qml")) {
        out[rel] = fs.readFileSync(path.join(ROOT, rel), "utf8")
      }
    }
  }
  walk("")
  return out
}

const QML = qmlFiles()

// ------------------------------------------------------------------ contrast

// The polish pass's four themes (1n2p.39: two dark, one light, one saturated), the
// tokens the kit's Color reads from each colors.toml (2026-11-10): `urgent` is its
// `red`. None ships a shell.toml, so the popup's background is the theme's.
const THEMES = {
  "catppuccin": { fg: "#cdd6f4", bg: "#1e1e2e", accent: "#89b4fa", urgent: "#f38ba8" },
  "gruvbox": { fg: "#d4be98", bg: "#282828", accent: "#7daea3", urgent: "#ea6962" },
  "catppuccin-latte": { fg: "#4c4f69", bg: "#eff1f5", accent: "#1e66f5", urgent: "#d20f39" },
  "hackerman": { fg: "#ddf7ff", bg: "#0B0C16", accent: "#82FB9C", urgent: "#50f872" },
}

const TEXT_MIN = 4.5 // WCAG AA, text
const MARK_MIN = 3 // WCAG AA, non-text: glyphs, bands, borders, fills

// The kit's selected fill: the foreground at `selected-fill-alpha`, 0.18 by default
// (the selected tab).
const SELECTED_TINT_ALPHA = 0.18

// What the widget lays over the popup's background, as it composes it: `{name: colour}`.
function surfaces(t) {
  const failure = M.tintedSurface(t.urgent, t.bg, 0.10)
  const success = M.tintedSurface(t.accent, t.bg, 0.08)
  return {
    popup: t.bg,
    cursor: M.mixColor(t.fg, t.bg, M.CURSOR_TINT_ALPHA),
    selected: M.mixColor(t.fg, t.bg, SELECTED_TINT_ALPHA),
    failure,
    failureCursor: M.mixColor(t.fg, failure, M.CURSOR_TINT_ALPHA),
    success,
  }
}

// Every pair of colours the widget draws, `[what, on, minimum, foreground, background]`.
function pairs(t) {
  const s = surfaces(t)
  const out = []
  // The foreground is text on every surface (a selected tab's label and count, a
  // card's title, a cursored row's title, an AlertLine's words).
  for (const [name, on] of Object.entries(s)) out.push(["foreground", name, TEXT_MIN, t.fg, on])
  // Secondary text: the plain dim on the popup and under the cursor; a card's dim on
  // its tint (PanelView's cardDim, NoticeCard's dim from its surface).
  const dim = M.dimColor(t.fg, t.bg)
  out.push(["dim", "popup", TEXT_MIN, dim, s.popup], ["dim", "cursor", TEXT_MIN, dim, s.cursor])
  const failureDim = M.dimColor(t.fg, s.failure)
  out.push(["failure dim", "failure", TEXT_MIN, failureDim, s.failure], ["failure dim", "failureCursor", TEXT_MIN, failureDim, s.failureCursor])
  out.push(["success dim", "success", TEXT_MIN, M.dimColor(t.fg, s.success), s.success])
  // The bar's badge: its figures on its fill.
  for (const kind of ["urgent", "accent"]) {
    const badge = M.badgeColors(t[kind], t.bg, t.fg)
    out.push(["badge figures", kind + " badge", TEXT_MIN, badge.text, badge.fill])
  }
  // Marks: the urgent and accent glyphs, bands, borders and fills, on every surface
  // they are drawn on; the bar's mark glyph on its disc.
  for (const name of ["popup", "cursor", "failure", "failureCursor"]) out.push(["urgent mark", name, MARK_MIN, t.urgent, s[name]])
  for (const name of ["popup", "cursor", "selected", "success"]) out.push(["accent mark", name, MARK_MIN, t.accent, s[name]])
  out.push(["mark glyph", "urgent disc", MARK_MIN, t.bg, t.urgent], ["mark glyph", "foreground disc", MARK_MIN, t.bg, t.fg])
  return out
}

describe("contrast: AA on the polish pass's four themes, from their tokens", () => {
  for (const [theme, t] of Object.entries(THEMES)) {
    test(theme, () => {
      const failing = pairs(t)
        .map(([what, on, min, a, b]) => ({ what, on, min, ratio: M.contrastRatio(a, b) }))
        .filter((p) => p.ratio < p.min)
        .map((p) => p.what + " on " + p.on + ": " + p.ratio.toFixed(2) + " < " + p.min)
      expect(failing).toEqual([])
    })
  }

  test("the badge keeps the theme's colour where it reads, and moves only as far as it must", () => {
    // Catppuccin's accent reads under the background's colour: kept as it is.
    const kept = M.badgeColors(THEMES.catppuccin.accent, THEMES.catppuccin.bg, THEMES.catppuccin.fg)
    expect(kept).toEqual({ fill: "#89b4fa", text: THEMES.catppuccin.bg })
    // Catppuccin Latte's accent gives 4.34:1 under its background: nudged toward the
    // foreground, two steps (one gives less than 4.5:1).
    const latte = THEMES["catppuccin-latte"]
    const nudged = M.badgeColors(latte.accent, latte.bg, latte.fg)
    expect(nudged.text).toBe(latte.bg)
    expect(M.contrastRatio(latte.bg, M.mixColor(latte.fg, latte.accent, 0.05))).toBeLessThan(TEXT_MIN)
    expect(nudged.fill).toBe(M.mixColor(latte.fg, latte.accent, 0.10))
    expect(M.contrastRatio(nudged.text, nudged.fill)).toBeGreaterThanOrEqual(TEXT_MIN)
    // QML hands colours over as {r, g, b} objects; they read too.
    const asObject = M.badgeColors({ r: 0.5373, g: 0.7059, b: 0.9804 }, "#1e1e2e", "#cdd6f4")
    expect(asObject.fill).toBe("#89b4fa")
  })

  // Urgent and accent are marks' colours, never text's (§5.1): as text they fall below
  // 4.5:1 on some themes (urgent 2.30 on miasma, accent 4.34 on catppuccin-latte). A
  // Text or PlainLabel in either colour must draw a glyph; a button's label is never in
  // either (its words go beside an urgent glyph or border instead, AlertLine, 1n2p.40).
  test("urgent and accent colour glyphs only, never words", () => {
    expect(wordsInMarkColours(QML)).toEqual([])
    const planted = Object.assign({}, QML)
    planted["views/UpdatesView.qml"] += "\n    PlainLabel {\n      color: root.failed ? root.urgent : root.dim\n      text: root.why\n    }\n"
    expect(wordsInMarkColours(planted).length).toBe(1)
  })
})

// The Text, PlainLabel and Button blocks of `files` whose words are drawn in a mark's
// colour: a Text or PlainLabel that draws no glyph, or any Button's label.
function wordsInMarkColours(files) {
  const tokens = /\b(urgent|accent|urgentColor|activeColor|bandColor|glyphColor)\b/
  const found = []
  for (const [name, src] of Object.entries(files)) {
    const lines = src.split("\n")
    for (let i = 0; i < lines.length; i++) {
      const open = /^(\s*)(Text|PlainLabel|Button)\s*\{\s*$/.exec(lines[i])
      if (!open) continue
      // The block's own lines, its children's aside (one indent deeper than its props).
      const inner = open[1] + "  "
      const own = []
      for (let j = i + 1; j < lines.length && !lines[j].startsWith(open[1] + "}"); j++) {
        if (lines[j].startsWith(inner) && !lines[j].startsWith(inner + " ")) own.push(lines[j].trim())
      }
      const colour = own.find((l) => /^(color|foreground)\s*:/.test(l)) || ""
      const glyph = own.some((l) => /^text\s*:.*(fromCodePoint|glyph|Glyph)/.test(l))
      // The colours it may take: a conditional's condition is no colour.
      const binding = colour.replace(/^(color|foreground)\s*:\s*/, "")
      const values = binding.includes("?") ? binding.split("?").slice(1).join(" ") : binding
      if (colour !== "" && tokens.test(values) && (open[2] === "Button" || !glyph)) {
        found.push(name + ":" + (i + 1) + " " + open[2] + " " + colour)
      }
    }
  }
  return found
}

// ------------------------------------------------------------------ colour alone

// Every binding of the runtime QML that picks a colour by a state, and what tells that
// state besides the colour (design doc §5.1, "never colour alone"). A new one fails
// until it is added here with its cue; one changed or gone fails until this is
// updated. The key is the file and the binding as written, trimmed.
const COLOUR_STATES = {
  "ui/AddonRow.qml\treadonly property color bandColor: band === \"urgent\" ? urgent : band === \"accent\" ? accent : \"transparent\"":
    "a row's band: its state glyph names the state, and its chips or meta say it (rows, below)",
  "ui/AddonRow.qml\tcolor: root.band !== \"\" ? root.bandColor : root.dim":
    "a row's state glyph in its band's colour: the glyph names the state, and the row's chips or meta say it (rows, below)",
  "ui/NoticeCard.qml\treadonly property color surface: kind === \"failure\" ? Model.tintedSurface(urgent, Commons.Color.popups.background, 0.10)":
    "the card's tint, under its kind's glyph and title (its dim is computed on it)",
  "ui/Chip.qml\tborder.color: urgent ? urgentColor : muted":
    "an urgent chip carries the alert glyph (chipGlyph) beside its words",
  "ui/TabCount.qml\tborder.color: urgent ? urgentColor : muted":
    "an urgent tab count's figures are bold (countText), and its accessible name says it needs attention",
  "ui/TabCount.qml\tcolor: root.urgent ? root.foreground : root.dim":
    "the figures themselves, bold when urgent",
  "ui/Chip.qml\tcolor: root.urgent ? root.foreground : root.dim":
    "an urgent chip's words, beside its alert glyph",
  "BarWidget.qml\tcolor: root.model.markUrgent === true ? button.activeColor : button.foreground":
    "each mark has its own glyph (Model.BAR_MARK_GLYPHS: broken, stale, attention, running)",
  "ui/PlanSheet.qml\tcolor: effect.urgent ? root.urgent : root.dim":
    "each effect line has its own glyph and its sentence (\"Also installs LibFoo\")",
  "ui/NoticeCard.qml\tcolor: kind === \"failure\" ? alpha(urgent, 0.10) : kind === \"success\" ? alpha(accent, 0.08) : \"transparent\"":
    "each kind has its own glyph (noticeGlyph: alert, check, information) and a title in words",
  "ui/NoticeCard.qml\tborderSpec: Border.flat(kind === \"failure\" ? alpha(urgent, 0.35) : kind === \"success\" ? alpha(accent, 0.35) : muted, 1)":
    "the same kind's glyph and title",
  "ui/NoticeCard.qml\treadonly property color glyphColor: kind === \"failure\" ? urgent : kind === \"success\" ? accent : dim":
    "the glyph's shape differs by kind",
  "PanelView.qml\tcolor: root.heroState === \"urgent\" ? root.urgent : (root.heroState === \"busy\" ? root.accent : root.dim)":
    "the hero's line says it in words, urgent first (\"2 NEW ERRORS · 1 PROBLEM\"), and the activity bar names what runs",
  "views/IssuesView.qml\tcolor: entry.modelData.band === \"urgent\" ? root.urgent : root.dim":
    "the problem's glyph (broken, needs attention) and its severity word (rows, below)",
  "views/HistoryView.qml\tforeground: entry.modelData.state === \"undoable\" || entry.modelData.state === \"redo\" ? root.foreground : root.dim":
    "a change undone, or one undo never reverts, says so in its chips and its why (\"undone\")",
  "views/AddonPage.qml\tforeground: modelData.state === \"undone\" ? root.dim : root.foreground":
    "an undone change's words say \"undone\" (page.change.undone)",
  "views/ErrorPage.qml\tcolor: modelData.own ? root.foreground : root.dim":
    "the addon's own stack frames are bold too",
  "ui/AddonRow.qml\tcolor: root.pending ? root.foreground : root.dim":
    "a pending row's meta is replaced by its verb (\"Updating… downloading (2 of 3)\") and its glyph by the spinner",
  "ui/Badge.qml\treadonly property var colors: Model.badgeColors(urgent ? urgentColor : accent, background, foreground)":
    "an urgent count comes with the attention mark under it (bar, below), or one that outranks it",
  "views/InstalledView.qml\tcolor: root.count.none ? root.foreground : root.dim":
    "\"0 of 42\" says it, bold",
  "views/HistoryView.qml\tcolor: root.count.none ? root.foreground : root.dim":
    "\"0 of 42\" says it, bold",
  "views/UpdatesView.qml\tcolor: root.rows.length === 0 && root.scopeRows.length > 0 ? root.foreground : root.dim":
    "\"0 of 5\" says it, bold",
  "views/IssuesView.qml\tcolor: entry.modelData.urgent ? root.foreground : root.dim":
    "the severity word itself, beside the problem's glyph",
  "ui/AlertLine.qml\tcolor: root.marked ? root.foreground : root.dim":
    "a marked line carries the alert glyph (alertGlyph)",
  "ui/TabStrip.qml\tdim: tab.selected ? root.foreground : Model.dimColor(root.foreground, Commons.Color.popups.background)":
    "the selected tab is its view, named by the content and Accessible.selected; this keeps its figures readable",
}

// The state-switched colour bindings of `files`: a property that names a colour (or a
// colour model) bound to a conditional, as `file \t binding`. A frame's fallback
// (`frame ? frame.urgent : Commons.Color.urgent`) is no state.
function colourStates(files) {
  const property = /^\s*((readonly\s+)?property\s+(color|var)\s+\w+|[\w.]*(color|Color|foreground|borderSpec|dim)|dim)\s*:\s*(.*)$/
  const out = []
  for (const [name, src] of Object.entries(files)) {
    for (const line of src.split("\n")) {
      const m = property.exec(line)
      if (!m || !m[5].includes("?")) continue
      if (/^\s*(frame|root\.bar|root\.frame)\s*\?/.test(m[5]) && !/urgent\s*\?|accent\s*\?/.test(m[5])) continue
      if (!/\b(urgent|accent|urgentColor|activeColor|bandColor|dim|muted|foreground)\b/.test(m[5])) continue
      out.push(name + "\t" + line.trim())
    }
  }
  return out.sort()
}

describe("colour alone: every coloured state is told some other way too", () => {
  test("every state-switched colour of the QML is registered with its cue", () => {
    const found = colourStates(QML)
    expect(found.filter((k) => !Object.prototype.hasOwnProperty.call(COLOUR_STATES, k))).toEqual([])
    expect(Object.keys(COLOUR_STATES).filter((k) => !found.includes(k)).sort()).toEqual([])
  })

  test("the scan finds a planted colour-only state", () => {
    const planted = Object.assign({}, QML)
    planted["views/UpdatesView.qml"] += "\nText {\n  color: root.planted ? root.urgent : root.foreground\n}\n"
    expect(colourStates(planted).filter((k) => !COLOUR_STATES.hasOwnProperty(k)))
      .toEqual(["views/UpdatesView.qml\tcolor: root.planted ? root.urgent : root.foreground"])
  })

  test("Addons rows: an urgent band has an urgent chip in words, an accent band the update chip", () => {
    const env = JSON.parse(JSON.stringify(fixture("list-markers")))
    const rows = M.installedRows(M.reduceList(env), "").concat(M.installedRows(M.reduceList(fixture("list")), ""))
    expect(rows.some((r) => r.band === "urgent")).toBe(true)
    for (const r of rows) {
      if (r.band === "urgent") expect(r.chips.some((c) => c.urgent && c.text !== ""), r.name).toBe(true)
      if (r.band === "accent") expect(r.chips.map((c) => c.text), r.name).toContain(M.copy("installed.chip.update"))
      if (r.band !== "") expect(r.glyph, r.name).toBeGreaterThan(0)
    }
  })

  test("Updates rows: each banded kind has a glyph of its own", () => {
    const banded = Object.keys(M.UPDATE_BANDS).filter((k) => M.UPDATE_BANDS[k] !== "")
    const glyphs = Object.keys(M.UPDATE_GLYPHS).map((k) => M.UPDATE_GLYPHS[k])
    expect(new Set(glyphs).size).toBe(glyphs.length)
    for (const kind of banded) expect(M.UPDATE_GLYPHS[kind], kind).toBeGreaterThan(0)
  })

  test("Issues rows: an urgent band has its severity word and glyph", () => {
    const status = M.reduceStatus(fixture("status-attention"), NOW)
    const rows = M.issueRows(M.issueSections(status.targets, status.warnings || [], "", status.issues || [], status.commands || []))
    expect(rows.some((r) => r.band === "urgent")).toBe(true)
    for (const r of rows.filter((r) => r.band === "urgent")) {
      expect(r.severityWord, r.title).not.toBe("")
      expect(r.glyph, r.title).toBeGreaterThan(0)
    }
  })

  test("Errors rows: an urgent band says how many are new", () => {
    const status = M.reduceStatus(fixture("status-errors"), NOW)
    const at = Date.parse("2026-10-15T18:29:00Z") + 3 * 3600000
    const rows = M.errorRows(M.errorSections(M.reduceErrors(fixture("errors")), status.targets, "", at))
    expect(rows.some((r) => r.band === "urgent")).toBe(true)
    for (const r of rows.filter((r) => r.band === "urgent")) {
      expect(r.meta, r.addon).toContain(M.copy("errors.new", { n: r["new"] }))
    }
  })

  test("the bar: an urgent glyph or badge always has a mark, attention or one that outranks it", () => {
    const statuses = ["status-attention", "status-errors", "status-updates", "status-idle", "status-running"]
      .map((name) => M.reduceStatus(fixture(name), NOW))
    const contexts = []
    for (const phase of ["starting", "ok", "unreachable", "missing", "incompatible"]) {
      for (const stale of [false, true]) {
        contexts.push({ phase, working: false, nowMs: NOW, answeredAtMs: stale ? NOW - 3600000 : NOW, staleAfterMs: 60000 })
      }
    }
    let urgentSeen = 0
    for (const status of statuses.concat([null])) {
      for (const ctx of contexts) {
        const bar = M.barState(status, ctx)
        if (bar.urgent || bar.badgeUrgent) {
          urgentSeen++
          expect(["attention", "stale", "broken"], JSON.stringify([bar.state, ctx.phase])).toContain(bar.mark)
        }
        // Each mark draws its own glyph.
        if (bar.mark !== "") expect(M.BAR_MARK_GLYPHS[bar.mark]).toBeGreaterThan(0)
        // Said, not only seen: the bar's name is its tooltip's first line.
        expect(M.barAccessibleName(bar)).toBe(M.copy("bar.accessible.summary", { summary: bar.tooltip[0] }))
      }
    }
    expect(urgentSeen).toBeGreaterThan(0)
    expect(new Set(Object.values(M.BAR_MARK_GLYPHS)).size).toBe(Object.keys(M.BAR_MARK_GLYPHS).length)
  })
})
