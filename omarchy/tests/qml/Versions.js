.pragma library

// The omaddons version the QML tests' envelopes carry: their own, and the binary's,
// which qml_fixtures.js restamps with it so a render reads the same at every release.
// Any version is the widget's (omad-widget-plugin-3bcbh.4): it reads every contract from
// its manifest's minimum, and has no Build.js to compare a version with.

var TOOL_VERSION = "0.0.0-fixture"
