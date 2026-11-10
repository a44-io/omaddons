//! The production fixers' plans (F-leaves), each the `plan` of its failure modes'
//! `FixerSpec` in the registry. A plan reads the doctor's view and never writes: E5
//! lowers what it returns, E6 runs it.

pub mod chmod;
pub mod quarantine;
pub mod recover;
pub mod retention;
pub mod set_aside;
