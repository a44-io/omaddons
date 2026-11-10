pragma Singleton
import QtQuick

// Omarchy's Style at scale 1: space(px) is px, and the spacing and font tokens'
// defaults; motion on (duration(ms) is ms unless reduceMotion).
QtObject {
  function space(px) { return px }

  readonly property int cornerRadius: 0
  property bool reduceMotion: false

  function duration(ms) { return reduceMotion ? 0 : ms }

  readonly property QtObject spacing: QtObject {
    readonly property int xs: 3
    readonly property int sm: 4
    readonly property int md: 6
    readonly property int lg: 8
    readonly property int xl: 10
    readonly property int xxl: 12
    readonly property int controlPaddingX: 10
    readonly property int controlPaddingY: 6
    // The kit's semantic tokens at their defaults (a theme's shell.toml can pin them).
    readonly property int rowGap: 8
    readonly property int rowPaddingX: 12
    readonly property int labelGap: 4
    readonly property int popupPadding: 14
  }

  readonly property QtObject font: QtObject {
    readonly property string family: "monospace"
    readonly property int caption: 10
    readonly property int bodySmall: 11
    readonly property int body: 12
    readonly property int subtitle: 13
    readonly property int title: 14
    readonly property int heading: 16
    readonly property int display: 24
    readonly property int icon: 14
  }
}
