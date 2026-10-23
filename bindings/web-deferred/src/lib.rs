//! Deferred actions, outbound delivery, maintenance, and administrative policy.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn evaluate_deferred(request: &str) -> String {
    den_sync::evaluate_deferred(request)
}
