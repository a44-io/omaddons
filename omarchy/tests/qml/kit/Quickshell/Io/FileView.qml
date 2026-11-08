import QtQml

// Quickshell.Io.FileView for kit mode: reads `path` once, on the next turn of the event
// loop, through XMLHttpRequest (the run sets QML_XHR_ALLOW_FILE_READ=1), then emits
// `loaded` or `loadFailed`. Never writes and never watches.
QtObject {
  id: root

  property string path: ""
  property bool watchChanges: false
  property bool printErrors: true
  property bool blockLoading: false
  property string content: ""

  signal loaded()
  signal loadFailed(int error)
  signal fileChanged()

  function text() { return content }
  function reload() { Qt.callLater(read) }

  function read() {
    if (path === "") {
      loadFailed(1)
      return
    }
    var xhr = new XMLHttpRequest()
    xhr.open("GET", "file://" + path, false)
    try {
      xhr.send()
    } catch (e) {
      loadFailed(1)
      return
    }
    if (xhr.responseText === "" || xhr.responseText === null) {
      loadFailed(1)
      return
    }
    content = xhr.responseText
    loaded()
  }

  onPathChanged: reload()
  Component.onCompleted: reload()
}
