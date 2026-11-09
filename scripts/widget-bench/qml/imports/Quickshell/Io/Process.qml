import QtQml
import "BenchEnv.js" as BenchEnv

// The bench's Quickshell.Io.Process (scripts/widget-bench.sh lays it over the test
// stubs for tst_bench_open.qml only): a run of the widget's binary is posted to the
// bench's runner (runner.js), which runs the real binary and answers with its exit code,
// stdout and stderr lines. The order matches the stub's and the real Process's: `started`
// on a later turn, stderr lines, the end of stdout, then `exited` and `running` false.
// Any other command (the doorbell's inotifywait) starts and stays quiet, as a watcher
// that never fires.
QtObject {
  id: root

  property var command: []
  property var environment: ({})
  property bool clearEnvironment: false
  property bool running: false
  property var processId: null
  property var stdout: null
  property var stderr: null
  property var signals: []

  signal started()
  signal exited(int exitCode, int exitStatus)

  property bool forwarded: false
  property bool done: false

  onRunningChanged: {
    if (!running) return
    ProcessRegistry.launched(root)
    Qt.callLater(function() {
      root.processId = ProcessRegistry.nextPid()
      root.started()
      var argv = root.command || []
      if (argv.length > 0 && String(argv[0]) === BenchEnv.BINARY) root.forward(argv)
    })
  }

  function forward(argv) {
    forwarded = true
    var xhr = new XMLHttpRequest()
    xhr.onreadystatechange = function() {
      if (xhr.readyState !== XMLHttpRequest.DONE || root.done) return
      var reply = null
      try {
        reply = JSON.parse(xhr.responseText)
      } catch (e) {
        reply = { "code": 125, "stdout": "", "stderr": ["widget-bench: the runner did not answer: " + e] }
      }
      root.finish(reply)
    }
    xhr.open("POST", BenchEnv.RUNNER_URL + "/run")
    xhr.setRequestHeader("Content-Type", "application/json")
    xhr.send(JSON.stringify({ "command": argv, "environment": root.environment }))
  }

  function finish(reply) {
    done = true
    var lines = reply.stderr || []
    for (var i = 0; i < lines.length; i++) if (stderr && typeof stderr.feedLine === "function") stderr.feedLine(String(lines[i]))
    if (stdout && typeof stdout.feedLine !== "function") {
      stdout.text = String(reply.stdout || "")
      stdout.streamFinished()
    }
    exited(reply.code, 0)
    running = false
  }

  // The bench never times a child out; a signal ends a quiet helper at once.
  function signal(sig) {
    signals = signals.concat([sig])
    if (!forwarded && !done) finish({ "code": sig, "stdout": "", "stderr": [] })
  }

  function startDetached() {
    ProcessRegistry.detachedStart(root)
  }

  Component.onCompleted: ProcessRegistry.add(root)
  Component.onDestruction: ProcessRegistry.remove(root)
}
