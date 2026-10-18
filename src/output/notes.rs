//! The run's notes: what a command found on its way that the user must hear about
//! whether or not it then succeeds (vision §5.9: a failure is exactly when an agent most
//! needs a `journal_pending` or a `config_unknown_key`).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::error::{CommandHint, Warning};

/// Warnings and command hints found during one run, shared by every clone. Commands add
/// them as soon as they are found (`World::load` does for its config, registry,
/// discovery, journal and credential warnings), so an error returned later keeps them:
/// [`super::emit`] puts the warnings in the envelope and on stderr either way. A warning
/// already held is not added twice.
#[derive(Debug, Clone, Default)]
pub struct RunNotes {
    inner: Arc<Mutex<Notes>>,
}

#[derive(Debug, Default)]
struct Notes {
    warnings: Vec<Warning>,
    commands: Vec<CommandHint>,
}

impl RunNotes {
    pub fn warn(&self, warning: Warning) {
        self.warn_all(std::slice::from_ref(&warning));
    }

    pub fn warn_all(&self, warnings: &[Warning]) {
        let mut notes = self.lock();
        for warning in warnings {
            if !notes.warnings.contains(warning) {
                notes.warnings.push(warning.clone());
            }
        }
    }

    /// A next step that holds even if the command then fails (the undo of a commit that
    /// stands).
    pub fn hint(&self, command: CommandHint) {
        self.lock().commands.push(command);
    }

    /// The warnings and hints so far, in the order they were found.
    pub fn snapshot(&self) -> (Vec<Warning>, Vec<CommandHint>) {
        let notes = self.lock();
        (notes.warnings.clone(), notes.commands.clone())
    }

    /// A poisoned lock is used as is: notes are plain data, valid after any panic.
    fn lock(&self) -> MutexGuard<'_, Notes> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }
}
