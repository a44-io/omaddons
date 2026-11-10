// The words guard's checks (design doc §6.2, omad-e49b), as pure functions over a map
// of runtime file name to text, so tests/copy.test.js runs each over the real source and
// over a copy holding one planted violation. Four rules:
//
// - qml-literal: a QML `text:`, `tooltipText:`, `placeholderText:` or `heading:` binding,
//   or the `Accessible.name:` or `Accessible.description:` a screen reader says
//   (1n2p.40), holding a string literal a player reads: two or more words, or a capital
//   letter (copy ids, keys and slot names are lowercase single words; glyphs are code
//   points).
// - model-literal: a Model.js string literal of two or more words outside COPY,
//   ERROR_WORDS, WARNING_WORDS, KEYMAP and comments. Not counted: a command path (a
//   CLI_PATHS key), and the message of a `refuse(…)` or `Error(…)`, a diagnostic the
//   widget keys by its code, as it keys the CLI's errors (§6.1). KEYMAP is a word table
//   of its own (1n2p.37): its labels and notes are the key reference's and every
//   tooltip's words, and tests/omarchy_widget.rs holds each row equal to README's key
//   table, so they live in that one checked place, never split into COPY ids.
// - unused-id: a COPY id no runtime file names, as a literal or under a prefix the code
//   builds ids from (`"card.action." + name`).
// - code-without-words: an error or warning code of `omaddons robot-docs errors` with no
//   ERROR_WORDS or WARNING_WORDS entry ("not met" is an entry).
//
// A finding is `{rule, file, line, value}`; its key, `rule \t file \t JSON(value)`, is
// what the allowlist (tests/copy_allowlist.txt) holds.

const WORD_PROPERTIES = ["text", "tooltipText", "placeholderText", "heading", "Accessible\\.name", "Accessible\\.description"]
const DIAGNOSTIC_CALLS = ["refuse", "Error"]
const WORD_TABLES = ["COPY", "ERROR_WORDS", "WARNING_WORDS", "KEYMAP"]

// The characters after which a `/` starts a regex literal, not a division.
const REGEX_AFTER = "(,=:[!&|?{};+-*%<>~^"
const REGEX_AFTER_WORDS = ["return", "typeof", "case", "in", "of", "delete", "void"]

// Every string literal of `src` (JavaScript, or a QML expression), skipping comments and
// regex literals: `{value, line, start, end, calls}`, `line` from 1 and `calls` the names
// of the calls it is an argument of, innermost last (`Model.copy(` is "copy").
function literals(src) {
  const out = []
  const calls = []
  let i = 0
  let line = 1
  const before = () => {
    let j = i - 1
    while (j >= 0 && /\s/.test(src[j])) j--
    let k = j
    while (k >= 0 && /[A-Za-z0-9_$]/.test(src[k])) k--
    return { char: j < 0 ? "" : src[j], word: src.slice(k + 1, j + 1) }
  }
  while (i < src.length) {
    const c = src[i]
    const d = src[i + 1]
    if (c === "\n") {
      line++
      i++
    } else if (c === "/" && d === "/") {
      while (i < src.length && src[i] !== "\n") i++
    } else if (c === "/" && d === "*") {
      i += 2
      while (i < src.length && !(src[i] === "*" && src[i + 1] === "/")) {
        if (src[i] === "\n") line++
        i++
      }
      i += 2
    } else if (c === "/" && (REGEX_AFTER.includes(before().char) || before().char === "" || REGEX_AFTER_WORDS.includes(before().word))) {
      i++
      let inClass = false
      while (i < src.length && src[i] !== "\n" && (src[i] !== "/" || inClass)) {
        if (src[i] === "\\") i++
        else if (src[i] === "[") inClass = true
        else if (src[i] === "]") inClass = false
        i++
      }
      i++
      while (i < src.length && /[a-z]/.test(src[i])) i++
    } else if (c === '"' || c === "'" || c === "`") {
      const start = i
      const startLine = line
      let value = ""
      i++
      while (i < src.length && src[i] !== c) {
        if (src[i] === "\\") {
          value += src[i + 1] === "n" ? "\n" : src[i + 1]
          i += 2
          continue
        }
        if (src[i] === "\n") line++
        value += src[i]
        i++
      }
      i++
      out.push({ value, line: startLine, start, end: i, calls: calls.slice() })
    } else if (c === "(") {
      const word = before().word
      calls.push(word)
      i++
    } else if (c === ")") {
      calls.pop()
      i++
    } else {
      i++
    }
  }
  return out
}

