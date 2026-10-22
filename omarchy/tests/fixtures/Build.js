.pragma library

// A hand-written stand-in for the Build.js that `omaddons omarchy install` renders (E1),
// for the QML tests: the pipeline copies it into a copy of the plugin. Its id is not
// the widget's, so the widget's id stays a literal in manifest.json alone.

var BINARY = "/opt/omaddons-fixture/omaddons"
var TOOL_VERSION = "0.0.0-fixture"
var PLUGIN_ID = "a44.omaddons-fixture"
var CONTEXT = {
  "HOME": "/home/fixture",
  "XDG_CONFIG_HOME": "/home/fixture/.config",
  "XDG_STATE_HOME": "/home/fixture/.local/state",
  "XDG_CACHE_HOME": "/home/fixture/.cache",
  "XDG_DATA_HOME": "/home/fixture/.local/share"
}
var CACHE_DIR = "/home/fixture/.cache/omaddons"
var CONTENT_SHA256 = "0000000000000000000000000000000000000000000000000000000000000000"
