pragma Singleton
import QtQml

// Quickshell's singleton: every execDetached argv is recorded, never run, and env()
// answers a fixture session: its home, whose XDG directories are the defaults below it
// (Model.sessionContext). A test may set `environment` to another session.
QtObject {
  property var detachedCommands: []
  property var environment: ({ "HOME": "/home/fixture" })

  function execDetached(command) {
    detachedCommands = detachedCommands.concat([JSON.parse(JSON.stringify(command))])
  }

  function resetDetachedCommands() {
    detachedCommands = []
  }

  // As Quickshell's: a variable's value, or null when it is not set.
  function env(name) {
    return environment.hasOwnProperty(name) ? environment[name] : null
  }
}
