// Loads the widget's model under bun as QML loads it (omad-widget-plugin-3bcbh.1).
//
// The model is four files (Words.js, Shared.js, View.js, Model.js; Model.js's header
// says why), each reading the one below it through QML's `.import "X.js" as X`, a
// directive bun cannot parse. load("Model.js") reads a file, turns each such line into
// `var X = <X.js's names>` on the same line, so every line number stays the source's,
// and runs it. It returns what the file exports (Model.js's guarded export), else its
// top-level names, as QML gives them to an importer. Every call loads afresh, as each
// QML importer gets its own copy; nothing is cached and nothing is written.
//
// Any other directive (`.pragma`, a module `.import`) throws: the model has none, and
// this loader would not run it as QML does.

const fs = require("fs")
const path = require("path")
const vm = require("vm")

const ROOT = path.join(__dirname, "..")

// `.import "<file>.js" as <Name>`, the one directive the model uses.
const IMPORT = /^\.import "([^"\\]+\.js)" as ([A-Za-z_$][A-Za-z0-9_$]*)\s*$/

// The names a file declares at the top level: the widget writes every one at column 0
// as `var NAME` or `function NAME(`, and nests nothing there.
function topLevelNames(src) {
  return [...src.matchAll(/^(?:var|function)\s+([A-Za-z_$][A-Za-z0-9_$]*)/gm)].map((m) => m[1])
}

// Each `.import` of `src`: `{line, file, as}`, `line` from 1.
function importsOf(src) {
  const out = []
  src.split("\n").forEach((text, i) => {
    const m = IMPORT.exec(text)
    if (m) out.push({ line: i + 1, file: m[1], as: m[2] })
  })
  return out
}

// `src` as bun runs it: a function of `(module, exports, __import)` that returns the
// file's top-level names, each `.import` line now `var X = __import("X.js");`. The
// function's head stays on the file's first line, so line N of the file is line N.
// `label` names the file in an error.
function transform(src, label) {
  const lines = src.split("\n").map((text, i) => {
    if (!text.startsWith(".")) return text
    const m = IMPORT.exec(text)
    if (!m) throw new Error(label + ":" + (i + 1) + ": a directive tests/load.js cannot run: " + text)
    return "var " + m[2] + " = __import(" + JSON.stringify(m[1]) + ");"
  })
  const names = topLevelNames(src)
  return "(function (module, exports, __import) {" + lines.join("\n") +
    "\n;return {" + names.map((n) => JSON.stringify(n) + ": " + n).join(", ") + "}\n})"
}

// `name` is relative to omarchy/, or absolute.
function load(name) {
  const file = path.isAbsolute(name) ? name : path.join(ROOT, name)
  const body = transform(fs.readFileSync(file, "utf8"), path.relative(ROOT, file))
  const run = vm.runInThisContext(body, { filename: file })
  const module = { exports: {} }
  const declared = run(module, module.exports, (rel) => load(path.join(path.dirname(file), rel)))
  return Object.keys(module.exports).length > 0 ? module.exports : declared
}

module.exports = load
module.exports.load = load
module.exports.topLevelNames = topLevelNames
module.exports.importsOf = importsOf
module.exports.transform = transform
module.exports.ROOT = ROOT
