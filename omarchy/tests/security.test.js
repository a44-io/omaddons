// The widget's security scans (F7, pipeline step 2): text lints over every runtime file
// under omarchy/ (tests aside) that fail when a code path goes around a mechanism that
// keeps the widget safe. The mechanisms are the guarantees (PlainLabel, Model.buildArgv,
// Model.cliArgv with its classifier, the stamped context, the preview flow, a plan's
// apply only from Model.planFrom); the scans only catch an accidental way around them,
// so each one also runs over an in-memory copy with one planted violation, which it
// must report. Checks 3b, 9 and 10 (M4-D28, M4-D48) hold the plan runner: an apply argv
// only from the preview the widget just ran, no decision from a warning's text, and no
// issue matched by its id. Check 11 (M4-D38, M4-D44) keeps consent a person's: no
// template or literal gives `--force` or `--yes`, the widget never runs `doctor gc` or
// `doctor undo`, a repair runs only through its bound template from the row's own
// `fix`, and its Yes always carries its preview's token. Check 12 keeps a purge out.
// The plugin-id literal (check 8) is scanned again, over the
// embedded assets, in tests/omarchy_widget.rs.

const { describe, expect, test } = require("bun:test")
const fs = require("fs")
const path = require("path")
const M = require("../Model.js")

const ROOT = path.join(__dirname, "..")
const REPO = path.join(ROOT, "..")
// The widget's id, from the one file that may write it.
const ID = JSON.parse(fs.readFileSync(path.join(ROOT, "manifest.json"), "utf8")).id

// ------------------------------------------------------------------ the files

// Every runtime .qml and .js under omarchy/, as E1 embeds them (tests/ and a generated
// Build.js aside).
function runtimeNames() {
  const out = []
  const walk = (dir) => {
    for (const entry of fs.readdirSync(path.join(ROOT, dir), { withFileTypes: true })) {
      const rel = dir ? dir + "/" + entry.name : entry.name
      if (entry.isDirectory()) {
        if (rel !== "tests") walk(rel)
      } else if (/\.(qml|js)$/.test(entry.name) && rel !== "Build.js") {
        out.push(rel)
      }
    }
  }
  walk("")
  return out.sort()
}

const REAL = Object.fromEntries(runtimeNames().map((name) => [name, fs.readFileSync(path.join(ROOT, name), "utf8")]))

// `files` with `insertion` put after the first `anchor` in `name`.
function plant(files, name, anchor, insertion) {
  const text = files[name]
  const at = text.indexOf(anchor)
  if (at < 0) throw new Error(name + " has no " + JSON.stringify(anchor) + " to plant after")
  return Object.assign({}, files, { [name]: text.slice(0, at + anchor.length) + insertion + text.slice(at + anchor.length) })
}

// `files` with `from`, which must occur exactly once in `name`, replaced by `to`.
function replaced(files, name, from, to) {
  const text = files[name]
  const at = text.indexOf(from)
  if (at < 0 || text.indexOf(from, at + 1) >= 0) throw new Error(name + " holds " + JSON.stringify(from) + (at < 0 ? " nowhere" : " more than once"))
  return Object.assign({}, files, { [name]: text.slice(0, at) + to + text.slice(at + from.length) })
}

// `files` with the first `removal` after the first `anchor` in `name` taken out.
function unplant(files, name, anchor, removal) {
  const text = files[name]
  const at = text.indexOf(anchor)
  const cut = at < 0 ? -1 : text.indexOf(removal, at)
  if (cut < 0) throw new Error(name + " has no " + JSON.stringify(removal) + " after " + JSON.stringify(anchor))
  return Object.assign({}, files, { [name]: text.slice(0, cut) + text.slice(cut + removal.length) })
}

