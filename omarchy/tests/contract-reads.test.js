// The widget reads only what the contract declares (H1's guard, M4-D55, risk 5 of M4): every
// envelope key a Model.js reader touches, over every envelope the real binary wrote
// (pipeline step 1), and every literal `env.…` read in the QML, must match a path the
// published JSON Schemas declare. A reducer reads defensively, so a key renamed by the
// pre-freeze audit would read as `undefined` and pass model.test.js; here it fails, naming
// the reader, the fixture and the closest declared path.
//
// Readers are found, not listed: every Model.js export declared `function <name>(env…)`.
// Keep that style for a new reader (an arrow function is not found). The guard's inputs,
// contract-schemas.json (`robot-docs schema --json`) and contract-errors.json (`robot-docs
// errors --json`), are written beside the fixtures by write_widget_fixtures.

const { describe, expect, test } = require("bun:test")
const fs = require("fs")
const path = require("path")
const M = require("../Model.js")

const OUT = process.env.OMADDONS_WIDGET_OUT
if (!OUT) throw new Error("OMADDONS_WIDGET_OUT is unset: run the widget pipeline (argc widget-test)")
const FIXTURES = path.join(OUT, "fixtures")
const INPUTS = new Set(["contract-schemas.json", "contract-errors.json"])
const WIDGET = path.join(__dirname, "..")

// model.test.js's NOW: a fixed instant past every fixture's timestamps.
const NOW = Date.parse("2100-01-01T00:00:00Z")

// ---------------------------------------------------------------- paths and patterns

// A path is an array of segments: a key, "[]" for any array item, "{}" for any one key of
// a map, "**" for anything below. Shown as `data.targets[].updates`.
function show(segments) {
  var out = ""
  for (var i = 0; i < segments.length; i++) {
    var s = segments[i]
    out += s === "[]" ? "[]" : (out ? "." : "") + s
  }
  return out
}

function keyOf(segments) {
  return segments.join("\u0001")
}

// Whether a declared pattern admits a read path.
function admits(pattern, read) {
  for (var i = 0; i < pattern.length; i++) {
    if (pattern[i] === "**") return true
    if (i >= read.length) return false
    if (pattern[i] === "{}") {
      if (read[i] === "[]") return false
      continue
    }
    if (pattern[i] !== read[i]) return false
  }
  return pattern.length === read.length
}

// ---------------------------------------------------------------- the declared paths

// The envelope leaves `data` to the command schemas (it declares only that `data` exists),
// so its walk stops there instead of declaring everything below it.
const ENVELOPE_SLOTS = new Set(["data"])

function isScalar(node) {
  if (node.const !== undefined || Array.isArray(node.enum)) return true
  var types = Array.isArray(node.type) ? node.type : node.type === undefined ? null : [node.type]
  return types !== null && types.indexOf("object") < 0 && types.indexOf("array") < 0
}

// Walks one schema (its own `$defs`), adding each path it declares to `out`.
function walk(root, node, segments, out, refs, slots) {
  if (node === true) {
    out.set(keyOf(segments.concat("**")), segments.concat("**"))
    return
  }
  if (node === false || node === null || typeof node !== "object") return
  if (segments.length) out.set(keyOf(segments), segments)
  if (slots.has(show(segments))) return
  var unions = false
  if (typeof node.$ref === "string") {
    var ref = /^#\/\$defs\/(.+)$/.exec(node.$ref)
    if (!ref) throw new Error("unexpected $ref " + node.$ref + " at " + show(segments))
    var def = root.$defs && root.$defs[ref[1]]
    if (def === undefined) throw new Error("unresolved $ref " + node.$ref + " at " + show(segments))
    // A type that contains itself declares nothing new below the repeat.
    if (refs.indexOf(ref[1]) < 0) walk(root, def, segments, out, refs.concat(ref[1]), slots)
    unions = true
  }
  ;["oneOf", "anyOf", "allOf"].forEach(function (keyword) {
    ;(node[keyword] || []).forEach(function (branch) {
      unions = true
      walk(root, branch, segments, out, refs, slots)
    })
  })
  var structural = false
  if (node.properties) {
    structural = true
    Object.keys(node.properties).forEach(function (key) {
      walk(root, node.properties[key], segments.concat(key), out, refs, slots)
    })
  }
  if (node.items !== undefined) {
    structural = true
    walk(root, node.items, segments.concat("[]"), out, refs, slots)
  }
  ;(node.prefixItems || []).forEach(function (item) {
    structural = true
    walk(root, item, segments.concat("[]"), out, refs, slots)
  })
  if (node.additionalProperties !== undefined) {
    structural = true
    walk(root, node.additionalProperties, segments.concat("{}"), out, refs, slots)
  }
  if (node.patternProperties) {
    structural = true
    Object.keys(node.patternProperties).forEach(function (pattern) {
      walk(root, node.patternProperties[pattern], segments.concat("{}"), out, refs, slots)
    })
  }
  // An object the schema leaves open (no keys, no items, no union): anything below it.
  if (!structural && !unions && !isScalar(node) && segments.length) {
    out.set(keyOf(segments.concat("**")), segments.concat("**"))
  }
}

