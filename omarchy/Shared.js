// What the views share with the rest of the model: the slot patterns a view checks
// before it offers an action (buildArgv checks the same ones again), the envelope check,
// text helpers (str, num, plural, joinNames, ages, dates and clock times), the safe-link
// check, and the few helpers of the plan runner and the plan sheet the views use.
//
// The second of the model's four files (Model.js says how they fit): it imports
// Words.js and names its words again, and View.js and Model.js name each of these again
// in turn. Every declaration came here whole, with its comment, from the Model.js
// section its rule names (omad-widget-plugin-3bcbh.1).
//
// No `.pragma`: each importer gets its own copy. Engine-neutral JavaScript only (no QML
// globals, ES2017 at most, `var`/`function`), so Qt's V4 and bun behave alike. bun loads
// it through tests/load.js, which gives its top-level names as QML's `.import` does.
.import "Words.js" as Words

// Words.js's names, again.
var COPY = Words.COPY
var ERROR_WORDS = Words.ERROR_WORDS
var WARNING_WORDS = Words.WARNING_WORDS
var copy = Words.copy

// ------------------------------------------------------------- from building argv

// A release as `list` names it (`latest.release_id`, RG-36): `<ref>@<tag>`, the ref one
// of omaddons' source prefixes, one line with no space, at most 512 characters. Skip this
// release and Offer again pass it whole (`--release=`), never a bare version label, which
// would widen "this release" to every release that reads the same. One that is not this
// shape is shown, never offered.
var RELEASE_ID_PATTERN = /^[a-z]+:[^\s@\u0000-\u001f\u007f][^\s\u0000-\u001f\u007f]*@[^\s@\u0000-\u001f\u007f]+$/
var RELEASE_ID_MAX_CHARS = 512
// A release's source revision (`latest.revision`, a WowAce re-tag's): a u32's digits.
var REVISION_PATTERN = /^(0|[1-9][0-9]{0,9})$/
// A release's version as a source lists it (an addon page's Versions, 1n2p.24), for
// `install --version=`: a tag or a revision, one word that starts with a letter or digit,
// at most 128 characters. A listed version that is not one is shown, never offered.
var VERSION_PATTERN = /^[A-Za-z0-9][A-Za-z0-9._+~@\/-]{0,127}$/
// The channels `update --channel=` takes (its `possible values`), least stable last.
var CHANNELS = ["stable", "beta", "alpha"]
var LABEL_MAX_CHARS = 120

// --------------------------------------------------------- from compatibility (M3-D42)

function envelopeOk(env) {
  return !!env && typeof env === "object" && !Array.isArray(env)
    && typeof env.ok === "boolean" && typeof env.schema_version === "string"
    && typeof env.tool_version === "string" && typeof env.command === "string"
}

// -------------------------------------------------------------- from the reducers

function plural(n, one, many) {
  return n + " " + (n === 1 ? one : (many || one + "s"))
}

function joinNames(names) {
  if (names.length <= 1) return names.join("")
  return names.slice(0, -1).join(", ") + " and " + names[names.length - 1]
}

function num(value) {
  return typeof value === "number" && isFinite(value) ? value : 0
}

function str(value) {
  return typeof value === "string" ? value : ""
}

function strOrNull(value) {
  return typeof value === "string" ? value : null
}

function stringList(value) {
  var out = []
  if (Array.isArray(value)) for (var i = 0; i < value.length; i++) if (typeof value[i] === "string") out.push(value[i])
  return out
}

// An RFC 3339 instant, as the CLI writes them ("2026-11-01T10:00:00.123456789Z"), or a
// date, in ms since the epoch; NaN for any other text, which never reaches Date.parse:
// Qt's V4 engine spends about a millisecond on a text it cannot parse (a slow fallback),
// which the Updates view paid for every row without a release date, 480 ms at 500 rows
// (1n2p.38). Every time the widget reads from text goes through here.
var INSTANT_PATTERN = /^\d{4}-\d{2}-\d{2}(?:[Tt]\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?(?:[Zz]|[+-]\d{2}:\d{2})?)?$/

function instantMs(iso) {
  var s = typeof iso === "string" ? iso : ""
  return INSTANT_PATTERN.test(s) ? Date.parse(s) : NaN
}

// "25 min ago", "3 hours ago", "2 days ago", "just now"; "" without a time.
function sinceText(iso, nowMs) {
  var at = instantMs(iso)
  if (isNaN(at) || typeof nowMs !== "number") return ""
  var minutes = Math.max(0, Math.floor((nowMs - at) / 60000))
  if (minutes < 1) return "just now"
  if (minutes < 60) return minutes + " min ago"
  var hours = Math.floor(minutes / 60)
  if (hours < 48) return plural(hours, "hour") + " ago"
  return plural(Math.floor(hours / 24), "day") + " ago"
}

