// The widget's security scans (F7, pipeline step 2): text lints over every runtime file
// under omarchy/ (tests aside) that fail when a code path goes around a mechanism that
// keeps the widget safe. The mechanisms are the guarantees (PlainLabel, Model.buildArgv,
// Model.cliArgv with its classifier, the stamped context, the preview flow, a plan's
// apply only from Model.planFrom); the scans only catch an accidental way around them,
// so each one also runs over an in-memory copy with one planted violation, which it
// must report. Checks 3b, 9 and 10 (M4-D28, M4-D48) hold the plan runner: an apply argv
// only from the preview the widget just ran, no decision from a warning's text, and no
// issue matched by its id. The plugin-id literal (check 8) is scanned again, over the
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
// with the text as one argument.
const LITERAL_TOOLS = ["inotifywait", "omarchy-notification-send", "xdg-open", "omarchy-launch-browser"]
const ARGV_BUILDERS = { "toastArgv": "omarchy-notification-send", "errorToastArgv": "omarchy-notification-send",
  "updateToastArgv": "omarchy-notification-send", "resultToastArgv": "omarchy-notification-send", "addonsDirArgv": "xdg-open",
  "copyArgv": "wl-copy" }
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
  // Processes: a literal tool, the exact reinstall, or the lane's argv.
  for (const name of qmlNames(files)) {
    for (const l of lines(files[name])) {
      const prop = l.text.match(/^\s*command:\s*(.*)$/)
      const assigned = l.text.match(/\w+\.command\s*=\s*(.*)$/)
      if (prop) {
        const tool = prop[1].match(/^\[\s*"([^"]+)"/)
        if (!tool || !LITERAL_TOOLS.includes(tool[1])) out.push(violation(name, l.n, "a Process command from no allowed source: " + prop[1].slice(0, 60)))
      }
      if (assigned && assigned[1].trim() !== REINSTALL) out.push(violation(name, l.n, "a Process command from no allowed source: " + assigned[1].slice(0, 60)))
    }
  }
  const service = codeOf(files["Service.qml"] || "")
  for (const c of calls(files, /\.run\((?=r,)/, ["Service.qml"])) {
    if (!c.args.startsWith("(r, built.argv, built.env)")) out.push(violation("Service.qml", c.line, "a lane runs an argv not from argvOf"))
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
  // runPlan applies only a decision's plan or the pending confirmation's.
  for (const c of calls({ "Service.qml": files["Service.qml"] || "" }, /(?<!function )\brunPlan\(/)) {
    const from = c.args.match(/^\((decision|pending)\.plan,/)
    if (!from) out.push(violation("Service.qml", c.line, notFromPlan + ": runPlan" + c.args.slice(0, 40)))
    else if (from[1] === "decision" && !/var decision = Model\.previewDecision\(/.test(enclosing(lineIndex(service, c.line)).body)) {
      out.push(violation("Service.qml", c.line, notFromPlan + ": decision is not Model.previewDecision's"))
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
    ["function runCliArgv(", /if \(verdict\.destructive\) \{[^}]*previewOf\(spec, "", false\)/],
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
  "reduceStatus": "a failed status's error.message, for the bar's tooltip",
  "issueSections": "each warning warningsOf reduced, as its row's title"
}
// The plan runner's decisions (M4-D28): typed fields only.
const DECISIONS = ["planFrom", "approvalOf", "requestedItems", "selectionMatches", "previewDecision", "reasonText", "changeLines"]
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

// 12. Irreversible stays out (1n2p.43): no template holds a purge, the classifier refuses
// one, and no other file names the flag, so no Enter and no Yes ever deletes what no undo
// brings back. (Scan 11, consent stays a person's, belongs to M4 G2.)
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

// 7. Notifications (M3-D14, M3-D21): the error toast sees names, counts and clients.
function checkNotifications(files) {
  const out = []
  const model = files["Model.js"] || ""
  const toast = functionBody(model, "errorToastArgv")
  const facts = functionBody(model, "errorFacts")
  for (const [name, fn] of [["errorToastArgv", toast], ["errorFacts", facts]]) {
    if (!fn) {
      out.push(violation("Model.js", 1, name + " is missing"))
      continue
    }
    for (const field of ERROR_FIELDS) {
      if (new RegExp("\\b" + field + "\\b").test(fn.body)) out.push(violation("Model.js", fn.line, name + " reads " + field))
    }
  }
  if (toast) {
    const read = new Set([...toast.body.matchAll(/facts\.(\w+)/g)].map((m) => m[1]))
    for (const field of read) if (!["names", "counts", "targets"].includes(field)) out.push(violation("Model.js", toast.line, "errorToastArgv reads facts." + field))
  }
  for (const c of calls(files, /\bModel\.(errorToastArgv|updateToastArgv|resultToastArgv)\(/, qmlNames(files))) {
    for (const field of ERROR_FIELDS) {
      if (new RegExp("\\b" + field + "\\b").test(c.args)) out.push(violation(c.file, c.line, c.match[1] + " is passed " + field))
    }
    if (c.match[1] === "errorToastArgv" && !/^\(Model\.errorFacts\(\w+\), Build\.PLUGIN_ID\)$/.test(c.args)) {
      out.push(violation(c.file, c.line, "errorToastArgv takes Model.errorFacts(…) only"))
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
  for (const c of calls(files, /\bModel\.(errorToastArgv|updateToastArgv|resultToastArgv)\(/, qmlNames(files))) {
    if (!/, Build\.PLUGIN_ID\)$/.test(c.args)) out.push(violation(c.file, c.line, c.match[1] + " summons Build.PLUGIN_ID"))
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
  ["12. irreversible stays out", checkIrreversible],
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
  const slotsOf = (name) => Object.fromEntries((M.COMMANDS[name].argv.join(" ").match(/\{(\w+)\}/g) || []).map((s) => [s.slice(1, -1), "x"]))

  test("every previewed template and destructive path refuses to run without its preview", () => {
    const needing = Object.keys(M.COMMANDS).filter(needsPreview)
    expect(needing.sort()).toEqual(["install", "prune_backup", "reinstall", "remove", "restore", "restore_addon",
      "update_addon", "update_all", "update_target"])
    for (const name of needing) {
      // Its Yes is its preview's plan, applied (M4-D28): the real form never builds.
      expect(() => M.buildArgv(name, slotsOf(name), opts)).toThrow(/preview_required/)
      expect(M.buildArgv(name, slotsOf(name), Object.assign({ preview: true }, opts)).argv).toContain("--dry-run")
    }
    for (const [cliPath, row] of Object.entries(M.CLI_PATHS)) {
      if (!row.destructive) continue
      expect(() => M.cliArgv(["omaddons"].concat(cliPath.split(" ")).concat(["-t", "retail"]), opts)).toThrow(/destructive_without_preview/)
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
      // A `bind` template runs with its view's token.
      const bound = M.COMMANDS[name].bind ? { token: "pc1:" + "0".repeat(64) } : {}
      const built = M.buildArgv(name, slotsOf(name), Object.assign({ preview: needsPreview(name) }, bound, opts))
      expect(built.env).toEqual(CONTEXT)
    }
  })

  test("a query fills only read-only templates", () => {
    for (const name of Object.keys(M.COMMANDS).filter((n) => M.COMMANDS[n].mutates)) {
      expect(() => M.buildArgv(name, Object.assign({ query: "q" }, slotsOf(name)), Object.assign({ preview: needsPreview(name) }, opts))).toThrow(/unknown_slot/)
    }
  })
})