// The words of `text`: its whitespace-separated pieces that hold a letter.
function words(text) {
  return String(text).split(/\s+/).filter((piece) => /[A-Za-z]/.test(piece)).length
}

// The span `[open, close]` of each `var NAME = {…}` table of `src` named in `names`,
// matched brace for brace outside strings and comments.
function tableSpans(src, names) {
  const spans = []
  for (const name of names) {
    // An object table (`var COPY = {`), or a list of rows (`var KEYMAP = [`).
    const brace = src.indexOf("var " + name + " = {")
    const bracket = src.indexOf("var " + name + " = [")
    const head = brace >= 0 ? brace : bracket
    if (head < 0) continue
    const opener = brace >= 0 ? "{" : "["
    const open = src.indexOf(opener, head)
    let depth = 0
    for (const token of braces(src, open, opener)) {
      depth += token.char === opener ? 1 : -1
      if (depth === 0) {
        spans.push([open, token.at])
        break
      }
    }
  }
  return spans
}

// The braces of `src` from `from` on, outside strings and comments.
function* braces(src, from, opener) {
  const open = opener || "{"
  const close = open === "[" ? "]" : "}"
  let i = from
  while (i < src.length) {
    const c = src[i]
    if (c === "/" && src[i + 1] === "/") {
      while (i < src.length && src[i] !== "\n") i++
    } else if (c === "/" && src[i + 1] === "*") {
      i = src.indexOf("*/", i + 2) + 2
      if (i < 2) return
    } else if (c === '"' || c === "'" || c === "`") {
      i++
      while (i < src.length && src[i] !== c) i += src[i] === "\\" ? 2 : 1
      i++
    } else {
      if (c === open || c === close) yield { char: c, at: i }
      i++
    }
  }
}

function inside(spans, literal) {
  return spans.some(([open, close]) => literal.start > open && literal.end <= close)
}

// A QML literal a player reads: a letter, and two words or a capital.
function qmlProse(value) {
  return /[A-Za-z]/.test(value) && (words(value) >= 2 || /[A-Z]/.test(value))
}

// Whether a binding that ends with `expr` goes on to the `next` line: brackets still open,
// an operator at its end, or one at the start of the next.
function continues(expr, next) {
  if (next === undefined) return false
  const bare = expr.replace(/"(?:[^"\\]|\\.)*"|'(?:[^'\\]|\\.)*'/g, '""').replace(/\/\/.*$/gm, "")
  const open = (bare.match(/[([{]/g) || []).length - (bare.match(/[)\]}]/g) || []).length
  return open > 0 || /[?:+,(&|]\s*$/.test(bare) || /^\s*(\?|:|\+|&&|\|\||\.)/.test(next)
}

// Rule qml-literal over every `.qml` file of `files`.
function qmlFindings(files) {
  const out = []
  const property = new RegExp("^\\s*(" + WORD_PROPERTIES.join("|") + ")\\s*:\\s*(.*)$")
  for (const name of Object.keys(files).filter((n) => n.endsWith(".qml")).sort()) {
    const lines = files[name].split("\n")
    for (let i = 0; i < lines.length; i++) {
      const match = property.exec(lines[i])
      if (!match) continue
      let expr = match[2]
      let j = i
      while (continues(expr, lines[j + 1])) {
        j++
        expr += "\n" + lines[j]
      }
      for (const literal of literals(expr)) {
        if (qmlProse(literal.value)) {
          out.push({ rule: "qml-literal", file: name, line: i + literal.line, value: literal.value })
        }
      }
      i = j
    }
  }
  return out
}

