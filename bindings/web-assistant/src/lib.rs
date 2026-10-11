//! Transport only. The assistant ops (den-spec `wire/assistant-v1.md`) in a module of their own, so the web loads their
//! crypto only where it is used.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn evaluate_assistant(request: &str) -> String {
    den_sync::evaluate_assistant(request)
}
