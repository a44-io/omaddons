pragma Singleton
import QtQml

// Every mock Process alive, every launch (command and environment as they were, since
// the service destroys a process after it completes), and every detached start.
QtObject {
  property var processes: []
  property var launches: []
  property var detached: []
  property bool missingBinary: false
  property int pid: 1000

  function reset() {
    launches = []
    detached = []
    missingBinary = false
  }

  function add(process) {
    processes = processes.concat([process])
  }

  function remove(process) {
    var next = []
    for (var i = 0; i < processes.length; i++) if (processes[i] !== process) next.push(processes[i])
    processes = next
  }

  function copy(value) {
    return value === undefined ? undefined : JSON.parse(JSON.stringify(value))
  }

  function launched(process) {
    launches = launches.concat([{ "process": process, "command": copy(process.command), "environment": copy(process.environment),
      "stdinEnabled": process.stdinEnabled === true }])
  }

  function detachedStart(process) {
    detached = detached.concat([{ "command": copy(process.command), "environment": copy(process.environment) }])
  }

  function nextPid() {
    pid++
    return pid
  }
}
