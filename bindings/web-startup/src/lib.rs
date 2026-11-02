//! Cold-open import plus Home rendering and its immediate read subscriptions.
use wasm_bindgen::prelude::*;

#[wasm_bindgen]
pub fn evaluate_startup(request: &str) -> String {
    den_sync::evaluate_startup(request)
}

/// The library's documents held once in this module, and whole-library lists answered from them. Transport only: the
/// lists are den-core's (`LibraryColumns`). A refusal throws an `Error` whose message is den-core's code.
#[wasm_bindgen]
pub struct LibraryColumns(den_sync::LibraryColumns);

/// One list query's answer: named typed columns, `<list>.<column>`.
#[wasm_bindgen]
pub struct ColumnsAnswer(den_sync::ColumnsAnswer);

fn refused(code: String) -> JsError {
    JsError::new(&code)
}

#[wasm_bindgen]
impl ColumnsAnswer {
    pub fn names(&self) -> Vec<String> {
        self.0.names()
    }

    /// A numeric column (`NaN` for none) as a `Float64Array`.
    pub fn numbers(&self, name: &str) -> Option<Vec<f64>> {
        self.0.numbers(name).map(<[f64]>::to_vec)
    }

    /// A text column (`""` for none).
    pub fn strings(&self, name: &str) -> Option<Vec<String>> {
        self.0.strings(name).map(<[String]>::to_vec)
    }
}

#[wasm_bindgen]
impl LibraryColumns {
    #[wasm_bindgen(constructor)]
    #[allow(clippy::new_without_default)]
    pub fn new() -> LibraryColumns {
        LibraryColumns(den_sync::LibraryColumns::new())
    }

    /// A JSON array of documents, each added or replaced: the number held. A document that cannot be read refuses the
    /// batch.
    pub fn upsert(&mut self, documents: &str) -> Result<u32, JsError> {
        self.0
            .upsert_text(documents)
            .map(|n| n as u32)
            .map_err(refused)
    }

    /// `doc_decode` of one row's plaintext, its document kept here: the decode's envelope without the document.
    pub fn decode(&mut self, plaintext: &[u8], expected: Option<String>) -> String {
        let decoded = self.0.decode(plaintext, expected.as_deref());
        format!(r#"{{"ok":{decoded},"version":1}}"#)
    }

    pub fn remove(&mut self, name: &str) -> bool {
        self.0.remove(name)
    }

    /// One held document's JSON, as it was added.
    pub fn document(&self, name: &str) -> Option<String> {
        self.0.document(name).map(str::to_owned)
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u32 {
        self.0.len() as u32
    }

    pub fn home(&self, now: f64) -> ColumnsAnswer {
        ColumnsAnswer(self.0.home(now as i64))
    }

    pub fn history(&self, now: f64) -> ColumnsAnswer {
        ColumnsAnswer(self.0.history(now as i64))
    }

    pub fn watchlist(&self, now: f64) -> ColumnsAnswer {
        ColumnsAnswer(self.0.watchlist(now as i64))
    }

    /// Continue Watching with `layouts` (JSON, by TMDB id) and `airing` (JSON, or none).
    pub fn continuing(
        &self,
        now: f64,
        layouts: &str,
        airing: Option<String>,
    ) -> Result<ColumnsAnswer, JsError> {
        let layouts = den_sync::ColumnsLayouts::read_text(layouts).map_err(refused)?;
        let airing = airing
            .as_deref()
            .map(den_sync::ColumnsAiring::read_text)
            .transpose()
            .map_err(refused)?;
        Ok(ColumnsAnswer(self.0.continuing(
            now as i64,
            &layouts,
            airing.as_ref(),
        )))
    }

    /// The newest stamp and when an answer next changes with no write.
    pub fn newest(&self, now: f64) -> ColumnsAnswer {
        ColumnsAnswer(self.0.newest(now as i64))
    }

    /// Every held document's JSON, as one array: what the assistant and deferred modules read documents from.
    pub fn documents(&self) -> String {
        self.0.documents_text()
    }
}
