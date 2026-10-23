//! The run's notes: what a command found on its way that the user must hear about
//! whether or not it then succeeds (vision §5.9: a failure is exactly when an agent most
//! needs a `journal_pending` or a `config_unknown_key`).

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::error::{CommandHint, Suggestion, Warning, WarningCode};

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
    /// The journal warnings found at the start of the run, and how to read them again.
    journal: Option<(Vec<Warning>, Refresh)>,
    /// The same command online, when `--offline` alone kept the run offline.
    online_rerun: Option<Vec<String>>,
}

/// Reads the journal warnings again; `None` when they cannot be read.
#[derive(Clone)]
struct Refresh(Arc<dyn Fn() -> Option<Vec<Warning>> + Send + Sync>);

impl std::fmt::Debug for Refresh {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Refresh")
    }
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

    /// The journal warnings found at the start of the run (`journal_pending`,
    /// `tx_in_progress`), added like any other, and how to read them again when it
    /// ends: a command that recovered a journal on its way no longer reports it as
    /// pending (vision §5.5).
    pub fn journal_found(
        &self,
        found: &[Warning],
        refresh: impl Fn() -> Option<Vec<Warning>> + Send + Sync + 'static,
    ) {
        self.warn_all(found);
        let mut notes = self.lock();
        let mut all = notes.journal.take().map(|(all, _)| all).unwrap_or_default();
        for warning in found {
            if !all.contains(warning) {
                all.push(warning.clone());
            }
        }
        notes.journal = Some((all, Refresh(Arc::new(refresh))));
    }

    /// Brings the journal warnings in `warnings` up to date once the run is over: one
    /// found at its start that no longer holds is dropped, one that holds now and is not
    /// there is added, and the rest keep their place. Nothing changes when the journal
    /// cannot be read again.
    pub fn settle_journal(&self, warnings: &mut Vec<Warning>) {
        let Some((found, refresh)) = self.lock().journal.clone() else {
            return;
        };
        let Some(now) = (refresh.0)() else {
            return;
        };
        warnings.retain(|w| !found.contains(w) || now.contains(w));
        for warning in now {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
    }

    /// The command to rerun online (`cli::online_rerun`): a `stale_check` warning's fix.
    pub fn online_rerun(&self, argv: Option<Vec<String>>) {
        self.lock().online_rerun = argv;
    }

    /// Gives each `stale_check` in `warnings` the online rerun as its fix, when there is
    /// one: an answer from stored data is refreshed by the same command, online.
    pub fn rerun_stale(&self, warnings: &mut [Warning]) {
        let Some(argv) = self.lock().online_rerun.clone() else {
            return;
        };
        for warning in warnings
            .iter_mut()
            .filter(|w| matches!(w.code, WarningCode::StaleCheck))
        {
            warning.fix = Some(Suggestion::command(argv.clone()));
        }
    }

    /// A next step that holds even if the command then fails (the undo of a commit that
    /// stands), once: a commit rolled forward in process notes its undo again.
    pub fn hint(&self, command: CommandHint) {
        let mut notes = self.lock();
        if !notes.commands.contains(&command) {
            notes.commands.push(command);
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::WarningCode;

    fn pending(tx: &str) -> Warning {
        Warning::new(WarningCode::JournalPending, format!("transaction {tx}"))
    }

    #[test]
    fn journal_warnings_describe_the_end_of_the_run() {
        let notes = RunNotes::default();
        let other = Warning::new(WarningCode::ConfigUnknownKey, "k");
        notes.warn(other.clone());
        let now = Arc::new(Mutex::new(Some(vec![pending("b"), pending("c")])));
        let read = Arc::clone(&now);
        notes.journal_found(&[pending("a"), pending("b")], move || {
            read.lock().unwrap().clone()
        });
        let recovered = Warning::new(WarningCode::JournalRecovered, "transaction a");
        let mut warnings = notes.snapshot().0;
        warnings.push(recovered.clone());
        notes.settle_journal(&mut warnings);
        // `a` was recovered, `b` still waits in its place, `c` appeared since.
        assert_eq!(
            warnings,
            [other.clone(), pending("b"), recovered.clone(), pending("c")]
        );

        // Unreadable at the end: left as found.
        *now.lock().unwrap() = None;
        let mut warnings = notes.snapshot().0;
        notes.settle_journal(&mut warnings);
        assert_eq!(warnings, [other.clone(), pending("a"), pending("b")]);

        // No journal warnings at the start: nothing to settle.
        let quiet = RunNotes::default();
        let mut warnings = vec![other.clone()];
        quiet.settle_journal(&mut warnings);
        assert_eq!(warnings, [other]);
    }
}
