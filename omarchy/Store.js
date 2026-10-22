.pragma library

// The shared service, for bar widgets whose `bar.shell.serviceFor` has not bound yet:
// a widget completes before the shell assigns its `bar`, while the service already
// exists (F0). The service sets itself here when it completes and clears itself when
// it is destroyed, since this state survives a plugin reload and would otherwise hold
// a dead instance until the next service sets itself.

var service = null

function set(instance) {
  service = instance
}

// Clears the store only if it still holds `instance`.
function clear(instance) {
  if (service === instance) service = null
}

function get() {
  return service
}
