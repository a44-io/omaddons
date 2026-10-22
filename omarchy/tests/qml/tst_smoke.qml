import QtQuick
import QtTest
import "../../Model.js" as Model
import "../../Build.js" as Build

// Pipeline step 4's first test: Model.js and the fixture Build.js load in Qt's own
// engine, and a few pure functions answer as they do under bun (the residual risk of
// two JavaScript engines). F2 adds the service's tests beside it.
TestCase {
  name: "Smoke"

  function test_model_loads_in_qt() {
    compare(Model.DEFAULT_SETTINGS.panelWidth, 540)
    compare(Model.setting({ "panelWidth": 900 }, "panelWidth"), 540)
    compare(Model.classifyCliArgv(["omaddons", "status", "--json"]).reason, "widget_flag")
  }

  function test_argv_builds_with_the_stamped_context() {
    var built = Model.buildArgv("update_addon", { "target": "retail", "addon": "$&-x" },
      { "binary": Build.BINARY, "label": "Update", "context": Build.CONTEXT })
    compare(built.argv[0], Build.BINARY)
    compare(built.argv[built.argv.length - 1], "$&-x")
    compare(built.env.XDG_CACHE_HOME, Build.CONTEXT.XDG_CACHE_HOME)
  }

  function test_reducers_run() {
    var env = { "ok": true, "schema_version": "1.0", "tool_version": Build.TOOL_VERSION, "command": "status",
      "data": { "summary": { "state": "updates", "updates": { "available": 2, "targets": ["retail"] } },
        "targets": [{ "id": "retail", "updates": { "available": 2 }, "issues": [] }] } }
    var model = Model.reduceStatus(env)
    compare(model.bar.badge, 2)
    compare(model.bar.tooltip[0], "2 updates on 1 client (retail 2)")
    verify(Model.compatible(env, Build))
  }
}
