import QtQml

// Quickshell's Singleton base type, for the shell kit's own singletons in kit mode: an
// object that holds children (an IpcHandler, a Timer).
QtObject {
  default property list<QtObject> data
}