// The lines of a file without its comments (the widget writes only whole-line `//`
// comments), numbered from 1.
function lines(text) {
  return text.split("\n").map((line, i) => ({ n: i + 1, text: /^\s*\/\//.test(line) ? "" : line }))
}

function codeOf(text) {
  return lines(text).map((l) => l.text).join("\n")
}

function lineAt(text, index) {
  return text.slice(0, index).split("\n").length
}

function violation(file, line, rule) {
  return file + ":" + line + ": " + rule
}

// The text from an opening bracket at `open` to its match (strings skipped).
function balanced(text, open) {
  const pairs = { "{": "}", "(": ")", "[": "]" }
  const close = pairs[text[open]]
  let depth = 0
  let quote = ""
  for (let i = open; i < text.length; i++) {
    const c = text[i]
    if (quote) {
      if (c === "\\") i++
      else if (c === quote) quote = ""
    } else if (c === '"' || c === "'" || c === "`") {
      quote = c
    } else if (c === text[open]) {
      depth++
    } else if (c === close) {
      depth--
      if (depth === 0) return text.slice(open, i + 1)
    }
  }
  return text.slice(open)
}

// Every `pattern` match in the code of every file, with the bracketed text after it.
function calls(files, pattern, names) {
  const out = []
  for (const name of names || Object.keys(files)) {
    const code = codeOf(files[name])
    const re = new RegExp(pattern.source, "g")
    let m
    while ((m = re.exec(code))) {
      const open = m.index + m[0].length - 1
      out.push({ file: name, line: lineAt(code, m.index), match: m, args: balanced(code, open), rest: code.slice(open) })
    }
  }
  return out
}

// A top-level `function name(` of Model.js, to the next top-level declaration.
function functionBody(text, name) {
  const code = codeOf(text)
  const at = code.search(new RegExp("^function " + name + "\\(", "m"))
  if (at < 0) return null
  const open = code.indexOf("{", at)
  return { line: lineAt(code, at), body: balanced(code, open) }
}

const qmlNames = (files) => Object.keys(files).filter((n) => n.endsWith(".qml"))

// ------------------------------------------------------------------ the checks

// 1. Rendering (M3-D21): a CLI string reaches the screen only through PlainLabel. No
// raw Text, Label, TextEdit or TextInput binds its text to `modelData`, `svc.`, `row.`
// or `entry.`, and every one is plain text; PlainLabel is plain text and lets no user
// change that.
function checkRendering(files) {
  const out = []
  for (const name of qmlNames(files)) {
    if (name === "PlainLabel.qml") continue
    for (const found of calls(files, /(^|[\s:])(Text|Label|TextEdit|TextInput)\s*\{/m, [name])) {
      const block = found.args
      const own = block.slice(1, -1).replace(/\{[\s\S]*?\}/g, "")
      if (!/\btextFormat:\s*Text\.PlainText\b/.test(own)) {
        out.push(violation(name, found.line, found.match[2] + " without textFormat: Text.PlainText (use PlainLabel)"))
      }
      for (const text of own.split("\n").filter((l) => /^\s*text:/.test(l))) {
        if (/\b(modelData|svc\.|row\.|entry\.)/.test(text)) {
          out.push(violation(name, found.line, found.match[2] + " shows CLI data (" + text.trim() + "); use PlainLabel"))
        }
        // The shared components take their strings as properties, so the names above
        // never appear there: a component's Text shows only a literal or a glyph, and
        // every other string goes through PlainLabel (1n2p.12).
        if (name.startsWith("ui/") && !/^\s*text:\s*("[^"]*"|'[^']*')\s*$/.test(text) && !/fromCodePoint|glyph/i.test(text)) {
          out.push(violation(name, found.line, found.match[2] + " in a shared component shows a string (" + text.trim() + "); use PlainLabel"))
        }
      }
    }
  }
  // PlainLabel wraps its Text: a root Text, or a property naming textFormat, would let
  // a user set it.
  const label = codeOf(files["PlainLabel.qml"] || "")
  if (!/\btextFormat:\s*Text\.PlainText\b/.test(label)) out.push(violation("PlainLabel.qml", 1, "PlainLabel is not plain text"))
  if (/^(Text|Label)\s*\{/m.test(label) || /\b(property|alias)\b[^\n]*\btextFormat\b/.test(label)) {
    out.push(violation("PlainLabel.qml", 1, "PlainLabel lets a user set textFormat"))
  }
  return out
}

// The four literal tools besides omaddons (M3-D2), and the argv builders in Model.js
// with the tool each returns: the clipboard (`wl-copy`, 1n2p.43) only through its builder,
// with the text as one argument, and read (`wl-paste`, 1n2p.37) only through its own.
const LITERAL_TOOLS = ["inotifywait", "omarchy-notification-send", "xdg-open", "omarchy-launch-browser"]
const ARGV_BUILDERS = { "toastArgv": "omarchy-notification-send", "errorToastArgv": "omarchy-notification-send",
  "updateToastArgv": "omarchy-notification-send", "digestToastArgv": "omarchy-notification-send",
  "resultToastArgv": "omarchy-notification-send", "autoUpdateToastArgv": "omarchy-notification-send",
  "autoUpdateStoppedToastArgv": "omarchy-notification-send", "addonsDirArgv": "xdg-open",
  "copyArgv": "wl-copy", "pasteArgv": "wl-paste", "configEditorArgv": "omarchy-launch-config-editor" }
// Two argv outside the three sources, allowed exactly as written: `show()`'s fallback
// when the shell's summon facade is missing, and the self-reinstall (a fixed literal
// under the stamped context, check 2).
const SUMMON_FALLBACK = '["omarchy-shell", "shell", "summon", Build.PLUGIN_ID, "{}"]'
const REINSTALL = '[root.binary, "--actor", "widget", "--reason", "reinstall from the bar widget", "omarchy", "install"]'

// 2. Process launching (M3-D2, M3-D3, M3-D7): no shell, no string-form launch, and
// every omaddons process under the stamped context.
function checkLaunching(files) {
  const out = []
  const forbidden = [/\bUtil\.execDetached\b/, /\bbar\.run\(/, /"bash"/, /"-c"/, /"sh"/, /execDetached\(\s*["'`]/]
  for (const name of Object.keys(files)) {
    for (const l of lines(files[name])) {
      for (const re of forbidden) if (re.test(l.text)) out.push(violation(name, l.n, "launches through a shell or a string (" + re.source + ")"))
    }
  }
  for (const c of calls(files, /\bQuickshell\.execDetached\(/)) {
    const arg = c.args.slice(1).trimStart()
    if (!arg.startsWith("[") && !/^Model\.\w+\(/.test(arg)) out.push(violation(c.file, c.line, "execDetached takes an argv array or a Model argv builder"))
  }
  // Process blocks: an omaddons command (the binary, or the lane's argv) needs the context.
  for (const c of calls(files, /(^|\s)Process\s*\{/m, qmlNames(files))) {
    const command = (c.args.match(/^\s*command:\s*(.*)$/m) || [])[1]
    if (command && /\b(binary|argv)\b/.test(command) && !/^\s*environment:\s*Build\.CONTEXT\s*$/m.test(c.args)) {
      out.push(violation(c.file, c.line, "an omaddons Process without environment: Build.CONTEXT"))
    }
  }
  // Imperative launches: `x.command = …` needs `x.environment = Build.CONTEXT` and
  // `x.startDetached()` in the same function.
  for (const name of qmlNames(files)) {
    const code = codeOf(files[name])
    const re = /(\w+)\.command\s*=/g
    let m
    while ((m = re.exec(code))) {
      const fn = code.slice(code.lastIndexOf("function ", m.index))
      const body = balanced(fn, fn.indexOf("{"))
      if (!body.includes(m[1] + ".environment = Build.CONTEXT") || !body.includes(m[1] + ".startDetached()")) {
        out.push(violation(name, lineAt(code, m.index), m[1] + " runs without the stamped context or startDetached()"))
      }
    }
  }
  // The lane: a child created with the run's argv and env, which come from argvOf
  // under Build.CONTEXT.
  for (const c of calls(files, /createObject\(/, qmlNames(files))) {
    if (!/"command"/.test(c.args)) continue
    if (!/"command":\s*argv,\s*"environment":\s*env\s*\}/.test(c.args)) out.push(violation(c.file, c.line, "a child process without the run's env"))
  }
  const service = codeOf(files["Service.qml"] || "")
  const argvOf = service.slice(service.indexOf("function argvOf("))
  if (!/"context":\s*Build\.CONTEXT/.test(balanced(argvOf, argvOf.indexOf("{")))) {
    out.push(violation("Service.qml", lineAt(service, service.indexOf("function argvOf(")), "argvOf builds without Build.CONTEXT"))
  }
  return out
}

// 3. Argv provenance (M3-D2): every argv comes from Model.buildArgv, Model.cliArgv (the
// classifier) or one of the four literal tools.
function checkProvenance(files) {
  const out = []
  for (const c of calls(files, /\bQuickshell\.execDetached\(/)) {
    const arg = c.args.slice(1, -1).trim()
    const builder = arg.match(/^Model\.(\w+)\(/)
    const literal = arg.match(/^\[\s*"([^"]+)"/)
    if (builder && ARGV_BUILDERS[builder[1]]) continue
    if (arg === SUMMON_FALLBACK) continue
    if (literal && LITERAL_TOOLS.includes(literal[1])) {
      if (literal[1] === "omarchy-launch-browser") {
        const url = arg.slice(literal[0].length).replace(/^\s*,\s*/, "").replace(/\]$/, "").trim()
        const line = codeOf(files[c.file]).split("\n")[c.line - 1]
        if (!line.includes("Model.isSafeUrl(" + url + ")")) out.push(violation(c.file, c.line, "a browser launch without Model.isSafeUrl(" + url + ")"))
      }
      continue
    }
    out.push(violation(c.file, c.line, "an argv from no allowed source: " + arg.slice(0, 60)))
  }
  // Each builder returns only its tool's argv: null, the array itself, or another
  // builder's of the same tool.
  const model = files["Model.js"] || ""
  for (const [name, tool] of Object.entries(ARGV_BUILDERS)) {
    const fn = functionBody(model, name)
    if (!fn) {
      out.push(violation("Model.js", 1, "argv builder " + name + " is missing"))
      continue
    }
    for (const r of fn.body.match(/\breturn\b[^\n]*/g) || []) {
      const via = r.match(/^return (\w+)\(/)
      const ok = r === "return null" || r.startsWith('return ["' + tool + '"') || (!!via && ARGV_BUILDERS[via[1]] === tool)
      if (!ok) out.push(violation("Model.js", fn.line, name + " returns something other than " + tool + "'s argv: " + r))
    }
  }
  // Processes: a literal tool, the exact reinstall, the lane's argv, or the clipboard's
  // argv from its builder alone (omad-phcg: a copy reads wl-copy's exit, and a paste
  // wl-paste's output, so each runs as a Process, never from a literal argv).
  for (const name of qmlNames(files)) {
    for (const l of lines(files[name])) {
      const prop = l.text.match(/^\s*command:\s*(.*)$/)
      const assigned = l.text.match(/\w+\.command\s*=\s*(.*)$/)
      if (prop) {
        const tool = prop[1].match(/^\[\s*"([^"]+)"/)
        const copy = /^Model\.copyArgv\([\w.]+\)\s*$/.test(prop[1]) || /^Model\.pasteArgv\(\)\s*$/.test(prop[1])
        if (!copy && (!tool || !LITERAL_TOOLS.includes(tool[1]))) out.push(violation(name, l.n, "a Process command from no allowed source: " + prop[1].slice(0, 60)))
      }
      if (assigned && assigned[1].trim() !== REINSTALL) out.push(violation(name, l.n, "a Process command from no allowed source: " + assigned[1].slice(0, 60)))
    }
  }
  const service = codeOf(files["Service.qml"] || "")
  // The lane runs argvOf's build: its argv, its env and its standard input (1n2p.37).
  for (const c of calls(files, /\.run\((?=r,)/, ["Service.qml"])) {
    if (!c.args.startsWith("(r, built.argv, built.env, built.stdin)")) out.push(violation("Service.qml", c.line, "a lane runs an argv not from argvOf"))
  }
  if (!/var built = null[\s\S]*?built = argvOf\(r\)/.test(service.slice(service.indexOf("function pump(")))) {
    out.push(violation("Service.qml", 1, "pump's argv is not argvOf's"))
  }
  const argvOf = service.slice(service.indexOf("function argvOf("))
  const argvBody = balanced(argvOf, argvOf.indexOf("{"))
  if (!/return r\.cli \? Model\.cliArgv\(r\.cli, opts\) : Model\.buildArgv\(r\.template, r\.slots, opts\)/.test(argvBody) ||
      (argvBody.match(/\breturn\b/g) || []).length !== 1) {
    out.push(violation("Service.qml", lineAt(service, service.indexOf("function argvOf(")), "argvOf returns something other than Model.cliArgv or Model.buildArgv"))
  }
  return out
}

// The functions of Model.js where the literal "apply" belongs: the classifier's rule
// (classifyCliArgv, cliArgv), the one place an apply argv is read from a preview
// (planFrom), and the two that read an envelope's `command` (applyResult,
// needsPreviewAgain).
const APPLY_FUNCTIONS = ["planFrom", "classifyCliArgv", "cliArgv", "applyResult", "needsPreviewAgain"]
// The values a request's or the dialog's "plan" may take in Service.qml, each with
// where it comes from.
const PLAN_VALUES = {
  "null": "no plan: a request's default, and M3's confirmation (targets rm), which stores none",
  "plan": "runPlan's parameter, called only with a decision's or the pending confirmation's plan",
  "decision.plan": "the dialog's pending plan, from Model.previewDecision",
  "r.plan || null": "argvOf's options, the request's own plan"
}

// The first and last line of `name`'s body in Model.js's text.
function bodyLines(text, name) {
  const fn = functionBody(text, name)
  return fn ? [fn.line, fn.line + fn.body.split("\n").length - 1] : null
}

// 3b. A plan's apply (M4-D28): an apply argv comes only from the preview the widget
// just ran. The literal "apply" lives only in Model.js's CLI_PATHS and APPLY_FUNCTIONS,
// never in QML and never in an array literal; every plan Service.qml runs is the one
// Model.previewDecision took from Model.planFrom.
function checkApplyProvenance(files) {
  const out = []
  const message = "an apply argv outside Model.planFrom"
  for (const name of Object.keys(files)) {
    const code = codeOf(files[name])
    const allowed = []
    if (name === "Model.js") {
      const raw = files[name]
      const begin = raw.indexOf("// BEGIN CLI_PATHS")
      const end = raw.indexOf("// END CLI_PATHS")
      if (begin >= 0 && end > begin) allowed.push([lineAt(raw, begin), lineAt(raw, end)])
      for (const fn of APPLY_FUNCTIONS) {
        const range = bodyLines(raw, fn)
        if (range) allowed.push(range)
      }
    }
    const literal = /["'`]apply["'`]/g
    let m
    while ((m = literal.exec(code))) {
      const line = lineAt(code, m.index)
      if (name.endsWith(".qml") || !allowed.some((r) => line >= r[0] && line <= r[1])) out.push(violation(name, line, message))
    }
    for (const l of lines(files[name])) {
      if (/\[[^\]\n]*["'`]apply["'`]/.test(l.text)) out.push(violation(name, l.n, message + " (an array literal)"))
    }
  }
  const service = codeOf(files["Service.qml"] || "")
  const notFromPlan = "a plan not from Model.planFrom"
  const enclosing = (index) => {
    const start = service.lastIndexOf("function ", index)
    const open = service.indexOf("{", start)
    return { head: service.slice(start, open), body: balanced(service, open) }
  }
  const values = /"plan":\s*([^,}\n]+)/g
  let v
  while ((v = values.exec(service))) {
    const value = v[1].trim()
    const line = lineAt(service, v.index)
    if (!PLAN_VALUES.hasOwnProperty(value)) {
      out.push(violation("Service.qml", line, notFromPlan + ": " + value))
      continue
    }
    const fn = enclosing(v.index)
    if (value === "plan" && !/\(plan\b/.test(fn.head) && !/var plan = Model\.planFrom\(/.test(fn.body)) {
      out.push(violation("Service.qml", line, notFromPlan + ": plan is neither a parameter nor Model.planFrom's"))
    }
    if (value === "decision.plan" && !/var decision = Model\.previewDecision\(/.test(fn.body)) {
      out.push(violation("Service.qml", line, notFromPlan + ": decision is not Model.previewDecision's"))
    }
  }
  // runPlan applies only a decision's plan or the pending confirmation's; or, for an
  // install on several clients (omad-3rre), one of the pending confirmation's plans,
  // each previewedEach's own `decision.plan`: the first in confirmYes, the next
  // (pendingInstalls' first, the confirmation's rest) in appliedEach.
  for (const c of calls({ "Service.qml": files["Service.qml"] || "" }, /(?<!function )\brunPlan\(/)) {
    const from = c.args.match(/^\((decision|pending|pending\.each\[0\]|next)\.plan,/)
    const fn = enclosing(lineIndex(service, c.line))
    if (!from) out.push(violation("Service.qml", c.line, notFromPlan + ": runPlan" + c.args.slice(0, 40)))
    else if (from[1] === "decision" && !/var decision = Model\.previewDecision\(/.test(fn.body)) {
      out.push(violation("Service.qml", c.line, notFromPlan + ": decision is not Model.previewDecision's"))
    } else if (from[1] === "pending.each[0]" && !/^function confirmYes\(/.test(fn.head)) {
      out.push(violation("Service.qml", c.line, notFromPlan + ": the confirmation's first plan outside confirmYes"))
    } else if (from[1] === "next" && (!/^function appliedEach\(/.test(fn.head) || !/var next = pendingInstalls\[0\]/.test(fn.body))) {
      out.push(violation("Service.qml", c.line, notFromPlan + ": next is not the pending installs' first"))
    }
  }
  // The pending installs are the confirmation's plans: set from it in confirmYes, then
  // only shortened (or emptied) as each applies.
  const installs = /\bpendingInstalls\s*=\s*([^\n]+)/g
  let p
  while ((p = installs.exec(service))) {
    const value = p[1].trim()
    if (value !== "pending.each.slice(1)" && value !== "pendingInstalls.slice(1)" && value !== "[]") {
      out.push(violation("Service.qml", lineAt(service, p.index), notFromPlan + ": pendingInstalls = " + value.slice(0, 40)))
    }
  }
  const argvOf = service.slice(service.indexOf("function argvOf("))
  if (!/"plan": r\.plan \|\| null/.test(balanced(argvOf, argvOf.indexOf("{")))) {
    out.push(violation("Service.qml", lineAt(service, service.indexOf("function argvOf(")), "argvOf passes no request's plan"))
  }
  return out
}

// The character index where `line` (from 1) starts in `text`.
function lineIndex(text, line) {
  let at = 0
  for (let n = 1; n < line && at >= 0; n++) at = text.indexOf("\n", at) + 1
  return at
}

// 4. Files and URLs (M3-D3): the widget writes no file and opens only safe URLs.
function checkFiles(files) {
  const out = []
  for (const name of Object.keys(files)) {
    for (const l of lines(files[name])) {
      if (/\bsetText\(|\bFileView\b/.test(l.text)) out.push(violation(name, l.n, "the widget writes no file (FileView, setText)"))
      if (/\bQt\.openUrlExternally\(/.test(l.text) && !/\bModel\.isSafeUrl\(/.test(l.text)) out.push(violation(name, l.n, "a URL opened without Model.isSafeUrl"))
    }
  }
  return out
}

// The COMMANDS table, from Model.js's text.
function tableOf(model) {
  const m = model.match(/^var COMMANDS = (\{[\s\S]*?\n\})\n\/\/ END COMMANDS/m)
  return m ? JSON.parse(m[1]) : null
}

// 5. Slots (M3-D2): `{query}` (what the user typed) fills read-only templates only.
function checkSlots(files) {
  const out = []
  const model = files["Model.js"] || ""
  const table = tableOf(model)
  if (!table) return [violation("Model.js", 1, "no COMMANDS table")]
  const start = model.search(/^var COMMANDS = \{/m)
  for (const [name, t] of Object.entries(table)) {
    const line = lineAt(model, model.indexOf('"' + name + '":', start))
    if (t.mutates === true && t.argv.includes("{query}")) out.push(violation("Model.js", line, name + " mutates and takes {query}"))
  }
  for (const name of qmlNames(files)) {
    for (const l of lines(files[name])) {
      if (/"query"\s*:/.test(l.text) && /\b(run|runBound|mutation)\(/.test(l.text)) out.push(violation(name, l.n, "a query slot passed to a change"))
      const literal = l.text.match(/"template":\s*"(\w+)"/)
      if (literal && /"query"\s*:/.test(l.text) && table[literal[1]] && table[literal[1]].mutates) {
        out.push(violation(name, l.n, "a query slot passed to " + literal[1]))
      }
    }
  }
  return out
}

// What needs its preview first: `preview` templates and destructive paths.
function needsPreview(name) {
  const t = M.COMMANDS[name]
  return !!t && (!!t.preview || M.CLI_PATHS[M.templateShape(name, M.COMMANDS).path].destructive === true)
}

// The plan-storing paths (M4-D22), from Model.js's text.
function planPathsOf(model) {
  const m = model.match(/^var PLAN_PATHS = (\[[^\n]*\])\n\/\/ END PLAN_PATHS/m)
  return m ? JSON.parse(m[1]) : null
}

// Templates on a plan-storing path that run directly: they change only the record's
// pin flag (no --version, which would install a release) and are undoable (M3-D9).
const RECORD_ONLY = ["pin", "unpin"]

// 6. Previews (M3-D9, M3-D46, M4-D28): a destructive action, and every template that
// changes addons on a plan-storing path, reaches a lane only through the preview flow.
// Views reach the service through run, runBound, runCliArgv and confirmYes, which route
// that work to previewOf; nothing names such a template in a direct mutation; a match
// preview alone runs under the network deadline.
function checkPreviews(files) {
  const out = []
  const model = files["Model.js"] || ""
  const table = tableOf(model)
  const planPaths = planPathsOf(model)
  if (!table || !planPaths) return [violation("Model.js", 1, "no COMMANDS table or PLAN_PATHS list")]
  const start = model.search(/^var COMMANDS = \{/m)
  for (const [name, t] of Object.entries(table)) {
    const line = lineAt(model, model.indexOf('"' + name + '":', start))
    const path = M.templateShape(name, table).path
    if (t.confirm !== undefined) out.push(violation("Model.js", line, name + " keeps the old confirm key"))
    const changesAddons = t.mutates === true && planPaths.includes(path)
    const previews = t.preview === "match" || t.preview === "confirm"
    if (changesAddons && (RECORD_ONLY.includes(name) ? t.argv.includes("--version") : !previews)) {
      out.push(violation("Model.js", line, name + " changes addons without a preview"))
    }
    if (M.CLI_PATHS[path] && M.CLI_PATHS[path].destructive && t.preview !== "confirm") {
      out.push(violation("Model.js", line, name + " is destructive without preview: confirm"))
    }
  }
  for (const name of Object.keys(files).filter((n) => n !== "Service.qml")) {
    for (const l of lines(files[name])) {
      if (/\b(mutation|previewOf|request|laneNamed|argvOf|pump)\(/.test(l.text)) out.push(violation(name, l.n, "reaches the service's lanes around run, runBound and runCliArgv"))
    }
  }
  for (const c of calls(files, /\b(mutation|request)\(\{/, ["Service.qml"])) {
    const literal = c.args.match(/"template":\s*"(\w+)"/)
    if (literal && needsPreview(literal[1])) out.push(violation("Service.qml", c.line, literal[1] + " runs without its preview"))
  }
  const service = codeOf(files["Service.qml"] || "")
  const routes = [
    ["function run(", /if \(template\.preview \|\| Model\.CLI_PATHS\[path\]\.destructive\) \{[^}]*previewOf\(spec, "", false\)/],
    // A CLI-offered install or update previews too, as README promises (1n2p.32).
    ["function runCliArgv(", /if \(verdict\.destructive \|\| verdict\.path === "install" \|\| verdict\.path === "update"\) \{[^}]*previewOf\(spec, "", false\)/],
    ["function runBound(", /if \(!template \|\| template\.bind !== true\) return/],
  ]
  for (const [head, route] of routes) {
    const at = service.indexOf(head)
    if (at < 0 || !route.test(balanced(service.slice(at), service.slice(at).indexOf("{")))) {
      out.push(violation("Service.qml", at < 0 ? 1 : lineAt(service, at), head.slice(9) + ") does not route destructive work to its preview"))
    }
  }
  const previewAt = service.indexOf("function previewOf(")
  const previewBody = previewAt < 0 ? "" : balanced(service.slice(previewAt), service.slice(previewAt).indexOf("{"))
  if (!/if \(spec\.mode === "match"\) \{[^}]*deadlineMs = networkDeadlineMs/.test(previewBody)
      || (previewBody.match(/networkDeadlineMs/g) || []).length !== 1) {
    out.push(violation("Service.qml", previewAt < 0 ? 1 : lineAt(service, previewAt), "previewOf gives the network deadline to more than a match preview"))
  }
  return out
}

// Model.js functions whose body mentions warnings and reads a `.message` that decides
// nothing, each with what it reads.
const MESSAGE_READERS = {
  "warningsOf": "a warning's message, to show it beside its fix",
  "warningMessages": "the messages of the warnings of one code, to show them",
  "issueSections": "each warning warningsOf reduced, as its row's title"
}
// The plan runner's decisions (M4-D28): typed fields only.
const DECISIONS = ["planFrom", "approvalOf", "requestedItems", "selectionMatches", "previewDecision", "reasonEffects", "changeLines"]
const COMPARISON = /===|!==|==|indexOf\(|\.test\(|\.match\(|includes\(|some\(|find\(|filter\(|startsWith\(/

// 9. Warnings decide nothing (M4-D28): a warning's text is shown, never tested; the
// plan runner's decisions read no warning at all.
function checkWarnings(files) {
  const out = []
  const model = files["Model.js"] || ""
  const code = codeOf(model)
  const heads = /^function (\w+)\(/gm
  let m
  while ((m = heads.exec(code))) {
    const fn = functionBody(model, m[1])
    if (fn && /\bwarnings\b/.test(fn.body) && /\.message\b/.test(fn.body) && !MESSAGE_READERS.hasOwnProperty(m[1])) {
      out.push(violation("Model.js", fn.line, m[1] + " reads a warning's message"))
    }
  }
  for (const name of DECISIONS) {
    const fn = functionBody(model, name)
    if (!fn) out.push(violation("Model.js", 1, name + " is missing"))
    else if (/\bwarnings\b|\.message\b/.test(fn.body)) out.push(violation("Model.js", fn.line, name + " reads warnings"))
  }
  for (const name of Object.keys(files)) {
    for (const l of lines(files[name])) {
      if (/warningMessages\(/.test(l.text) && COMPARISON.test(l.text)) out.push(violation(name, l.n, "a warning's text decides something"))
    }
  }
  for (const name of qmlNames(files)) {
    for (const l of lines(files[name])) {
      if (/\.message\b/.test(l.text) && /warning/i.test(l.text) && COMPARISON.test(l.text)) {
        out.push(violation(name, l.n, "a view decides from a warning's text"))
      }
    }
  }
  return out
}

// 10. No issue by its id (M4-D48): the widget lists `issues[]` as the CLI reports them,
// so a failure-mode id the CLI narrows or splits changes nothing the widget decides.
function checkIssueIds(files) {
  const out = []
  for (const name of Object.keys(files)) {
    for (const l of lines(files[name])) {
      if (/["'`]fm-[a-z]/.test(l.text)) out.push(violation(name, l.n, "a literal failure-mode id (the widget lists issues generically, M4-D48)"))
      if (/\b(issue|row|mi|i)\.id\s*[!=]==?\s*["'`]/.test(l.text)) out.push(violation(name, l.n, "an issue matched by its id"))
    }
  }
  return out
}

// The flags that give consent: only a person in a terminal gives them (M4-D38).
const CONSENT = ["--force", "--yes"]

// The words a template's argv starts with, up to its first flag or slot (at most two).
function templateWords(argv) {
  const words = []
  for (const part of argv) {
    if (words.length === 2 || typeof part !== "string" || part.startsWith("-") || part.includes("{")) break
    words.push(part)
  }
  return words.join(" ")
}

// The text of the first argument of a call whose bracketed arguments are `args`.
function firstArgument(args) {
  let depth = 0
  let quote = ""
  for (let i = 1; i < args.length; i++) {
    const c = args[i]
    if (quote) {
      if (c === "\\") i++
      else if (c === quote) quote = ""
    } else if (c === '"' || c === "'" || c === "`") {
      quote = c
    } else if ("([{".includes(c)) {
      depth++
    } else if (")]}".includes(c)) {
      if (depth === 0) return args.slice(1, i).trim()
      depth--
    } else if (c === "," && depth === 0) {
      return args.slice(1, i).trim()
    }
  }
  return args.slice(1).trim()
}

// 11. Consent stays a person's (M4-D38, M4-D36, M4-D22, M4-D44): no template carries
// `--force` or `--yes`, runs `doctor gc` or `doctor undo`, or runs `doctor fix` but as a
// bound `--only={fm}` preview; the consent literals live only in CONSENT_FLAGS, which
// the classifier refuses with `doctor gc` and the doctor's own flows; no QML file names
// a doctor command or template, so the Issues view reaches a repair only through
// `row.fix.template` (Model.repairRequest), never through the classifier's path, and a
// problem's page runs nothing but through the view; and a bound repair's Yes always
// carries its preview's token.
function checkConsent(files) {
  const out = []
  const model = files["Model.js"] || ""
  const table = tableOf(model)
  if (!table) return [violation("Model.js", 1, "no COMMANDS table")]
  const start = model.search(/^var COMMANDS = \{/m)
  for (const [name, t] of Object.entries(table)) {
    const line = lineAt(model, model.indexOf('"' + name + '":', start))
    const argv = Array.isArray(t.argv) ? t.argv : []
    const words = templateWords(argv)
    for (const part of argv) {
      const flag = String(part).split("=")[0]
      if (CONSENT.includes(flag)) out.push(violation("Model.js", line, name + ": a template carries " + flag))
      if (words === "doctor fix" && flag.startsWith("-") && String(part).includes("{") && part !== "--only={fm}") {
        out.push(violation("Model.js", line, name + ": a doctor fix template with an option slot other than --only={fm} (" + part + ")"))
      }
      if (String(part).includes("{fm}") && (words !== "doctor fix" || part !== "--only={fm}")) {
        out.push(violation("Model.js", line, name + ": {fm} outside doctor fix's --only={fm}"))
      }
    }
    if (words === "doctor gc" || words === "doctor undo") out.push(violation("Model.js", line, name + ": a template runs " + words + ", never the widget's"))
    if (words === "doctor fix" && t.preview !== "bind") out.push(violation("Model.js", line, name + ": a doctor fix template without preview: bind"))
  }
  // The consent literals, in any quote and their `=` forms, only on CONSENT_FLAGS's line.
  const literal = /(["'`])--(force|yes)(=[^"'`\n]*)?\1/
  for (const name of Object.keys(files)) {
    for (const l of lines(files[name])) {
      if (literal.test(l.text) && !(name === "Model.js" && /^var CONSENT_FLAGS = /.test(l.text.trim()))) {
        out.push(violation(name, l.n, "a consent flag literal outside CONSENT_FLAGS"))
      }
    }
  }
  const classifier = functionBody(model, "classifyCliArgv")
  if (!classifier || !/\bCONSENT_FLAGS\b/.test(classifier.body)) {
    out.push(violation("Model.js", classifier ? classifier.line : 1, "the classifier lost its consent rule (CONSENT_FLAGS)"))
  }
  for (const reason of ["consent_flag", "doctor_gc", "own_flow_only"]) {
    if (!classifier || !classifier.body.includes('"' + reason + '"')) {
      out.push(violation("Model.js", classifier ? classifier.line : 1, "the classifier lost its consent rule (" + reason + ")"))
    }
  }
  // cliArgv's first read of the argv is the classifier, whose refusal throws.
  const cli = functionBody(model, "cliArgv")
  const call = cli ? cli.body.indexOf("classifyCliArgv(argv") : -1
  if (!cli || call < 0 || cli.body.search(/\bargv\b/) !== call + "classifyCliArgv(".length || !/if \(!verdict\.ok\) throw refuse\(/.test(cli.body)) {
    out.push(violation("Model.js", cli ? cli.line : 1, "cliArgv builds before the classifier refuses"))
  }
  for (const name of qmlNames(files)) {
    for (const l of lines(files[name])) {
      if (/["'`]doctor/.test(l.text)) out.push(violation(name, l.n, "a QML file names a doctor command or template"))
    }
  }
  const issues = "views/IssuesView.qml"
  if (files[issues]) {
    for (const c of calls({ [issues]: files[issues] }, /\bsvc\.run\(/)) {
      const first = firstArgument(c.args)
      const named = first.match(/^"(\w+)"$/)
      const plain = !!named && !!table[named[1]] && !templateWords(table[named[1]].argv).startsWith("doctor")
      if (first !== "row.fix.template" && !plain) out.push(violation(issues, c.line, "the Issues view runs a repair other than row.fix.template (" + first + ")"))
    }
    for (const c of calls({ [issues]: files[issues] }, /\b(frame\.runCli|svc\.runCliArgv)\(/)) {
      if (/\.fix\b/.test(firstArgument(c.args))) out.push(violation(issues, c.line, "the Issues view runs a repair through the classifier's path"))
    }
  }
  // A problem's page runs nothing itself: its buttons are the Issues view's (1n2p.28), so
  // the rules above hold on the page too.
  const page = "views/ProblemPage.qml"
  if (files[page]) {
    for (const c of calls({ [page]: files[page] }, /\b(svc\.run|svc\.runBound|svc\.runCliArgv|frame\.runCli)\(/)) {
      out.push(violation(page, c.line, "a problem's page runs a fix itself (" + c.match[1] + "); its buttons go through the Issues view"))
    }
  }
  // The bind preview's Yes carries its token: the branch keeps the decision's token and
  // binds, confirmYes passes it, and buildArgv requires it of a `bind` template.
  const service = codeOf(files["Service.qml"] || "")
  const tokenless = "a bind preview's Yes runs without its token"
  const at = service.indexOf('r.spec.mode === "bind"')
  if (at < 0) {
    out.push(violation("Service.qml", 1, tokenless + " (no bind preview branch)"))
  } else {
    const block = balanced(service, service.indexOf("{", at))
    const decided = block.match(/var (\w+) = Model\.doctorPreviewDecision\(/)
    const pending = block.match(/pendingConfirm = (\{[^\n]*\})/)
    const keeps = !!decided && !!pending && /"binds":\s*true\b/.test(pending[1])
      && new RegExp('"token":\\s*' + decided[1] + "\\.token\\b").test(pending[1])
    if (!keeps) out.push(violation("Service.qml", lineAt(service, at), tokenless))
  }
  const yesAt = service.indexOf("function confirmYes(")
  const yes = yesAt < 0 ? "" : balanced(service, service.indexOf("{", yesAt))
  if (!/"token":\s*pending\.binds\s*\?\s*pending\.token\s*:\s*""/.test(yes)) {
    out.push(violation("Service.qml", yesAt < 0 ? 1 : lineAt(service, yesAt), tokenless + " (confirmYes)"))
  }
  const build = functionBody(model, "buildArgv")
  const ifMatch = build ? calls({ "Model.js": build.body }, /\bifMatchFor\(/)[0] : null
  if (!ifMatch || !/template\.preview === "bind"/.test(ifMatch.args)) {
    out.push(violation("Model.js", build ? build.line : 1, tokenless + " (buildArgv binds no bind template)"))
  }
  return out
}

// 12. Irreversible stays out (1n2p.43): no template holds a purge, the classifier refuses
// one, and no other file names the flag, so no Enter and no Yes ever deletes what no undo
// brings back.
function checkIrreversible(files) {
  const out = []
  const model = files["Model.js"] || ""
  const start = model.indexOf("var COMMANDS = {")
  if (start === -1) {
    out.push(violation("Model.js", 1, "the COMMANDS table is missing"))
  } else {
    const table = balanced(model.slice(start), model.slice(start).indexOf("{"))
    if (/--purge/.test(table)) out.push(violation("Model.js", lineAt(model, start), "a COMMANDS template holds --purge"))
  }
  const classifier = functionBody(model, "classifyCliArgv")
  if (!classifier || !/IRREVERSIBLE_FLAGS/.test(classifier.body) || !/"irreversible"/.test(classifier.body)) {
    out.push(violation("Model.js", classifier ? classifier.line : 1, "classifyCliArgv does not refuse an irreversible flag"))
  }
  if (!/var IRREVERSIBLE_FLAGS = \[[^\]]*"--purge"/.test(model)) {
    out.push(violation("Model.js", 1, "IRREVERSIBLE_FLAGS does not hold --purge"))
  }
  for (const name of Object.keys(files)) {
    for (const l of lines(files[name])) {
      if (/--purge/.test(l.text) && !(name === "Model.js" && /^var IRREVERSIBLE_FLAGS = /.test(l.text.trim()))) {
        out.push(violation(name, l.n, "names --purge outside IRREVERSIBLE_FLAGS"))
      }
    }
  }
  return out
}

const ERROR_FIELDS = ["message", "stack", "locals", "raw", "location", "path", "text", "folder", "file", "details"]

// 7. Notifications (M3-D14, M3-D21): the error toast and the session digest (1n2p.33)
// see names, counts and clients.
function checkNotifications(files) {
  const out = []
  const model = files["Model.js"] || ""
  const builders = ["errorToastArgv", "digestToastArgv"]
  const fns = {}
  for (const name of [...builders, "errorFacts", "errorParts"]) {
    const fn = functionBody(model, name)
    fns[name] = fn
    if (!fn) {
      out.push(violation("Model.js", 1, name + " is missing"))
      continue
    }
    for (const field of ERROR_FIELDS) {
      if (new RegExp("\\b" + field + "\\b").test(fn.body)) out.push(violation("Model.js", fn.line, name + " reads " + field))
    }
  }
  for (const name of builders) {
    const fn = fns[name]
    if (!fn) continue
    const read = new Set([...fn.body.matchAll(/facts\.(\w+)/g)].map((m) => m[1]))
    for (const field of read) if (!["names", "counts", "targets"].includes(field)) out.push(violation("Model.js", fn.line, name + " reads facts." + field))
  }
  for (const c of calls(files, /\bModel\.(errorToastArgv|digestToastArgv|updateToastArgv|resultToastArgv|autoUpdateToastArgv|autoUpdateStoppedToastArgv)\(/, qmlNames(files))) {
    for (const field of ERROR_FIELDS) {
      if (new RegExp("\\b" + field + "\\b").test(c.args)) out.push(violation(c.file, c.line, c.match[1] + " is passed " + field))
    }
    if (c.match[1] === "errorToastArgv" && !/^\(Model\.errorFacts\(\w+\), Build\.PLUGIN_ID\)$/.test(c.args)) {
      out.push(violation(c.file, c.line, "errorToastArgv takes Model.errorFacts(…) only"))
    }
    if (c.match[1] === "digestToastArgv" && !/^\(Model\.errorFacts\(\w+\), /.test(c.args)) {
      out.push(violation(c.file, c.line, "digestToastArgv takes Model.errorFacts(…) for its errors"))
    }
  }
  return out
}

// 8. The plugin id (E1): every use derives from Build.PLUGIN_ID, so the debug checkpoint
// widget gets its own service, IPC handler and panel.
function checkPluginId(files) {
  const out = []
  const literal = new RegExp(ID.replace(/\./g, "\\.") + "(?![\\w-])")
  for (const name of Object.keys(files)) {
    for (const l of lines(files[name])) {
      if (literal.test(l.text)) out.push(violation(name, l.n, "a literal plugin id (use Build.PLUGIN_ID)"))
      for (const m of l.text.matchAll(/\b(serviceFor|summon)\(\s*([^,)]*)/g)) {
        if (m[2].trim() !== "Build.PLUGIN_ID") out.push(violation(name, l.n, m[1] + " takes Build.PLUGIN_ID"))
      }
      const prop = l.text.match(/^\s*(moduleName|ipcTarget):\s*(.*)$/)
      if (prop && prop[2].trim() !== "Build.PLUGIN_ID") out.push(violation(name, l.n, prop[1] + " is Build.PLUGIN_ID"))
    }
  }
  for (const c of calls(files, /IpcHandler\s*\{/, qmlNames(files))) {
    const target = (c.args.match(/^\s*target:\s*(.*)$/m) || [])[1]
    if (!target || target.trim() !== "Build.PLUGIN_ID") out.push(violation(c.file, c.line, "the IPC target is Build.PLUGIN_ID"))
  }
  for (const c of calls(files, /\bModel\.(errorToastArgv|digestToastArgv|updateToastArgv|resultToastArgv|autoUpdateToastArgv|autoUpdateStoppedToastArgv)\(/, qmlNames(files))) {
    if (!/, Build\.PLUGIN_ID\)$/.test(c.args)) out.push(violation(c.file, c.line, c.match[1] + " summons Build.PLUGIN_ID"))
  }
  return out
}

// 13. Standard input (1n2p.37): a pasted addon list is the only data the widget ever
// writes to a child, and only to the lane's child of a request built with it. buildArgv
// takes it from pastedList alone, for a `stdin: "list"` template, which is `sync -`'s
// confirmed preview; the lane's input is its run's own argument, written once the child
// starts, then closed. Nothing else opens or writes a child's standard input.
const STDIN_LINES = [
  /^\s*proc = processComponent\.createObject\(lane, \{ "stdinEnabled": input !== null, "command": argv, "environment": env \}\)$/,
  /^\s*child\.write\(input\)$/,
  /^\s*child\.stdinEnabled = false$/,
]

function checkStdin(files) {
  const out = []
  for (const name of Object.keys(files)) {
    for (const l of lines(files[name])) {
      if (!/\bstdinEnabled\b|\.write\(/.test(l.text)) continue
      if (name !== "Service.qml" || !STDIN_LINES.some((re) => re.test(l.text))) {
        out.push(violation(name, l.n, "a child's standard input outside the lane's request: " + l.text.trim().slice(0, 60)))
      }
    }
  }
  const service = codeOf(files["Service.qml"] || "")
  const lane = service.slice(service.indexOf("function run(r, argv, env, stdin)"))
  if (service.indexOf("function run(r, argv, env, stdin)") < 0 || !/^\s*input = typeof stdin === "string" \? stdin : null$/m.test(balanced(lane, lane.indexOf("{")))) {
    out.push(violation("Service.qml", 1, "the lane's input is not its run's stdin"))
  }
  if ((service.match(/\binput = /g) || []).length !== 2 || !/^\s*input = null$/m.test(service)) {
    out.push(violation("Service.qml", 1, "the lane's input is set other than from its run, or never cleared"))
  }
  const model = files["Model.js"] || ""
  const build = functionBody(model, "buildArgv")
  if (!build || !/if \(template\.stdin === "list"\) \{\s*var list = pastedList\(o\.stdin\)\s*if \(!list\.ok\) throw[^\n]*\s*stdin = list\.text/.test(build.body)) {
    out.push(violation("Model.js", build ? build.line : 1, "buildArgv's standard input is not pastedList's"))
  }
  const table = tableOf(model) || {}
  for (const [name, t] of Object.entries(table)) {
    if (t.stdin === undefined) continue
    if (t.stdin !== "list" || t.argv[0] !== "sync" || !t.argv.includes("-") || t.preview !== "confirm") {
      out.push(violation("Model.js", lineAt(model, model.indexOf('"' + name + '":')), name + " takes standard input other than as sync -'s confirmed preview"))
    }
  }
  return out
}

const CHECKS = [
  ["1. rendering", checkRendering],
  ["2. process launching", checkLaunching],
  ["3. argv provenance", checkProvenance],
  ["3b. apply from its plan", checkApplyProvenance],
  ["4. files and URLs", checkFiles],
  ["5. slots", checkSlots],
  ["6. previews", checkPreviews],
  ["7. notifications", checkNotifications],
  ["8. the plugin id", checkPluginId],
  ["9. warnings decide nothing", checkWarnings],
  ["10. no issue id by name", checkIssueIds],
  ["11. consent", checkConsent],
  ["12. irreversible stays out", checkIrreversible],
  ["13. standard input", checkStdin],
]

// ------------------------------------------------------------------ the tests

describe("the scans cover the finished widget", () => {
  test("every runtime file, exactly the embedded assets but the manifest", () => {
    const assets = fs.readFileSync(path.join(REPO, "src/omarchy/assets.rs"), "utf8")
    const embedded = [...assets.matchAll(/include_str!\("\.\.\/\.\.\/omarchy\/([^"]+)"\)/g)].map((m) => m[1]).filter((n) => n !== "manifest.json")
    expect(Object.keys(REAL)).toEqual(embedded.sort())
  })

  for (const [name, check] of CHECKS) {
    test(name + " passes on the real source", () => {
      expect(check(REAL)).toEqual([])
    })
  }
})

// Each check over a copy with one violation planted, which it must report.
const PLANTED = [
  ["1. rendering", checkRendering, "a rich Text bound to svc.errors in a view",
    plant(REAL, "views/IssuesView.qml", "Item {\n", '  Text { text: svc.errors ? svc.errors.targets[0].addons[0].errors[0].message : "" }\n'), /Text without textFormat|shows CLI data/],
  ["1. rendering", checkRendering, "a shared component showing its title through a plain Text",
    plant(REAL, "ui/AddonRow.qml", "  id: root\n", "  Text {\n    textFormat: Text.PlainText\n    text: root.title\n  }\n"), /shared component shows a string/],
  ["1. rendering", checkRendering, "PlainLabel exposing textFormat",
    plant(REAL, "PlainLabel.qml", "  property alias text: label.text\n", "  property alias textFormat: label.textFormat\n"), /lets a user set textFormat/],
  ["2. process launching", checkLaunching, "an omaddons Process without environment",
    plant(REAL, "Service.qml", "  Process {\n    id: reinstallProc\n  }\n", '  Process {\n    id: planted\n    command: [root.binary, "status"]\n  }\n'), /without environment: Build\.CONTEXT/],
  ["2. process launching", checkLaunching, "a string-form notification",
    plant(REAL, "Service.qml", "  function afterStatus() {\n", '    Quickshell.execDetached("omarchy-notification-send omaddons hi")\n'), /a string|argv array/],
  ["2. process launching", checkLaunching, "a shell",
    plant(REAL, "PanelView.qml", "  function openAddOns() {\n", '    Quickshell.execDetached(["bash", "-c", "xdg-open ~"])\n'), /through a shell/],
  ["3. argv provenance", checkProvenance, "a hand-built omaddons argv",
    plant(REAL, "Service.qml", "  Process {\n    id: reinstallProc\n  }\n", '  Process {\n    id: planted\n    command: [root.binary, "update"]\n    environment: Build.CONTEXT\n  }\n'), /no allowed source/],
  // omad-phcg: a copy runs as a Process with its builder's argv, never a literal one.
  ["3. argv provenance", checkProvenance, "a copy through a Process with a literal argv",
    plant(REAL, "Service.qml", "  Process {\n    id: reinstallProc\n  }\n", '  Process {\n    id: planted\n    command: ["wl-copy", "--", root.binary]\n  }\n'), /a Process command from no allowed source: \["wl-copy"/],
  ["3. argv provenance", checkProvenance, "a copy Process whose builder is not the clipboard's",
    plant(REAL, "Service.qml", "  Process {\n    id: reinstallProc\n  }\n", '  Process {\n    id: planted\n    command: Model.configEditorArgv(root.binary)\n  }\n'), /a Process command from no allowed source: Model\.configEditorArgv/],
  // omad-r0to: the config editor runs only through its builder (the path `config` said).
  ["3. argv provenance", checkProvenance, "a bare config editor launch",
    plant(REAL, "PanelView.qml", "  function openAddOns() {\n", '    Quickshell.execDetached(["omarchy-launch-config-editor", root.filter])\n'), /an argv from no allowed source/],
  ["3. argv provenance", checkProvenance, "a browser launch without isSafeUrl",
    plant(REAL, "PanelView.qml", "  function openAddOns() {\n", '    Quickshell.execDetached(["omarchy-launch-browser", svc.lastError.message])\n'), /without Model\.isSafeUrl/],
  ["4. files and URLs", checkFiles, "a FileView writing",
    plant(REAL, "Service.qml", "  Process {\n    id: reinstallProc\n  }\n", '  FileView {\n    path: "/tmp/x"\n    Component.onCompleted: setText("x")\n  }\n'), /writes no file/],
  ["5. slots", checkSlots, "{query} in update_addon's argv",
    plant(REAL, "Model.js", '"update_addon":   { "argv": ["update", "-t", "{target}", "--", "{addon}"', ', "{query}"'), /update_addon mutates and takes \{query\}/],
  ["5. slots", checkSlots, "a query slot passed to update_addon",
    plant(REAL, "views/SearchView.qml", "Item {\n", '  function planted() { svc.run("update_addon", { "target": "retail", "addon": "x", "query": root.query }, "Update") }\n'), /a query slot passed to a change/],
  ["6. previews", checkPreviews, "a removal straight to the lane",
    plant(REAL, "Service.qml", "  function afterStatus() {\n", '    mutation({ "resource": "remove", "template": "remove", "slots": {}, "label": "Remove" })\n'), /remove runs without its preview/],
  ["6. previews", checkPreviews, "a view around run",
    plant(REAL, "views/InstalledView.qml", "Item {\n", '  function planted() { svc.mutation({ "template": "remove" }) }\n'), /around run/],
  ["7. notifications", checkNotifications, "an error's message passed to the toast",
    plant(REAL, "Service.qml", "  function afterStatus() {\n",
      '    Quickshell.execDetached(Model.errorToastArgv({ "names": [entry.message], "counts": [1], "targets": [] }, Build.PLUGIN_ID))\n'), /is passed message|Model\.errorFacts/],
  ["7. notifications", checkNotifications, "the builder reading a message",
    plant(REAL, "Model.js", "function errorToastArgv(facts, pluginId) {\n", "  var leak = facts.message\n"), /errorToastArgv reads message/],
  ["7. notifications", checkNotifications, "the digest reading a stack",
    plant(REAL, "Model.js", "function digestToastArgv(facts, updates, kinds, pluginId) {\n", "  var leak = facts.stack\n"), /digestToastArgv reads stack/],
  ["7. notifications", checkNotifications, "the digest given hand-made facts",
    plant(REAL, "Service.qml", "  function afterStatus() {\n",
      '    Quickshell.execDetached(Model.digestToastArgv({ "names": [entry.addon], "counts": [1] }, null, null, Build.PLUGIN_ID))\n'), /digestToastArgv takes Model\.errorFacts/],
  ["8. the plugin id", checkPluginId, "a literal id in serviceFor",
    plant(REAL, "BarWidget.qml", "  ipcTarget: Build.PLUGIN_ID\n", '  readonly property var planted: shell.serviceFor("' + ID + '")\n'), /literal plugin id|serviceFor takes Build\.PLUGIN_ID/],
  ["3b. apply from its plan", checkApplyProvenance, "a hand-built apply argv",
    plant(REAL, "Service.qml", "  function afterStatus() {\n", '    mutation({ "resource": "x", "cli": ["omaddons", "apply", "01M8Q2ZZZZZZZZZZZZZZZZZZZZ"], "label": "x" })\n'),
    /apply argv outside Model\.planFrom/],
  ["3b. apply from its plan", checkApplyProvenance, "a plan from a literal",
    plant(REAL, "Service.qml", "  function afterStatus() {\n", '    mutation({ "resource": "x", "plan": { "argv": ["omaddons"] }, "label": "x" })\n'),
    /plan not from Model\.planFrom/],
  ["3b. apply from its plan", checkApplyProvenance, "a fix that offers apply",
    plant(REAL, "Model.js", "function fixButtons(env) {\n", '  var sneaky = ["omaddons", "apply"]\n'), /apply argv outside Model\.planFrom/],
  // omad-3rre: an install on several clients applies only the confirmation's own plans.
  ["3b. apply from its plan", checkApplyProvenance, "a next plan outside the install chain",
    plant(REAL, "Service.qml", "  function afterStatus() {\n", '    var next = { "plan": null }\n    runPlan(next.plan, {}, 0)\n'),
    /next is not the pending installs' first/],
  ["3b. apply from its plan", checkApplyProvenance, "pending installs from elsewhere",
    plant(REAL, "Service.qml", "  function afterStatus() {\n", '    pendingInstalls = lastResult.commands\n'),
    /pendingInstalls = lastResult/],
  ["6. previews", checkPreviews, "update_addon without its preview",
    unplant(REAL, "Model.js", '"update_addon":', ', "preview": "match"'), /update_addon changes addons without a preview/],
  ["6. previews", checkPreviews, "a pin with a version and no preview",
    plant(REAL, "Model.js", '"pin":            { "argv": ["pin", "-t", "{target}"', ', "--version", "1"'), /pin changes addons without a preview/],
  ["6. previews", checkPreviews, "the old confirm key",
    plant(REAL, "Model.js", '"remove":         { "argv": ["remove", "-t", "{target}", "--", "{addon}"],        "mutates": true', ', "confirm": true'),
    /keeps the old confirm key/],
  ["9. warnings decide nothing", checkWarnings, "the routine rule reading a warning",
    plant(REAL, "Model.js", "function previewDecision(spec, env) {\n", "  if (env.warnings && env.warnings[0] && env.warnings[0].message) return null\n"),
    /previewDecision reads warnings/],
  ["9. warnings decide nothing", checkWarnings, "a helper testing a warning's text",
    plant(REAL, "Model.js", "// -------------------------------------------------------------- the reducers\n",
      'function sneaky(env) { return env.warnings[0].message.indexOf("dependency") >= 0 }\n'), /sneaky reads a warning's message/],
  ["9. warnings decide nothing", checkWarnings, "a service deciding from warning text",
    plant(REAL, "Service.qml", "  function afterStatus() {\n", '    if (Model.warningMessages(env, "x")[0] === "y") refresh()\n'), /a warning's text decides something/],
  ["12. irreversible stays out", checkIrreversible, "a template with --purge",
    plant(REAL, "Model.js", '"remove":         { "argv": ["remove", "-t", "{target}", "--", "{addon}"]', ', "argv2": ["targets", "rm", "--purge"]'),
    /COMMANDS template holds --purge/],
  ["12. irreversible stays out", checkIrreversible, "a classifier that forgets the purge",
    unplant(REAL, "Model.js", "function classifyCliArgv(", '      verdict.reason = "irreversible"\n'), /does not refuse an irreversible flag/],
  ["12. irreversible stays out", checkIrreversible, "a view building a purge",
    plant(REAL, "views/IssuesView.qml", "Item {\n", '  function planted() { svc.mutation({ "cli": ["omaddons", "targets", "rm", "x", "--purge"] }) }\n'),
    /names --purge outside/],
  ["10. no issue id by name", checkIssueIds, "an id literal in the Issues view",
    plant(REAL, "views/IssuesView.qml", "Item {\n",
      '  readonly property bool planted: !!svc && svc.targets.some(function(t) { return t.issues.some(function(i) { return i.id === "fm-concurrency_primitives-journal-pending" }) })\n'),
    /literal failure-mode id|issue matched by its id/],
  ["11. consent", checkConsent, "a forced template",
    plant(REAL, "Model.js", "var COMMANDS = {\n",
      '  "doctor_fix_forced": { "argv": ["doctor", "fix", "--only={fm}", "--force", "--yes"], "mutates": true, "preview": "bind" },\n'),
    /a template carries --force/],
  ["11. consent", checkConsent, "a doctor fix template without bind",
    unplant(REAL, "Model.js", '"doctor_fix":', ', "preview": "bind"'), /doctor fix template without preview: bind/],
  ["11. consent", checkConsent, "a doctor gc template",
    plant(REAL, "Model.js", "var COMMANDS = {\n", '  "tidy": { "argv": ["doctor", "gc", "--before", "30d"], "mutates": true },\n'),
    /a template runs doctor gc/],
  ["11. consent", checkConsent, "a doctor undo template",
    plant(REAL, "Model.js", "var COMMANDS = {\n", '  "repair_undo": { "argv": ["doctor", "undo"], "mutates": true },\n'),
    /a template runs doctor undo/],
  ["11. consent", checkConsent, "a forced argv in a view",
    plant(REAL, "views/IssuesView.qml", "  function activate(row) {\n", '    frame.runCli({ "argv": row.fix.argv.concat(["--force"]) }, "Fix")\n'),
    /consent flag literal outside CONSENT_FLAGS/],
  ["11. consent", checkConsent, "a forced argv in a view, through the classifier's path",
    plant(REAL, "views/IssuesView.qml", "  function activate(row) {\n", '    frame.runCli({ "argv": row.fix.argv.concat(["--force"]) }, "Fix")\n'),
    /runs a repair through the classifier's path/],
  ["11. consent", checkConsent, "the classifier without its consent flags",
    replaced(REAL, "Model.js", "if (CONSENT_FLAGS.indexOf(flagName(argv[c])) !== -1) {", "if ([].indexOf(flagName(argv[c])) !== -1) {"),
    /lost its consent rule \(CONSENT_FLAGS\)/],
  ["11. consent", checkConsent, "a doctor template named in a view",
    plant(REAL, "views/IssuesView.qml", "  function activate(row) {\n", '    svc.run("doctor_fix", { "fm": row.id }, "Repair")\n'),
    /a QML file names a doctor command or template/],
  ["11. consent", checkConsent, "a doctor template named in a view, around row.fix",
    plant(REAL, "views/IssuesView.qml", "  function activate(row) {\n", '    svc.run("doctor_fix", { "fm": row.id }, "Repair")\n'),
    /runs a repair other than row\.fix\.template/],
  ["11. consent", checkConsent, "a problem's page running its repair itself",
    plant(REAL, "views/ProblemPage.qml", "  function handleKey(name, cursor) {\n", '    svc.run(row.fix.template, row.fix.slots, row.fix.label)\n'),
    /a problem's page runs a fix itself/],
  ["11. consent", checkConsent, "a bind preview that does not bind",
    replaced(REAL, "Service.qml", '"token": repair.token, "binds": true', '"token": repair.token, "binds": false'),
    /a bind preview's Yes runs without its token$/],
  ["11. consent", checkConsent, "a confirmation that drops the token",
    replaced(REAL, "Service.qml", '"token": pending.binds ? pending.token : ""', '"token": ""'),
    /without its token \(confirmYes\)/],
  ["11. consent", checkConsent, "buildArgv that no longer binds a bind template",
    replaced(REAL, "Model.js", 'template.bind === true || template.preview === "bind"', "template.bind === true"),
    /buildArgv binds no bind template/],
  ["11. consent", checkConsent, "cliArgv that builds before it classifies",
    plant(REAL, "Model.js", "function cliArgv(argv, opts) {\n", "  var early = argv.slice(1)\n"),
    /cliArgv builds before the classifier refuses/],
  ["11. consent", checkConsent, "a doctor fix template with another slot",
    plant(REAL, "Model.js", "var COMMANDS = {\n",
      '  "doctor_fix_more": { "argv": ["doctor", "fix", "--only={fm}", "--skip={fm}"], "mutates": true, "preview": "bind" },\n'),
    /option slot other than --only=\{fm\}/],
  // 1n2p.37: the clipboard is read through its builder alone, and a child's standard
  // input is the lane's request's, only for sync -'s confirmed preview.
  ["3. argv provenance", checkProvenance, "a paste through a Process with a literal argv",
    plant(REAL, "Service.qml", "  Process {\n    id: reinstallProc\n  }\n", '  Process {\n    id: planted\n    command: ["wl-paste", "--primary"]\n  }\n'),
    /a Process command from no allowed source: \["wl-paste"/],
  ["3. argv provenance", checkProvenance, "a lane run without argvOf's standard input",
    replaced(REAL, "Service.qml", "laneNamed(name).run(r, built.argv, built.env, built.stdin)", "laneNamed(name).run(r, built.argv, built.env, r.label)"),
    /a lane runs an argv not from argvOf/],
  ["13. standard input", checkStdin, "a Process given standard input outside the lane",
    plant(REAL, "PanelView.qml", "  function openAddOns() {\n", '    planted.stdinEnabled = true\n    planted.write(root.filter)\n'),
    /PanelView\.qml:\d+: a child's standard input outside the lane's request/],
  ["13. standard input", checkStdin, "the lane writing something other than its input",
    replaced(REAL, "Service.qml", "      child.write(input)\n", "      child.write(root.binary)\n"),
    /Service\.qml:\d+: a child's standard input outside the lane's request: child\.write\(root\.binary\)/],
  ["13. standard input", checkStdin, "the lane's input taken from somewhere other than its run",
    replaced(REAL, "Service.qml", '      input = typeof stdin === "string" ? stdin : null\n', "      input = r.label\n"),
    /the lane's input is not its run's stdin/],
  ["13. standard input", checkStdin, "buildArgv passing standard input it did not check",
    replaced(REAL, "Model.js", "    stdin = list.text\n", "    stdin = o.stdin\n"),
    /buildArgv's standard input is not pastedList's/],
  ["13. standard input", checkStdin, "a template other than sync's preview taking standard input",
    plant(REAL, "Model.js", "var COMMANDS = {\n", '  "install_paste": { "argv": ["install", "--", "{ref}"], "mutates": true, "preview": "confirm", "stdin": "list" },\n'),
    /install_paste takes standard input other than as sync -'s confirmed preview/],
]

describe("each scan catches its planted violation", () => {
  for (const [name, check, what, files, rule] of PLANTED) {
    test(name + ": " + what, () => {
      const found = check(files)
      expect(found.length).toBeGreaterThan(0)
      expect(found.some((v) => rule.test(v))).toBe(true)
    })
  }
})

// The mechanisms the scans stand guard over, at run time.
describe("the mechanisms behind the scans", () => {
  const CONTEXT = { HOME: "/h", XDG_CONFIG_HOME: "/h/c", XDG_STATE_HOME: "/h/s", XDG_CACHE_HOME: "/h/k", XDG_DATA_HOME: "/h/d" }
  const opts = { binary: "/opt/omaddons", label: "Test", context: CONTEXT }
  // Every slot `x`, except those with a grammar of their own: a failure-mode id
  // (`fm-x`), a folder (`/x`), a run's id (a whole ULID) and a channel (`beta`).
  const slotsOf = (name) => Object.fromEntries((M.COMMANDS[name].argv.join(" ").match(/\{(\w+)\}/g) || [])
    .map((s) => [s.slice(1, -1), s === "{fm}" ? "fm-x" : s === "{path}" ? "/x" : s === "{run}" ? "01M8Q2VA7K3N5P8R1T4W6X9Y0Z"
      : s === "{channel}" ? "beta" : "x"]))

  test("every previewed template and destructive path refuses to run without its preview", () => {
    const bound = Object.keys(M.COMMANDS).filter((name) => M.COMMANDS[name].preview === "bind")
    const needing = Object.keys(M.COMMANDS).filter((name) => needsPreview(name) && bound.indexOf(name) === -1)
    expect(needing.sort()).toEqual(["adopt_target", "install", "install_version", "prune_backup", "reinstall", "remove", "restore",
      "restore_addon", "sync_all", "sync_target", "undo_run", "undo_run_target", "update_addon", "update_all", "update_channel", "update_target"])
    // A repair's Yes is its own argv bound to its preview's token (G2): never without it.
    expect(bound.sort()).toEqual(["doctor_fix", "doctor_fix_target"])
    for (const name of bound) {
      expect(() => M.buildArgv(name, slotsOf(name), opts)).toThrow(/token_required/)
      expect(M.buildArgv(name, slotsOf(name), Object.assign({ preview: true }, opts)).argv).toContain("--dry-run")
      expect(M.buildArgv(name, slotsOf(name), Object.assign({ token: "pc1:" + "0".repeat(64) }, opts)).argv).toContain("--if-match")
    }
    for (const name of needing) {
      // Its Yes is its preview's plan, applied (M4-D28): the real form never builds. A
      // pasted list's preview builds with the list (1n2p.37).
      const input = M.COMMANDS[name].stdin ? { stdin: LIST } : {}
      expect(() => M.buildArgv(name, slotsOf(name), Object.assign({}, input, opts))).toThrow(/preview_required/)
      expect(M.buildArgv(name, slotsOf(name), Object.assign({ preview: true }, input, opts)).argv).toContain("--dry-run")
    }
    for (const [cliPath, row] of Object.entries(M.CLI_PATHS)) {
      if (!row.destructive) continue
      // `doctor undo` and `doctor gc` are refused before that, never the widget's (G2).
      const refusal = cliPath === "doctor gc" ? /doctor_gc/ : cliPath === "doctor undo" ? /own_flow_only/ : /destructive_without_preview/
      expect(() => M.cliArgv(["omaddons"].concat(cliPath.split(" ")).concat(["-t", "retail"]), opts)).toThrow(refusal)
    }
  })

  // A dry run that stored plan `id`, offering exactly its apply.
  const PLAN_ID = "01M8Q2ZZZZZZZZZZZZZZZZZZZZ"
  const previewWith = (id) => ({ ok: true, schema_version: "1.0", tool_version: "0.1.0", command: "update", exit_code: 0, partial: false,
    data: { plan_id: id, plan_expires_at: "2100-01-02T00:00:00Z", approval: { routine: true, reasons: [], selection: [], scope: {} } },
    warnings: [], commands: [{ action: "apply", command: "omaddons apply " + id, argv: ["omaddons", "apply", id], destructive: false }] })

  test("apply runs only from its plan", () => {
    const argv = ["omaddons", "apply", PLAN_ID]
    expect(M.classifyCliArgv(argv).reason).toBe("apply_outside_plan")
    expect(() => M.cliArgv(argv, opts)).toThrow(/apply_outside_plan/)
    const plan = M.planFrom(previewWith(PLAN_ID))
    expect(M.cliArgv(argv, Object.assign({ plan: plan }, opts)).argv.slice(-2)).toEqual(["apply", PLAN_ID])
    const other = M.planFrom(previewWith("01M8Q2YYYYYYYYYYYYYYYYYYYY"))
    expect(() => M.cliArgv(argv, Object.assign({ plan: other }, opts))).toThrow(/apply_outside_plan/)
  })

  test("a remediation, a fix, an alternative and a commands[] entry offering apply never become buttons", () => {
    const argv = ["omaddons", "apply", PLAN_ID]
    const offered = "omaddons apply " + PLAN_ID
    const status = { ok: true, schema_version: "1.0", tool_version: "0.1.0", command: "status", exit_code: 0, partial: false,
      data: { targets: [{ id: "retail", issues: [{ id: "x", severity: "P2", title: "t", remediation: { command: offered, argv: argv, destructive: false } }] }] },
      warnings: [{ code: "w", message: "m", fix: offered, fix_argv: argv }],
      commands: [{ action: "apply", command: offered, argv: argv, destructive: false }] }
    const reduced = M.reduceStatus(status)
    expect(reduced.targets[0].issues[0].remediation.runnable).toBe(false)
    expect(reduced.commands.map((c) => c.runnable)).toEqual([false])
    expect(reduced.warnings[0].fix).toBeNull()
    const failure = { ok: false, schema_version: "1.0", tool_version: "0.1.0", command: "update", exit_code: 1, warnings: [], commands: [],
      error: { code: "x", message: "y", fix: offered, fix_argv: argv, alternatives: [{ display: offered, argv: argv }], retryable: false, details: {} } }
    expect(M.fixButtons(failure)).toEqual([])
    expect(M.resultUndo({ commands: [{ action: "undo", argv: argv, runnable: false }] })).toBeNull()
  })

  test("every built argv runs under the stamped context", () => {
    for (const name of Object.keys(M.COMMANDS)) {
      // A `bind` template runs with its view's token, a pasted list's with the list.
      const bound = M.COMMANDS[name].bind ? { token: "pc1:" + "0".repeat(64) } : {}
      const input = M.COMMANDS[name].stdin ? { stdin: LIST } : {}
      const built = M.buildArgv(name, slotsOf(name), Object.assign({ preview: needsPreview(name) }, bound, input, opts))
      expect(built.env).toEqual(CONTEXT)
    }
  })

  // 1n2p.37: standard input is a checked addon list, for sync -'s preview alone.
  const LIST = 'schema_version = "1.0"\n\n[family.mainline]\naddons = ["DBM"]\n'
  test("standard input reaches only sync -'s preview, checked again when it is built", () => {
    const takers = Object.keys(M.COMMANDS).filter((name) => M.COMMANDS[name].stdin !== undefined)
    expect(takers.sort()).toEqual(["sync_all", "sync_target"])
    for (const name of takers) {
      const preview = Object.assign({ preview: true }, opts)
      expect(M.buildArgv(name, slotsOf(name), Object.assign({ stdin: LIST }, preview)).stdin).toBe(LIST)
      // Its real run is the plan's apply, which holds the text: never built here.
      expect(() => M.buildArgv(name, slotsOf(name), Object.assign({ stdin: LIST }, opts))).toThrow(/preview_required/)
      for (const bad of [undefined, "", "default_channel = \"beta\"\n", "[limits]\nx = 1\n", "x".repeat(M.PASTE_MAX_BYTES + 1)]) {
        expect(() => M.buildArgv(name, slotsOf(name), Object.assign({ stdin: bad }, preview))).toThrow(/bad_input/)
      }
    }
    for (const name of Object.keys(M.COMMANDS).filter((n) => takers.indexOf(n) === -1)) {
      const form = Object.assign({ preview: needsPreview(name) }, M.COMMANDS[name].bind ? { token: "pc1:" + "0".repeat(64) } : {}, opts)
      expect(() => M.buildArgv(name, slotsOf(name), Object.assign({ stdin: LIST }, form))).toThrow(/stdin_not_taken/)
      expect(M.buildArgv(name, slotsOf(name), form).stdin).toBeUndefined()
    }
    // A plan's apply never carries it.
    const plan = { "argv": ["omaddons", "apply", "01M8Q2VA7K3N5P8R1T4W6X9Y0Z"] }
    expect(M.cliArgv(plan.argv, Object.assign({ plan: plan }, opts)).stdin).toBeUndefined()
  })

  test("a doctor template never runs forced, and only bound or as its preview", () => {
    const token = "pc1:" + "0".repeat(64)
    const doctor = Object.keys(M.COMMANDS).filter((name) => M.COMMANDS[name].argv[0] === "doctor")
    expect(doctor.sort()).toEqual(["doctor_fix", "doctor_fix_target"])
    for (const name of doctor) {
      const preview = M.buildArgv(name, slotsOf(name), Object.assign({ preview: true }, opts)).argv
      expect([name, preview.includes("--dry-run"), preview.includes("--if-match")]).toEqual([name, true, false])
      expect(() => M.buildArgv(name, slotsOf(name), opts)).toThrow(/token_required/)
      expect(() => M.buildArgv(name, slotsOf(name), Object.assign({ confirmed: true }, opts))).toThrow(/token_required/)
      const bound = M.buildArgv(name, slotsOf(name), Object.assign({ token: token }, opts)).argv
      expect([name, bound.includes("--dry-run"), bound[bound.indexOf("--if-match") + 1]]).toEqual([name, false, token])
    }
    // No template, in any form it builds, gives consent.
    for (const name of Object.keys(M.COMMANDS)) {
      for (const form of [{ preview: true }, {}, { token: token }, { confirmed: true }]) {
        let argv = null
        try {
          argv = M.buildArgv(name, slotsOf(name), Object.assign({}, form, opts)).argv
        } catch (e) {
          continue
        }
        expect([name, form, argv.filter((a) => CONSENT.includes(a.split("=")[0]))]).toEqual([name, form, []])
      }
    }
  })

  test("a query fills only read-only templates", () => {
    for (const name of Object.keys(M.COMMANDS).filter((n) => M.COMMANDS[n].mutates)) {
      expect(() => M.buildArgv(name, Object.assign({ query: "q" }, slotsOf(name)), Object.assign({ preview: needsPreview(name) }, opts))).toThrow(/unknown_slot/)
    }
  })
})
