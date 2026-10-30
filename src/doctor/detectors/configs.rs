//! `configs`: the configuration omaddons reads.

use crate::doctor::budget::Budget;
use crate::doctor::finding::Detected;
use crate::doctor::registry::ids;
use crate::doctor::selection::Selection;
use crate::doctor::view::DoctorView;

/// `config-parse-error`: `config.toml` could not be read, as the view's config layer
/// recorded it.
pub(crate) fn config_parse_error(view: &DoctorView, _: &Selection, _: &mut Budget) -> Detected {
    view.config();
    Detected::complete(view.failure_observations(ids::CONFIG_PARSE_ERROR))
}