// Every path the published contract declares: each command's data under `data.` (the
// doctor tree's report at the top level, M4-D34), the envelope's own keys, and every
// error code's details under `error.details.`.
function declaredPaths(schemas, errors) {
  var out = new Map()
  Object.keys(schemas.commands).forEach(function (command) {
    var doctor = command === "doctor" || command.indexOf("doctor ") === 0
    var schema = schemas.commands[command]
    walk(schema, schema, doctor ? [] : ["data"], out, [], new Set())
  })
  walk(schemas.envelope, schemas.envelope, [], out, [], ENVELOPE_SLOTS)
  errors.error_codes.forEach(function (row) {
    walk(row.details_schema, row.details_schema, ["error", "details"], out, [], new Set())
  })
  return Array.from(out.values())
}

// ---------------------------------------------------------------- the recorder

const NOT_KEYS = new Set(["toJSON", "then", "constructor"])
const INDEX = /^(0|[1-9][0-9]*)$/

// A proxy of `env` that records the path of every key read through it (`get`, `in` and
// own-property checks), one proxy per object, so identity and Array.isArray hold.
function recording(env) {
  var reads = new Map()
  var proxies = new WeakMap()
  function record(segments) {
    reads.set(keyOf(segments), segments)
  }
  function pathOf(target, key, segments) {
    if (typeof key === "symbol" || NOT_KEYS.has(key)) return null
    if (Array.isArray(target)) {
      if (INDEX.test(key)) return segments.concat("[]")
      if (key in Array.prototype) return null
    } else if (key in Object.prototype) {
      return null
    }
    return segments.concat(key)
  }
  function wrap(value, segments) {
    if (value === null || typeof value !== "object") return value
    var known = proxies.get(value)
    if (known) return known
    var proxy = new Proxy(value, {
      get: function (target, key, receiver) {
        var child = pathOf(target, key, segments)
        var result = Reflect.get(target, key, receiver)
        if (!child) return result
        record(child)
        return wrap(result, child)
      },
      has: function (target, key) {
        var child = pathOf(target, key, segments)
        if (child) record(child)
        return Reflect.has(target, key)
      },
      getOwnPropertyDescriptor: function (target, key) {
        var child = pathOf(target, key, segments)
        if (child) record(child)
        return Reflect.getOwnPropertyDescriptor(target, key)
      },
    })
    proxies.set(value, proxy)
    return proxy
  }
  return { proxy: wrap(env, []), reads: reads }
}

// ---------------------------------------------------------------- the readers and fixtures

