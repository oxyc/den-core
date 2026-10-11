//! Transport only. No sync policy is implemented in bindings.
use wasm_bindgen::prelude::*;

/// Every op but the assistant's, which `den-core-web-assistant` carries in a module of its own.
#[wasm_bindgen]
pub fn evaluate(request: &str) -> String {
    den_sync::evaluate_core(request)
}