// "checked 25 min ago".
function ageText(iso, nowMs) {
  var since = sinceText(iso, nowMs)
  return since ? "checked " + since : ""
}

// ------------------------------------------------------------ from activity (1n2p.18)

function isSafeHost(host) {
  return ["github.com", "www.wowinterface.com", "tukui.org", "www.tukui.org", "www.wowace.com"].indexOf(host) !== -1
}

// Whether a link from CLI output may open in a browser: https on a known host, no
// credentials, no port.
function isSafeUrl(url) {
  if (typeof url !== "string") return false
  var match = /^https:\/\/([^\/?#]*)([\/?#].*)?$/.exec(url)
  if (!match) return false
  var host = match[1].toLowerCase()
  if (host.indexOf("@") !== -1 || host.indexOf(":") !== -1) return false
  return isSafeHost(host)
}

// "2026-10-22 22:46" (UTC, as the CLI writes it).
// An instant as local "YYYY-MM-DD HH:MM", the zone the bar's clock shows; a text that
// does not parse, as it is.
function dateText(iso) {
  var s = str(iso)
  var ms = instantMs(s)
  if (isNaN(ms)) return s
  var at = new Date(ms)
  var two = function(n) { return n < 10 ? "0" + n : String(n) }
  return at.getFullYear() + "-" + two(at.getMonth() + 1) + "-" + two(at.getDate())
    + " " + two(at.getHours()) + ":" + two(at.getMinutes())
}

// --------------------------------------------------- from the views' states (1n2p.19)

// "18:02", local time, from an RFC 3339 instant; "" for one that does not parse.
function clockText(iso) {
  var ms = instantMs(str(iso))
  if (isNaN(ms)) return ""
  var at = new Date(ms)
  var two = function(n) { return n < 10 ? "0" + n : String(n) }
  return two(at.getHours()) + ":" + two(at.getMinutes())
}

// A source kind as a player word (design doc §6.3).
var SOURCE_WORDS = { "gh": "GitHub", "wowi": "WoWInterface", "tukui": "Tukui", "wowace": "WowAce", "git": "git", "file": "a file" }

// Whether `query` matches: each of its words is a subsequence, ignoring case, of one of
// `fields` ("plt" matches Plater, "chg disk" a row whose files changed on disk). An
// empty query matches everything.
function filterMatches(query, fields) {
  var terms = str(query).toLowerCase().split(/\s+/).filter(function(w) { return w !== "" })
  var texts = (Array.isArray(fields) ? fields : []).map(function(f) { return str(f).toLowerCase() })
  var subsequence = function(term, text) {
    var at = 0
    for (var i = 0; i < text.length && at < term.length; i++) if (text.charAt(i) === term.charAt(at)) at++
    return at === term.length
  }
  return terms.every(function(term) { return texts.some(function(text) { return subsequence(term, text) }) })
}

var NOTES_MAX_CHARS = 400
var MORE_HOSTS = { "github.com": "GitHub", "www.wowinterface.com": "WoWInterface", "tukui.org": "Tukui", "www.tukui.org": "Tukui", "www.wowace.com": "WowAce" }

// ------------------------------------------------------------ from the plan runner (G1)

// A non-empty string, else null.
function nonEmpty(value) {
  return typeof value === "string" && value !== "" ? value : null
}

// Lower case for ASCII letters only, as the CLI matches addon names.
function asciiLower(text) {
  return String(text).replace(/[A-Z]/g, function(c) { return c.toLowerCase() })
}

// What an Updates or Installed row showed, in the approval's field names.
function shownOf(row) {
  var r = row && typeof row === "object" ? row : {}
  return {
    "target_key": nonEmpty(r.targetKey), "target_id": nonEmpty(r.target), "addon": nonEmpty(r.name),
    "source": nonEmpty(r.latestSource), "channel": nonEmpty(r.channel), "release_id": nonEmpty(r.latestReleaseId),
    "asset": nonEmpty(r.latestAsset), "from_version": nonEmpty(r.version), "to_version": nonEmpty(r.latest)
  }
}

// ---------------------------------------------------------- from the plan sheet (1n2p.17)

// "WeakAuras'" and "Retail's" (design doc §2.4's "WeakAuras' settings").
function possessive(name) {
  var s = str(name)
  if (s === "") return ""
  return /s$/i.test(s) ? s + "'" : s + "'s"
}

// A client by the name a player knows it by (design doc §6.3): the `targets` entry with
// this key, else this id; else the id itself.
function sheetClient(targets, id, key) {
  var list = Array.isArray(targets) ? targets : []
  var name = function(t) { return str(t.displayName) || str(t.id) }
  for (var i = 0; key && i < list.length; i++) if (list[i] && list[i].key === key) return name(list[i])
  for (var j = 0; id && j < list.length; j++) if (list[j] && list[j].id === id) return name(list[j])
  return str(id)
}
