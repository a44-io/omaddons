pragma Singleton
import QtQuick

// Omarchy's Style at scale 1: space(px) is px, and the font tokens' defaults.
QtObject {
  function space(px) { return px }

  readonly property QtObject font: QtObject {
    readonly property string family: "monospace"
    readonly property int caption: 10
    readonly property int bodySmall: 11
    readonly property int body: 12
    readonly property int title: 14
  }
}