const READER = /^function\s*\w*\s*\(\s*env\b/

function readersOf(model) {
  return Object.keys(model)
    .filter(function (name) {
      return typeof model[name] === "function" && READER.test(model[name].toString())
    })
    .sort()
    .map(function (name) {
      return { name: name, fn: model[name] }
    })
}

// The arguments after `env` a reader needs to read on: the fixture's own build for the
// version checks, so they get past `tool_version`, and the clock for the status reducer.
function extraArguments(name, raw) {
  if (name === "reduceStatus") return [NOW]
  if (name === "compatible" || name === "incompatibleText") return [{ TOOL_VERSION: raw.tool_version }]
  return []
}

function envelopeFixtures() {
  return fs
    .readdirSync(FIXTURES)
    .filter(function (file) {
      return file.endsWith(".json") && !INPUTS.has(file)
    })
    .sort()
    .map(function (file) {
      return { file: file, env: JSON.parse(fs.readFileSync(path.join(FIXTURES, file), "utf8")) }
    })
}

function contract() {
  var read = function (file) {
    return JSON.parse(fs.readFileSync(path.join(FIXTURES, file), "utf8")).data
  }
  return { schemas: read("contract-schemas.json"), errors: read("contract-errors.json") }
}

// ---------------------------------------------------------------- the check

function distance(a, b) {
  var previous = []
  for (var j = 0; j <= b.length; j++) previous.push(j)
  for (var i = 1; i <= a.length; i++) {
    var current = [i]
    for (var k = 1; k <= b.length; k++) {
      var cost = a[i - 1] === b[k - 1] ? 0 : 1
      current.push(Math.min(previous[k] + 1, current[k - 1] + 1, previous[k - 1] + cost))
    }
    previous = current
  }
  return previous[b.length]
}

function closest(read, patterns) {
  var target = show(read)
  var best = null
  var bestDistance = Infinity
  patterns.forEach(function (pattern) {
    var text = show(pattern)
    var d = distance(target, text)
    if (d < bestDistance || (d === bestDistance && text < best)) {
      best = text
      bestDistance = d
    }
  })
  return best
}

function undeclared(patterns, read) {
  return !patterns.some(function (pattern) {
    return admits(pattern, read)
  })
}

// Runs every reader over every fixture through the recorder; returns each undeclared read
// once per reader and fixture, with its message, and how many distinct paths were read.
function undeclaredReads(readers, fixtures, patterns) {
  var found = []
  var verdicts = new Map()
  readers.forEach(function (reader) {
    fixtures.forEach(function (fixture) {
      var rec = recording(fixture.env)
      try {
        reader.fn.apply(null, [rec.proxy].concat(extraArguments(reader.name, fixture.env)))
      } catch (_) {
        // Another command's envelope may make a reader throw; what it read first counts.
      }
      rec.reads.forEach(function (read, key) {
        if (!verdicts.has(key)) verdicts.set(key, undeclared(patterns, read))
        if (!verdicts.get(key)) return
        found.push({
          reader: reader.name,
          fixture: fixture.file,
          path: show(read),
          message:
            "Model.js reads " + show(read) + " (reader " + reader.name + ", fixture " + fixture.file +
            ") but no published schema declares it; closest declared: " + closest(read, patterns),
        })
      })
    })
  })
  return { found: found, distinct: verdicts.size }
}

// Every literal `env.<data|error|meta|commands|warnings>…` read in the widget's QML, as a
// path (`[…]` indexes as "[]"; `.length` is an array's, not a key).
function qmlReads(root) {
  var files = []
  ;(function collect(dir) {
    fs.readdirSync(dir, { withFileTypes: true }).forEach(function (entry) {
      var full = path.join(dir, entry.name)
      if (entry.isDirectory()) collect(full)
      else if (entry.name.endsWith(".qml")) files.push(full)
    })
  })(root)
  var reads = []
  var pattern = /\benv\.(data|error|meta|commands|warnings)((?:\.[A-Za-z_][A-Za-z0-9_]*|\[[^\]]+\])*)/g
  files.sort().forEach(function (file) {
    fs.readFileSync(file, "utf8")
      .split("\n")
      .forEach(function (line, index) {
        var match
        pattern.lastIndex = 0
        while ((match = pattern.exec(line)) !== null) {
          var segments = [match[1]]
          var rest = match[2].replace(/\[[^\]]+\]/g, ".[]").split(".").filter(Boolean)
          rest.forEach(function (segment) {
            if (segment !== "length") segments.push(segment)
          })
          reads.push({ file: path.relative(root, file), line: index + 1, path: segments })
        }
      })
  })
  return reads
}

// ---------------------------------------------------------------- the tests

describe("the widget reads only declared keys", function () {
  const { schemas, errors } = contract()
  const patterns = declaredPaths(schemas, errors)
  const readers = readersOf(M)
  const fixtures = envelopeFixtures()

  test("the guard exercised readers and fixtures", function () {
    expect(readers.length, "readers found in Model.js").toBeGreaterThanOrEqual(1)
    expect(fixtures.length, "envelope fixtures in " + FIXTURES).toBeGreaterThanOrEqual(10)
    expect(patterns.length, "declared paths").toBeGreaterThanOrEqual(100)
  })

  test("the declared paths leave nothing open at the top", function () {
    var open = patterns.map(show).filter(function (text) {
      return ["**", "data.**", "error.**", "error.details.**", "commands.**", "warnings.**"].indexOf(text) >= 0
    })
    expect(open, "a wildcard here would declare every read below it").toEqual([])
  })

  test("every key the reducers read is declared", function () {
    var result = undeclaredReads(readers, fixtures, patterns)
    var found = result.found
    // The readers read about 280 distinct paths over today's fixtures: far fewer means the
    // recorder or the readers' discovery broke, and the check below proves nothing.
    expect(result.distinct, "distinct paths the readers read").toBeGreaterThanOrEqual(100)
    if (found.length) {
      console.error(
        found
          .map(function (f) {
            return f.message
          })
          .join("\n") + "\n" + found.length + " undeclared reads"
      )
    }
    expect(found.map(function (f) { return f.message }), "undeclared reads").toEqual([])
  })

  test("every literal env read in the QML is declared", function () {
    var reads = qmlReads(WIDGET)
    expect(reads.length, "literal env reads found in the QML").toBeGreaterThanOrEqual(1)
    var found = reads
      .filter(function (r) {
        return undeclared(patterns, r.path)
      })
      .map(function (r) {
        return r.file + ":" + r.line + " reads " + show(r.path) + " but no published schema declares it; closest declared: " + closest(r.path, patterns)
      })
    if (found.length) console.error(found.join("\n") + "\n" + found.length + " undeclared QML reads")
    expect(found, "undeclared QML reads").toEqual([])
  })
})

