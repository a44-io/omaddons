pragma Singleton
import QtQml

// Quickshell.Hyprland's singleton: the tests emit its raw events, shaped as Quickshell's
// HyprlandEvent (`name`, `data`).
QtObject {
  signal rawEvent(var event)

  function emitRaw(name, data) {
    rawEvent({ "name": name, "data": data })
  }
}
