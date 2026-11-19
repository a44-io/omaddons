import QtQuick
import QtTest
import "../../Model.js" as Model
import "Versions.js" as Versions

// Pipeline step 4's first test: Model.js loads in Qt's own engine, and a few pure
// functions answer as they do under bun (the residual risk of two JavaScript engines).
// F2 adds the service's tests beside it.
TestCase {
  name: "Smoke"

  function test_model_loads_in_qt() {
    compare(Model.DEFAULT_SETTINGS.panelWidth, 600)
    compare(Model.setting({ "panelWidth": 900 }, "panelWidth"), 600)
    compare(Model.classifyCliArgv(["omaddons", "status", "--json"]).reason, "widget_flag")
  }

  function test_argv_builds_with_no_environment_of_its_own() {
    // An update runs as its preview (M4-D28), `omaddons` through PATH, and its child
    // inherits the session's environment (omad-widget-plugin-3bcbh.3).
    var built = Model.buildArgv("update_addon", { "target": "retail", "addon": "$&-x" },
      { "binary": "omaddons", "label": "Update", "preview": true })
    compare(built.argv[0], "omaddons")
    compare(built.argv[built.argv.length - 1], "$&-x")
    compare(Object.keys(built), ["argv"])
    compare(Model.sessionContext(function(name) { return name === "HOME" ? "/home/q" : null }).XDG_CACHE_HOME, "/home/q/.cache")
  }

  function test_reducers_run() {
    var env = { "ok": true, "schema_version": "1.2", "tool_version": Versions.TOOL_VERSION, "command": "status",
      "data": { "summary": { "state": "updates", "updates": { "available": 2, "targets": ["retail"] } },
        "targets": [{ "id": "retail", "updates": { "available": 2 }, "issues": [] }] } }
    var model = Model.reduceStatus(env)
    compare(model.summary.updates.available, 2)
    var bar = Model.barState(model, { "phase": "ok" })
    compare(bar.badge, 2)
    compare(bar.tooltip[0], "2 updates")
    compare(bar.tooltip[1], "retail · 2 updates")
    // Any omaddons version in a contract the panel reads (omad-widget-plugin-3bcbh.4).
    verify(Model.compatible(env, Model.CONTRACT_MIN))
    env.schema_version = "1.1"
    verify(!Model.compatible(env, Model.CONTRACT_MIN))
    compare(Model.contractMin({ "omaddons": { "contract_min": "1.0" } }), Model.CONTRACT_MIN)
  }
}
