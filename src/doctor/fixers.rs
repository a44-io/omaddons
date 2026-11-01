//! The production fixers' plans (F-leaves), each the `plan` of its failure modes'
//! `FixerSpec` in the registry. A plan reads the doctor's view and never writes: E5
//! lowers what it returns, E6 runs it.

pub mod recover;
