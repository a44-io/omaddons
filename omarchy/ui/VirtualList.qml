import QtQuick
import QtQuick.Controls

// A long list (more than 50 rows; design doc §5.5): a ListView that builds only the rows
// in view and reuses them, and keeps the cursor's row in view as j/k move it. The frame
// asks rowItem(i), which is null for a row not built yet: it scrolls through
// `positionCursor()` here, never by mapping a delegate that may not exist.
ListView {
  id: root

  // The row the frame's cursor is on; -1 is none.
  property int cursor: -1
  // The rows built over the list's life: the delegate-reuse test reads it.
  property int built: 0

  clip: true
  reuseItems: true
  cacheBuffer: 0
  boundsBehavior: Flickable.StopAtBounds
  ScrollBar.vertical: ScrollBar { policy: ScrollBar.AsNeeded }

  Accessible.role: Accessible.List

  function rowItem(i) {
    return i >= 0 && i < count ? itemAtIndex(i) : null
  }

  // The cursor's row brought into view, the least scroll that shows it whole.
  function positionCursor() {
    if (cursor < 0 || cursor >= count) return
    positionViewAtIndex(cursor, ListView.Contain)
  }

  onCursorChanged: positionCursor()
  onCountChanged: if (cursor >= count) cursor = count - 1
}
