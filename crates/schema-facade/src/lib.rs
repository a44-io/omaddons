//! omaddons' `schemars` (omad-ho1p): schemars itself with the `real` feature, a stand-in
//! without it.
//!
//! omaddons derives `JsonSchema` on some 600 types, and its tests generate the published
//! JSON Schemas from them. The release binary only prints those schemas, which a test
//! generated and embedded (`src/contract/schemas.json.zst`), so it does not link schemars.
//! Rather than gate every derive and `#[schemars(…)]` attribute, omaddons names this
//! package under the dependency key `schemars`:
//!
//! - **`real`** re-exports schemars whole, so `schemars::JsonSchema`, `schemars::Schema`,
//!   the derive and the macros are schemars's own, and so is the code the derive emits.
//! - **Without it**, `JsonSchema` is a trait every type implements, and its derive emits
//!   nothing (`omaddons-schema-stub-derive`). The code that generates schemas, and the few
//!   hand-written `JsonSchema` impls, are behind omaddons' `schemas` feature, so nothing
//!   else of schemars is needed.

#[cfg(feature = "real")]
pub use schemars::*;

#[cfg(not(feature = "real"))]
pub use omaddons_schema_stub_derive::JsonSchema;

/// Without schemars, every type is a `JsonSchema`, so the bounds omaddons writes
/// (`Outcome::Data: JsonSchema`, `Versioned: JsonSchema`) hold and ask nothing.
#[cfg(not(feature = "real"))]
pub trait JsonSchema {}

#[cfg(not(feature = "real"))]
impl<T: ?Sized> JsonSchema for T {}
