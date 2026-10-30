//! A detector's budget (M4-D43). The seven failure modes `status` publishes read computed
//! facts and walk nothing, so they never charge it; E3 adds the deadline, the entry counter
//! and the bounded walker.

/// What a detector may still spend.
#[derive(Debug)]
pub struct Budget {
    _private: (),
}

impl Budget {
    /// No limit at all.
    pub fn unlimited() -> Self {
        Self { _private: () }
    }
}
