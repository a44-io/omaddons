//! What the doctor's detectors read (M4-D29), in its smallest true form: the facts
//! `status` computes. E2 adds the tolerant layers and `DoctorView::load`, whose `load`
//! must also fill `status_facts` (from its layers, as `status` computes them) so that
//! `doctor` reports the seven failure modes `status` publishes; each catalog leaf that
//! moves one of the seven onto the layers drops that row's use of the facts, and F8 removes
//! them.

use super::facts::StatusFacts;

/// One read of the machine, shared by every detector of a run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DoctorView {
    status_facts: StatusFacts,
}

impl DoctorView {
    pub fn from_status_facts(status_facts: StatusFacts) -> Self {
        Self { status_facts }
    }

    pub fn status_facts(&self) -> &StatusFacts {
        &self.status_facts
    }
}
