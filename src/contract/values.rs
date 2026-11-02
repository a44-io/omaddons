//! The values an app-validated flag accepts, declared once beside its parser (M4-D2):
//! the parser reads the set, and so do `capabilities` and the semantic manifest. It
//! depends on nothing else in the crate, so the lowest modules can declare their sets.

/// What a flag clap does not validate accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueSet {
    /// Exactly these words.
    Words(&'static [&'static str]),
    /// A whole number followed by one of these units (`30m`), or an RFC 3339 time.
    DurationOrTime(&'static [DurationUnit]),
}

/// One unit of a [`ValueSet::DurationOrTime`] grammar: its suffix and its length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DurationUnit {
    pub suffix: &'static str,
    pub seconds: i64,
}
