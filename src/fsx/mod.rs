//! The single writer. Every filesystem mutation in the program lives here: the closed
//! `Op` enum, `mutate()`, `prune()`, the write-scope table, the deny-list, renameat2 and
//! reflink primitives, and the lock files.
