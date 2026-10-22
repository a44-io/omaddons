pragma Singleton
import QtQuick

// Omarchy's Border specs, as plain objects.
QtObject {
  function none() { return {} }
  function flat(color, width) { return { "color": color, "width": width } }
  function controlSpec(state, foreground, accent) { return { "state": state } }
}
