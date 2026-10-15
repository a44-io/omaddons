//! The single writer. Every filesystem mutation in the program lives here: the closed
//! `Op` enum, `mutate()`, `prune()`, the write-scope table, the deny-list, renameat2 and
//! reflink primitives, and the lock files.

// Until `mutate()` (fsx-mutate bead) calls them, only tests use the primitives. Once
// everything is used, the expectation fails and this attribute must go.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the primitives are wired up by the fsx-mutate bead"
    )
)]
pub(crate) mod failpoint;
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "the primitives are wired up by the fsx-mutate bead"
    )
)]
pub(crate) mod sys;
