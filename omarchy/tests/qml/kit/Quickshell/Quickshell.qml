pragma Singleton
import QtQml
import "KitEnv.js" as KitEnv

// The test stub's Quickshell singleton (../../imports/Quickshell/Quickshell.qml) for kit
// mode: every execDetached argv is recorded, never run, and env() answers what the
// snapshot run generated in KitEnv.js (HOME, so the kit finds the current theme).
QtObject {
  property var detachedCommands: []

  function execDetached(command) {
    detachedCommands = detachedCommands.concat([JSON.parse(JSON.stringify(command))])
  }

  function resetDetachedCommands() {
    detachedCommands = []
  }

  function env(name) {
    return KitEnv.ENV.hasOwnProperty(name) ? KitEnv.ENV[name] : ""
  }
}