// Rule model-literal over Model.js; `commandPaths` are the CLI paths it may name.
function modelFindings(files, commandPaths) {
  const src = files["Model.js"] || ""
  const spans = tableSpans(src, WORD_TABLES)
  const paths = new Set(commandPaths)
  const out = []
  for (const literal of literals(src)) {
    if (inside(spans, literal)) continue
    if (literal.calls.some((call) => DIAGNOSTIC_CALLS.includes(call))) continue
    if (paths.has(literal.value)) continue
    if (words(literal.value) >= 2) {
      out.push({ rule: "model-literal", file: "Model.js", line: literal.line, value: literal.value })
    }
  }
  return out
}

// COPY's ids, in source order: its top-level keys (two-space indent; a plural pair's
// "one" and "other" sit deeper).
function copyIds(files) {
  const src = files["Model.js"] || ""
  const span = tableSpans(src, ["COPY"])[0]
  if (!span) return []
  const body = src.slice(span[0], span[1])
  return [...body.matchAll(/^  "([^"\\]+)"\s*:/gm)].map((m) => m[1])
}

// What the runtime files name outside COPY: every literal, and every id-shaped one that
// ends in "." (`"card.action."`, `known("stale.changed.")`), a prefix the code builds ids
// from; tests/copy.test.js pins that list, so a new prefix is a deliberate edit.
function namesOutsideCopy(files) {
  const named = new Set()
  const prefixes = new Set()
  for (const name of Object.keys(files).sort()) {
    const src = files[name]
    const spans = name === "Model.js" ? tableSpans(src, ["COPY"]) : []
    for (const literal of literals(src)) {
      if (inside(spans, literal)) continue
      named.add(literal.value)
      if (/^[a-z][a-z0-9_.]*\.$/.test(literal.value)) prefixes.add(literal.value)
    }
  }
  return { named, prefixes }
}

// Rule unused-id.
function unusedFindings(files) {
  const { named, prefixes } = namesOutsideCopy(files)
  const src = files["Model.js"] || ""
  return copyIds(files)
    .filter((id) => !named.has(id) && ![...prefixes].some((prefix) => id.startsWith(prefix)))
    .map((id) => ({ rule: "unused-id", file: "Model.js", line: src.slice(0, src.indexOf('"' + id + '"')).split("\n").length, value: id }))
}

// Rule code-without-words: `catalog` is `robot-docs errors --json`'s envelope.
function codeFindings(catalog, errorWords, warningWords) {
  const data = (catalog && catalog.data) || {}
  const out = []
  for (const [kind, list, table] of [["error", data.error_codes, errorWords], ["warning", data.warning_codes, warningWords]]) {
    for (const entry of list || []) {
      const code = entry && entry.code
      if (typeof code === "string" && !Object.prototype.hasOwnProperty.call(table || {}, code)) {
        out.push({ rule: "code-without-words", file: kind, line: 0, value: code })
      }
    }
  }
  return out
}

function key(finding) {
  return finding.rule + "\t" + finding.file + "\t" + JSON.stringify(finding.value)
}

// The allowlist's keys, in file order (blank lines and `#` comments aside).
function parseAllowlist(text) {
  return String(text)
    .split("\n")
    .filter((line) => line.trim() !== "" && !line.startsWith("#"))
}

module.exports = {
  WORD_PROPERTIES,
  literals,
  words,
  tableSpans,
  qmlFindings,
  modelFindings,
  copyIds,
  namesOutsideCopy,
  unusedFindings,
  codeFindings,
  key,
  parseAllowlist
}
