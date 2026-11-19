.pragma library

// A hand-written stand-in for the Build.js that `omaddons omarchy install` renders (E1),
// for the QML tests: the pipeline copies it into a copy of the plugin. It holds only
// what the widget still reads from Build.js, its build's version and content hash
// (omad-widget-plugin-3bcbh.4 retires both); the widget finds its binary, environment
// and doorbell itself, and its id is Model.PLUGIN_ID (omad-widget-plugin-3bcbh.3).

var TOOL_VERSION = "0.0.0-fixture"
var CONTENT_SHA256 = "0000000000000000000000000000000000000000000000000000000000000000"
