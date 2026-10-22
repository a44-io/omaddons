pragma Singleton
import QtQml

// Quickshell's singleton: every execDetached argv is recorded, never run.
QtObject {
  property var detachedCommands: []

  function execDetached(command) {
    detachedCommands = detachedCommands.concat([JSON.parse(JSON.stringify(command))])
  }

  function resetDetachedCommands() {
    detachedCommands = []
  }

  function env(name) {
    return ""
  }
}
