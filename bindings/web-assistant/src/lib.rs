//! Transport only. Assistant and playback-receipt crypto in a module of their own, so the web loads it only where it
//! is used.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn evaluate_assistant(request: &str) -> String {
    den_sync::evaluate_assistant(request)
}
