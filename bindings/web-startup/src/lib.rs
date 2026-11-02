//! Cold-open import plus Home rendering and its immediate read subscriptions.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn evaluate_startup(request: &str) -> String {
    den_sync::evaluate_startup(request)
}

/// The library's documents held once in this module, and whole-library lists answered from them. Transport only:
/// every method answers den-core's versioned envelope.
#[wasm_bindgen]
pub struct LibraryColumns(den_sync::LibraryColumns);

#[wasm_bindgen]
impl LibraryColumns {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> LibraryColumns {
        LibraryColumns(den_sync::LibraryColumns::new())
    }

    /// A JSON array of documents, each added or replaced.
    pub fn upsert(&mut self, documents: &str) -> String {
        self.0.upsert_text(documents)
    }

    /// `doc_decode` of one row's plaintext, its document kept here.
    pub fn decode(&mut self, plaintext: &[u8], expected: Option<String>) -> String {
        self.0.decode_text(plaintext, expected.as_deref())
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.0.remove(name)
    }

    /// One held document's JSON.
    pub fn document(&self, name: &str) -> Option<String> {
        self.0.document(name).map(|document| document.to_string())
    }

    /// The named lists (`["home", "history"]`) at `now`.
    pub fn query(&self, queries: &str, now: f64) -> String {
        self.0.query_text(queries, now as i64)
    }
}
