// The marketplace's size limits (omad-widget-plugin-3bcbh.1). The Omarchy plugin
// marketplace's security baseline reads every text file of the commit it scans and fails
// closed on a file over 512 KiB (`securityFileByteLimit`) or a commit over 8 MiB, so a
// widget past either can never be listed: Model.js reached 538,951 bytes before it was
// split into the four files of its header. Every file of the plugin under omarchy/ (its
// tests aside) is held to 384 KiB, and all of them together to 4 MiB, which leaves room
// to grow before the scan's limits. A failure names the file, its size and the limit it
// guards. The check also runs over in-memory copies with one planted violation, which it
// must report, as tests/security.test.js does.

const { describe, expect, test } = require("bun:test")
const fs = require("fs")
const path = require("path")

const ROOT = path.join(__dirname, "..")
const KIB = 1024
const FILE_BUDGET = 384 * KIB
const SET_BUDGET = 4 * KIB * KIB
// The marketplace's own limits, which the budgets keep the plugin under.
const SCAN_FILE_LIMIT = 512 * KIB
const SCAN_SET_LIMIT = 8 * KIB * KIB

// Every file of the plugin under omarchy/, tests/ aside, as `{path: bytes}`: the runtime
// .qml, .js and .json files, and anything else a scan of the commit would read.
function pluginSizes() {
  const out = {}
  const walk = (dir) => {
    for (const entry of fs.readdirSync(path.join(ROOT, dir), { withFileTypes: true })) {
      const rel = dir ? dir + "/" + entry.name : entry.name
      if (entry.isDirectory()) {
        if (rel !== "tests") walk(rel)
      } else {
        out[rel] = fs.statSync(path.join(ROOT, rel)).size
      }
    }
  }
  walk("")
  return out
}

// What is over budget in `sizes`, one sentence each.
function overBudget(sizes) {
  const out = []
  let total = 0
  for (const name of Object.keys(sizes).sort()) {
    total += sizes[name]
    if (sizes[name] > FILE_BUDGET) {
      out.push(name + " is " + sizes[name] + " bytes, over the " + FILE_BUDGET + "-byte budget (384 KiB) that keeps " +
        "each file under the marketplace scan's " + SCAN_FILE_LIMIT + " bytes (512 KiB): split it, as Model.js is split")
    }
  }
  if (total > SET_BUDGET) {
    out.push("the plugin's files hold " + total + " bytes together, over the " + SET_BUDGET + "-byte budget (4 MiB) " +
      "that keeps the commit under the marketplace scan's " + SCAN_SET_LIMIT + " bytes (8 MiB)")
  }
  return out
}

const REAL = pluginSizes()

describe("the plugin stays inside the marketplace scan's limits", () => {
  test("every file and the whole set are within their budgets", () => {
    expect(overBudget(REAL)).toEqual([])
  })

  test("the walk reads the model's four files, the QML and the manifest", () => {
    for (const name of ["Words.js", "Shared.js", "View.js", "Model.js", "Service.qml", "manifest.json", "views/AddonPage.qml"]) {
      expect([name, typeof REAL[name]]).toEqual([name, "number"])
    }
    expect(Object.keys(REAL).some((name) => name.startsWith("tests/"))).toBe(false)
  })
})

describe("each budget catches a planted violation", () => {
  test("a file of 385 KiB", () => {
    const planted = Object.assign({}, REAL, { "views/Planted.qml": 385 * KIB })
    const found = overBudget(planted)
    expect(found.length).toBe(1)
    expect(found[0]).toStartWith("views/Planted.qml is 394240 bytes, over the 393216-byte budget")
    expect(found[0]).toContain("524288 bytes (512 KiB)")
  })

  test("a file exactly at the budget passes", () => {
    expect(overBudget(Object.assign({}, REAL, { "Planted.js": FILE_BUDGET }))).toEqual([])
  })

  test("a set past 4 MiB of files each within budget", () => {
    const planted = Object.assign({}, REAL)
    for (let i = 0; i < 11; i++) planted["planted/File" + i + ".js"] = 380 * KIB
    const found = overBudget(planted)
    expect(found.length).toBe(1)
    expect(found[0]).toStartWith("the plugin's files hold ")
    expect(found[0]).toContain("8388608 bytes (8 MiB)")
  })
})
