//! Transport only. No sync policy is implemented in bindings.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn evaluate(request: &str) -> String {
    den_sync::evaluate(request)
}
