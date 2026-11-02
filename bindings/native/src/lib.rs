//! Transport only. No sync policy is implemented in bindings.
uniffi::setup_scaffolding!();

#[uniffi::export]
pub fn evaluate(request: String) -> String {
    den_sync::evaluate(&request)
}

use std::sync::{Arc, Mutex, MutexGuard};

/// A refusal by den-core: its code (`invalid_request`, `invalid_layout`, …).
#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum ColumnsError {
    Refused(String),
}

impl std::fmt::Display for ColumnsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ColumnsError::Refused(code) = self;
        f.write_str(code)
    }
}

/// One list query's answer: named typed columns, `<list>.<column>`.
#[derive(uniffi::Object)]
pub struct ColumnsAnswer(den_sync::ColumnsAnswer);

#[uniffi::export]
impl ColumnsAnswer {
    pub fn names(&self) -> Vec<String> {
        self.0.names()
    }

    /// A numeric column (`NaN` for none).
    pub fn numbers(&self, name: String) -> Option<Vec<f64>> {
        self.0.numbers(&name).map(<[f64]>::to_vec)
    }

    /// A text column (`""` for none).
    pub fn strings(&self, name: String) -> Option<Vec<String>> {
        self.0.strings(&name).map(<[String]>::to_vec)
    }
}

/// The library's documents held once in den-core, and whole-library lists answered from them. Transport only: the
/// lists are den-core's (`LibraryColumns`).
#[derive(uniffi::Object)]
pub struct LibraryColumns(Mutex<den_sync::LibraryColumns>);

impl LibraryColumns {
    fn held(&self) -> MutexGuard<'_, den_sync::LibraryColumns> {
        // The index changes only by whole-document writes after every document of a batch is read, so a lock a panic
        // poisoned still holds a consistent index.
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

fn answer(answer: den_sync::ColumnsAnswer) -> Arc<ColumnsAnswer> {
    Arc::new(ColumnsAnswer(answer))
}

#[uniffi::export]
impl LibraryColumns {
    #[uniffi::constructor]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(den_sync::LibraryColumns::new())))
    }

    /// A JSON array of documents, each added or replaced: the number held. A document that cannot be read refuses the
    /// batch.
    pub fn upsert(&self, documents: String) -> Result<u64, ColumnsError> {
        self.held()
            .upsert_text(&documents)
            .map(|n| n as u64)
            .map_err(ColumnsError::Refused)
    }

    /// `doc_decode` of one row's plaintext, its document kept here: the decode's envelope without the document.
    pub fn decode(&self, plaintext: Vec<u8>, expected: Option<String>) -> String {
        let decoded = self.held().decode(&plaintext, expected.as_deref());
        format!(r#"{{"ok":{decoded},"version":1}}"#)
    }

    pub fn remove(&self, name: String) -> bool {
        self.held().remove(&name)
    }

    /// One held document's JSON, as it was added.
    pub fn document(&self, name: String) -> Option<String> {
        self.held().document(&name).map(str::to_owned)
    }

    #[allow(clippy::len_without_is_empty)]
    pub fn len(&self) -> u64 {
        self.held().len() as u64
    }

    /// Every held document's JSON, as one array.
    pub fn documents(&self) -> String {
        self.held().documents_text()
    }

    pub fn home(&self, now: i64) -> Arc<ColumnsAnswer> {
        answer(self.held().home(now))
    }

    pub fn history(&self, now: i64) -> Arc<ColumnsAnswer> {
        answer(self.held().history(now))
    }

    pub fn watchlist(&self, now: i64) -> Arc<ColumnsAnswer> {
        answer(self.held().watchlist(now))
    }

    /// Continue Watching with `layouts` (JSON, by TMDB id) and `airing` (JSON, or none).
    pub fn continuing(
        &self,
        now: i64,
        layouts: String,
        airing: Option<String>,
    ) -> Result<Arc<ColumnsAnswer>, ColumnsError> {
        let layouts =
            den_sync::ColumnsLayouts::read_text(&layouts).map_err(ColumnsError::Refused)?;
        let airing = airing
            .as_deref()
            .map(den_sync::ColumnsAiring::read_text)
            .transpose()
            .map_err(ColumnsError::Refused)?;
        Ok(answer(self.held().continuing(
            now,
            &layouts,
            airing.as_ref(),
        )))
    }

    /// The newest stamp and when an answer next changes with no write.
    pub fn newest(&self, now: i64) -> Arc<ColumnsAnswer> {
        answer(self.held().newest(now))
    }

    /// `assistant_projection`'s `lists` for `layouts` (JSON, by TMDB id), as JSON to pass on.
    pub fn assistant(&self, now: i64, layouts: String) -> Result<String, ColumnsError> {
        let layouts =
            den_sync::ColumnsLayouts::read_text(&layouts).map_err(ColumnsError::Refused)?;
        Ok(self.held().assistant(now, &layouts).to_string())
    }

    /// `pending_targets_v4` over the held documents for one account's `deliver` facts (JSON): its answer's JSON.
    pub fn pending(&self, deliver: String, now: i64) -> Result<String, ColumnsError> {
        self.held()
            .pending_text(&deliver, now)
            .map_err(ColumnsError::Refused)
    }
}

#[cfg(test)]
mod tests {
    use super::evaluate;
    use serde_json::{json, Value};