describe("the guard's own parts", function () {
  test("declaredPaths flattens refs, maps, unions and opaque nodes", function () {
    var schemas = {
      commands: {
        install: {
          $defs: {
            Version: { type: "object", properties: { tag: { type: "string" } } },
          },
          type: "object",
          properties: {
            from: { $ref: "#/$defs/Version" },
            to: { anyOf: [{ $ref: "#/$defs/Version" }, { type: "null" }] },
            by_target: { type: "object", additionalProperties: { type: "integer" } },
            items: { type: "array", items: { oneOf: [{ properties: { a: { type: "string" } } }, { properties: { b: { type: "string" } } }] } },
          },
        },
        "doctor explain": { type: "object", properties: { instances: { type: "array", items: { type: "object", additionalProperties: true } } } },
      },
      envelope: { type: "object", properties: { ok: { type: "boolean" }, data: { description: "the command's" } } },
    }
    var errors = { error_codes: [{ code: "x", details_schema: { type: "object", properties: { kept: { type: "object" } } } }] }
    var got = declaredPaths(schemas, errors).map(show).sort()
    expect(got, "the synthetic contract's paths").toEqual(
      [
        "data.from",
        "data.from.tag",
        "data.to",
        "data.to.tag",
        "data.by_target",
        "data.by_target.{}",
        "data.items",
        "data.items[]",
        "data.items[].a",
        "data.items[].b",
        "instances",
        "instances[]",
        "instances[].{}.**",
        "ok",
        "data",
        "error.details",
        "error.details.kept",
        "error.details.kept.**",
      ].sort()
    )
  })

  test("a planted undeclared read is reported", function () {
    const { schemas, errors } = contract()
    var patterns = declaredPaths(schemas, errors)
    function planted(env) {
      return env.data.no_such_key
    }
    var fixture = envelopeFixtures().find(function (f) {
      return f.file === "status-updates.json"
    })
    expect(fixture, "the status-updates fixture").toBeDefined()
    var found = undeclaredReads([{ name: "planted", fn: planted }], [fixture], patterns).found
    expect(found.map(function (f) { return f.path }), "the planted read alone").toEqual(["data.no_such_key"])
    expect(found[0].message, "the message").toStartWith(
      "Model.js reads data.no_such_key (reader planted, fixture status-updates.json) but no published schema declares it; closest declared: data."
    )
  })

  test("the recorder maps indexes to []", function () {
    var rec = recording({ data: { targets: [{ updates: [1, 2] }] } })
    var length = rec.proxy.data.targets[0].updates.length
    expect(length, "the read went through").toBe(2)
    expect(Array.isArray(rec.proxy.data.targets), "an array stays an array").toBe(true)
    expect(rec.proxy.data === rec.proxy.data, "one proxy per object").toBe(true)
    var got = Array.from(rec.reads.values()).map(show).sort()
    expect(got, "the recorded paths").toEqual(["data", "data.targets", "data.targets[]", "data.targets[].updates"])
  })

  test("the QML scan maps indexes to [] and drops length", function () {
    var dir = fs.mkdtempSync(path.join(OUT, "qml-scan-"))
    fs.writeFileSync(path.join(dir, "A.qml"), "x = env.commands.length + env.data.targets[i].id\n")
    var got = qmlReads(dir).map(function (r) {
      return r.file + ":" + r.line + " " + show(r.path)
    })
    expect(got, "the scanned reads").toEqual(["A.qml:1 commands", "A.qml:1 data.targets[].id"])
  })
})
