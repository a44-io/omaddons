import QtQuick

// Omarchy's qs.Ui PointerMoveGate, as the kit has it: filters synthetic hover churn from
// rows moving under a still pointer. reset() after keyboard moves and list changes;
// moved(item, mouse) from a row's onPositionChanged says whether the pointer itself moved
// (past `threshold`, or the first sample once allowInitialSample() was called).
QtObject {
  id: root
  property Item referenceItem: null
  property real threshold: 1
  property bool primed: false
  property bool initialSampleAllowed: false
  property real lastX: 0
  property real lastY: 0

  function reset() {
    root.primed = false
    root.initialSampleAllowed = false
    root.lastX = 0
    root.lastY = 0
  }

  function allowInitialSample() {
    root.reset()
    root.initialSampleAllowed = true
  }

  function moved(item, mouse) {
    if (!item || !mouse) {
      root.reset()
      return false
    }
    var target = root.referenceItem || item
    var point = item.mapToItem(target, mouse.x, mouse.y)
    var firstSample = !root.primed
    var didMove = !firstSample
      ? Math.abs(point.x - root.lastX) > root.threshold || Math.abs(point.y - root.lastY) > root.threshold
      : root.initialSampleAllowed
    if (firstSample || didMove) {
      root.lastX = point.x
      root.lastY = point.y
    }
    root.primed = true
    root.initialSampleAllowed = false
    return didMove
  }
}
