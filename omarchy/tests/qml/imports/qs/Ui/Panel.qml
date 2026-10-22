import QtQuick

// Omarchy's qs.Ui Panel as the bar widget uses it: the injected bar, settings and
// module name, and the open state behind open(), close() and toggle(). No IPC.
Item {
  property QtObject bar: null
  property string moduleName: ""
  property var settings: ({})
  property string ipcTarget: ""
  property bool manageIpc: true
  property bool opened: false
  // The directions switchPanel was asked for (Tab and Shift-Tab).
  property var switched: []

  function open() { opened = true }
  function close() { opened = false }
  function toggle() { opened ? close() : open() }
  function switchPanel(direction) {
    switched = switched.concat([direction])
    return true
  }

  function setting(name, fallback) {
    var value = settings ? settings[name] : undefined
    return value === undefined || value === null ? fallback : value
  }
}
