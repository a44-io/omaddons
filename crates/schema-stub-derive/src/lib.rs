//! `#[derive(JsonSchema)]` for a build without JSON Schemas (omad-ho1p).
//!
//! The release binary publishes schemas it embedded at build time
//! (`src/contract/schemas.json.zst`), so it needs no schema generator. The 600 derives in
//! omaddons stay as they are: `omaddons-schema-facade` exports this derive in place of
//! schemars's when its `real` feature is off. It declares the same helper attributes as
//! schemars's derive (`schemars`, `serde`, `validate`, `garde`), so every attribute the
//! types carry still compiles, and it emits no item: the facade's blanket `JsonSchema`
//! impl already covers every type.

use proc_macro::TokenStream;

/// Accepts a type schemars's `JsonSchema` derive would accept, and emits nothing.
#[proc_macro_derive(JsonSchema, attributes(schemars, serde, validate, garde))]
pub fn derive_json_schema(_item: TokenStream) -> TokenStream {
    TokenStream::new()
}
