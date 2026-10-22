import QtQml

// Quickshell.Io.Process as F0 observed it: `started` when running, `exited` then
// `running` false when the child ends, no `exited` at all for a missing binary,
// `signal(15)` ignored by a child that traps it, `signal(9)` always fatal. The tests
// drive the child through finish(), exit() and endStream().
QtObject {
  id: root

  property var command: []
  property var environment: ({})
  property bool clearEnvironment: false
  property bool running: false
  property var processId: null
  property var stdout: null
  property var stderr: null

  // Test controls and records.
  property bool ignoreTerm: false
  property var signals: []
  property int detachedStarts: 0

  property bool startPending: false

  signal started()
  signal exited(int exitCode, int exitStatus)

  // `started` arrives on a later turn of the event loop, as it does for a real child,
  // and always before anything the child does.
  onRunningChanged: {
    if (!running) return
    ProcessRegistry.launched(root)
    if (ProcessRegistry.missingBinary) {
      Qt.callLater(function() { root.running = false })
      return
    }
    startPending = true
    Qt.callLater(function() { root.ensureStarted() })
  }

  function ensureStarted() {
    if (!startPending) return
    startPending = false
    processId = ProcessRegistry.nextPid()
    started()
  }

  // A line on stderr, read as the SplitParser reads it.
  function emitStderr(line) {
    ensureStarted()
    if (stderr && typeof stderr.feedLine === "function") stderr.feedLine(String(line))
  }

  // A line on stdout (the doorbell watcher's SplitParser).
  function emitStdout(line) {
    ensureStarted()
    if (stdout && typeof stdout.feedLine === "function") stdout.feedLine(String(line))
  }

  function endStream(text) {
    ensureStarted()
    if (!stdout || typeof stdout.feedLine === "function") return
    stdout.text = String(text || "")
    stdout.streamFinished()
  }

  // The child ends: `exited`, then `running` false (F0's order).
  function exit(code, status) {
    ensureStarted()
    exited(code, status || 0)
    running = false
  }

  // A child that writes `text` and exits with `code`; "streamFirst" ends stdout before
  // the exit, as F0 saw in 17 of 17 runs, and the default reverses it.
  function finish(code, text, order) {
    if (order === "streamFirst") {
      endStream(text)
      exit(code, 0)
    } else {
      exit(code, 0)
      endStream(text)
    }
  }

  function signal(sig) {
    signals = signals.concat([sig])
    if (sig === 9 || !ignoreTerm) {
      exit(sig, 1)
      endStream("")
    }
  }

  function startDetached() {
    detachedStarts++
    ProcessRegistry.detachedStart(root)
  }

  Component.onCompleted: ProcessRegistry.add(root)
  Component.onDestruction: ProcessRegistry.remove(root)
}
