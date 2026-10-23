//! Cold-open import plus Home rendering and its immediate read subscriptions.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn evaluate_startup(request: &str) -> String {
    den_sync::evaluate_startup(request)
}
