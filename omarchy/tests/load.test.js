// The model's four files (Model.js's header, omad-widget-plugin-3bcbh.1) and the loader
// that reads them under bun (tests/load.js). Each file imports only the one below it and
// names every top-level name of that one again, so code reads the same in every file
// and QML still reads one namespace through Model.js; QML imports Model.js alone. The
// loader turns each `.import` into the imported file's names on the same line.

const { describe, expect, test } = require("bun:test")
const fs = require("fs")
const path = require("path")
const L = require("./load.js")

const ROOT = path.join(__dirname, "..")
// Lowest first: each file imports the one before it.
const MODEL = ["Words.js", "Shared.js", "View.js", "Model.js"]
const SRC = Object.fromEntries(MODEL.map((name) => [name, fs.readFileSync(path.join(ROOT, name), "utf8")]))

// `var NAME = Q.NAME` lines of `src`, by NAME.
function aliasesOf(src, as) {
  const out = {}
  const re = new RegExp("^var ([A-Za-z_$][A-Za-z0-9_$]*) = " + as + "\\.([A-Za-z_$][A-Za-z0-9_$]*)\\s*$", "gm")
  for (const m of src.matchAll(re)) out[m[1]] = (out[m[1]] || []).concat([m[2]])
  return out
}

// Every QML file under omarchy/ (tests aside), as `{path: text}`.
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

describe("the model's files", () => {
  test("each imports only the one below it", () => {
    const imports = MODEL.map((name) => [name, L.importsOf(SRC[name]).map((i) => i.file + " as " + i.as)])
    expect(imports).toEqual([
      ["Words.js", []],
      ["Shared.js", ["Words.js as Words"]],
      ["View.js", ["Shared.js as Shared"]],
      ["Model.js", ["View.js as View"]]
    ])
  })

  test("each names every name of the one it imports again, once, and no other", () => {
    for (let i = 1; i < MODEL.length; i++) {
      const name = MODEL[i]
      const below = MODEL[i - 1]
      const as = L.importsOf(SRC[name])[0].as
      const theirs = L.topLevelNames(SRC[below])
      const aliases = aliasesOf(SRC[name], as)
      // Every name of the file below, named again as itself, exactly once.
      const missing = theirs.filter((n) => !aliases[n])
      const twice = Object.keys(aliases).filter((n) => aliases[n].length > 1 || aliases[n][0] !== n)
      const stale = Object.keys(aliases).filter((n) => theirs.indexOf(n) === -1)
      expect([name, missing, twice, stale]).toEqual([name, [], [], []])
    }
  })

  test("no file declares a name twice, so none shadows a name it imports", () => {
    for (const name of MODEL) {
      const names = L.topLevelNames(SRC[name])
      const twice = names.filter((n, i) => names.indexOf(n) !== i)
      expect([name, twice]).toEqual([name, []])
    }
  })

  test("QML imports Model.js alone, and every name it reads there is one of Model.js's", () => {
    const own = new Set(L.topLevelNames(SRC["Model.js"]))
    for (const [file, text] of Object.entries(qmlFiles())) {
      for (const lower of ["Words.js", "Shared.js", "View.js"]) {
        expect([file, text.includes('"' + lower + '"') || text.includes("/" + lower + '"')]).toEqual([file, false])
      }
      // Code only: strings (an import's "Model.js") and comments aside.
      const code = text.replace(/"(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'/g, '""').replace(/\/\/.*$/gm, "")
      const unknown = [...new Set([...code.matchAll(/\bModel\.([A-Za-z_$][A-Za-z0-9_$]*)/g)].map((m) => m[1]))]
        .filter((n) => !own.has(n))
      expect([file, unknown]).toEqual([file, []])
    }
  })

  test("bun reads the same names as QML: Model.js's export, each file's top-level names", () => {
    const M = L("Model.js")
    expect(typeof M.addonPage).toBe("function")
    expect(typeof M.COPY["decision.unknown"]).toBe("string")
    expect(M.copy("decision.unknown")).toBe("omaddons didn't say what this would change; nothing ran")
    for (const name of ["Words.js", "Shared.js", "View.js"]) {
      expect([name, Object.keys(L(name))]).toEqual([name, L.topLevelNames(SRC[name])])
    }
  })
})

describe("the loader", () => {
  test("keeps every line where it is", () => {
    for (const name of MODEL) {
      const out = L.transform(SRC[name], name).split("\n")
      const src = SRC[name].split("\n")
      for (const i of L.importsOf(SRC[name])) {
        expect(out[i.line - 1]).toEndWith("var " + i.as + " = __import(" + JSON.stringify(i.file) + ");")
      }
      // The function's head shares line 1; its return and close add two lines at the end.
      expect([name, out.length]).toEqual([name, src.length + 2])
      expect(out.slice(1, src.length).filter((l, k) => l !== src[k + 1] && !l.startsWith("var "))).toEqual([])
    }
  })

  test("reports a failure at the line of the source", () => {
    const body = L.transform('// line 1\n.import "Words.js" as Words\nfunction boom() {\n  throw new Error("x")\n}\n', "Planted.js")
    const run = require("vm").runInThisContext(body, { filename: "Planted.js" })
    const names = run({ exports: {} }, {}, () => ({}))
    let stack = ""
    try {
      names.boom()
    } catch (e) {
      stack = String(e.stack)
    }
    expect(stack).toContain("Planted.js:4:")
  })

  test("refuses a directive it would not run as QML does", () => {
    expect(() => L.transform(".pragma library\nvar x = 1\n", "Planted.js")).toThrow("Planted.js:1: a directive tests/load.js cannot run")
    expect(() => L.transform('.import QtQuick 2.0 as Q\n', "Planted.js")).toThrow("Planted.js:1")
  })

  test("loads afresh, as each QML importer gets its own copy", () => {
    expect(L("Words.js").COPY).not.toBe(L("Words.js").COPY)
  })
})