    #[test]
    fn season_projection_crosses_the_native_binding() {
        let season = json!({
            "format": 4,
            "kind": "season",
            "title": {"type": "tv", "id": 1399},
            "season": 1,
            "seasonReset": null,
            "episodes": {
                "1": {"progress": {"value": 1, "at": [1000, 0, "a1b2c3d4e5f60718"], "viewing": 0},
                    "imported": false, "plays": {"0": 1000}, "cleared": null},
                "2": {}
            }
        });
        let batch: Value = serde_json::from_str(&evaluate(
            json!({"op": "season_state_v4", "season": season, "now": 2000}).to_string(),
        ))
        .unwrap();
        for episode in [1, 2] {
            let single: Value = serde_json::from_str(&evaluate(
                json!({"op": "episode_state_v4", "season": season, "episode": episode, "now": 2000})
                    .to_string(),
            ))
            .unwrap();
            assert_eq!(batch["ok"][episode.to_string()], single["ok"]);
        }
    }

    #[test]
    fn shared_profile_contract_crosses_the_native_binding() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../crates/den-sync/tests/fixtures/profiles-v1.json"
        ))
        .unwrap();
        for case in fixture["cases"].as_array().unwrap() {
            let response: Value =
                serde_json::from_str(&evaluate(case["request"].to_string())).unwrap();
            let expected = if let Some(error) = case.get("error") {
                json!({"version": 1, "error": error})
            } else {
                json!({"version": 1, "ok": case["ok"]})
            };
            assert_eq!(response, expected, "{}", case["name"]);
        }

        // The generic native transport must expose the two Kid-creation operations
        // without learning any profile policy of its own.
        let materialize: Value = serde_json::from_str(&evaluate(
            json!({"op":"profiles_materialize", "document":fixture["migrationBase"]}).to_string(),
        ))
        .unwrap();
        assert_eq!(materialize["ok"]["directory"], fixture["migrationBase"]);
        assert!(materialize["ok"]["jcs"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        assert!(materialize["ok"]["plaintext"]
            .as_str()
            .is_some_and(|value| !value.is_empty()));

        let journal = json!({
            "version":1,
            "profile":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "key":"VVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVVU=",
            "display":{"value":{"name":"Sam","avatar":"fox"},
                       "at":[1800000005000_u64,0,"a1b2c3d4e5f60718"]},
            "libraryAt":[1800000005000_u64,1,"a1b2c3d4e5f60718"],
            "maximumAge":8,
            "policyAt":[1800000005000_u64,2,"a1b2c3d4e5f60718"],
            "deletedAt":[1800000005000_u64,3,"a1b2c3d4e5f60718"]
        });
        for (input, effect) in [
            (json!({"state":"idle", "explicitIntent":false}), "no-op"),
            (
                json!({"state":"idle", "explicitIntent":true, "journal":journal}),
                "persist-journal",
            ),
        ] {
            let response: Value = serde_json::from_str(&evaluate(
                json!({"op":"profiles_create_next", "input":input}).to_string(),
            ))
            .unwrap();
            assert_eq!(response["ok"]["effect"], effect);
        }

        let depth = 1_500;
        let mut wire = String::from(
            r#"{"op":"profiles_materialize","document":{"migration":{},"futureNested":"#,
        );
        wire.push_str(&r#"{"next":"#.repeat(depth));
        wire.push_str("null");
        wire.push_str(&"}".repeat(depth));
        wire.push_str("}}");
        let response: Value = serde_json::from_str(&evaluate(wire)).unwrap();
        assert_eq!(response, json!({"version":1, "error":"invalid_request"}));
    }
}
