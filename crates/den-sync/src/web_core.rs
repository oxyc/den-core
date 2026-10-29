//! Compact, single-parse request boundary shared by browser and native cores.
//!
//! Keeping one field reader here avoids generating a separate serde visitor for every request variant. That matters
//! most after wasm2js expands the browser core to JavaScript; the typed boundary remains only as a test oracle.

use super::*;
use serde::de::{DeserializeOwned, MapAccess, Visitor};
use serde_json::Map;
use std::fmt;

const INVALID: &str = "invalid_request";

fn take<T: DeserializeOwned>(fields: &mut Map<String, Value>, name: &str) -> Result<T, String> {
    serde_json::from_value(fields.remove(name).ok_or(INVALID)?).map_err(|_| INVALID.into())
}

fn value(fields: &mut Map<String, Value>, name: &str) -> Result<Value, String> {
    take(fields, name)
}

fn text(fields: &mut Map<String, Value>, name: &str) -> Result<String, String> {
    take(fields, name)
}

fn vector<T: DeserializeOwned>(
    fields: &mut Map<String, Value>,
    name: &str,
) -> Result<Vec<T>, String> {
    take(fields, name)
}

fn default_vector<T: DeserializeOwned>(
    fields: &mut Map<String, Value>,
    name: &str,
) -> Result<Vec<T>, String> {
    default(fields, name)
}

fn optional<T: DeserializeOwned>(
    fields: &mut Map<String, Value>,
    name: &str,
) -> Result<Option<T>, String> {
    match fields.remove(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => serde_json::from_value(value)
            .map(Some)
            .map_err(|_| INVALID.into()),
    }
}

fn default<T: DeserializeOwned + Default>(
    fields: &mut Map<String, Value>,
    name: &str,
) -> Result<T, String> {
    match fields.remove(name) {
        None => Ok(T::default()),
        Some(value) => serde_json::from_value(value).map_err(|_| INVALID.into()),
    }
}

struct RequestFields(Map<String, Value>);

impl<'de> Deserialize<'de> for RequestFields {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct FieldsVisitor;

        impl<'de> Visitor<'de> for FieldsVisitor {
            type Value = RequestFields;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a request object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut input: A) -> Result<Self::Value, A::Error> {
                let mut fields = Map::new();
                while let Some((name, value)) = input.next_entry::<String, Value>()? {
                    if fields.insert(name, value).is_some() {
                        return Err(serde::de::Error::custom("duplicate request field"));
                    }
                }
                Ok(RequestFields(fields))
            }
        }

        deserializer.deserialize_map(FieldsVisitor)
    }
}

pub(super) fn run(input: &str) -> Result<Value, String> {
    let mut fields = serde_json::from_str::<RequestFields>(input)
        .map_err(|_| INVALID)?
        .0;
    let op: String = take(&mut fields, "op")?;
    match op.as_str() {
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "name" => Ok(json!(wire::name(&value(&mut fields, "row")?)?)),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "newest" => Ok(json!(wire::newest(&value(&mut fields, "row")?)?)),
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "merge" => merge(&value(&mut fields, "a")?, &value(&mut fields, "b")?),
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "capture" => capture(
            &value(&mut fields, "before")?,
            &value(&mut fields, "after")?,
            &take(&mut fields, "at")?,
            &text(&mut fields, "id")?,
        ),
        #[cfg(feature = "tracker-delivery")]
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "commands" => commands(
            &value(&mut fields, "event")?,
            &value(&mut fields, "current")?,
        ),
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "episode_mark" => episode_mark(
            &value(&mut fields, "row")?,
            &value(&mut fields, "mark")?,
            default(&mut fields, "authoritative")?,
        ),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "aired_episodes" => Ok(aired_episodes(
            &vector(&mut fields, "seasons")?,
            optional(&mut fields, "last_aired")?,
        )),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "series_state" => Ok(series_state(
            &vector(&mut fields, "seasons")?,
            optional(&mut fields, "last_aired")?,
            &vector(&mut fields, "watched")?,
        )),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "episode_after" => Ok(episode_after(
            take(&mut fields, "at")?,
            &vector(&mut fields, "seasons")?,
            optional(&mut fields, "last_aired")?,
        )),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "continue_target" => Ok(continue_target(
            &vector(&mut fields, "seasons")?,
            optional(&mut fields, "last_aired")?,
            optional(&mut fields, "last_played")?,
        )),
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "continue_entry" => {
            let input: ContinueInput =
                serde_json::from_value(Value::Object(fields)).map_err(|_| INVALID.to_string())?;
            Ok(continue_entry(&input))
        }
        "issue" => {
            let last: Stamp = take(&mut fields, "last")?;
            let last = optional::<Stamp>(&mut fields, "seen")?
                .map(|seen| last.clone().max(seen))
                .unwrap_or(last);
            Ok(json!(last.issue(
                take(&mut fields, "now")?,
                take(&mut fields, "device")?,
            )?))
        }
        #[cfg(feature = "tracker-delivery")]
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "decide" => Ok(json!(decide(
            &take(&mut fields, "command")?,
            &take(&mut fields, "remote")?,
        ))),
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "episode_state_v4" => {
            let title = optional::<Value>(&mut fields, "title")?
                .as_ref()
                .map(library_v4::readable)
                .transpose()?;
            let season = optional::<Value>(&mut fields, "season")?
                .as_ref()
                .map(library_v4::readable)
                .transpose()?;
            let episode: Value = take(&mut fields, "episode")?;
            let episode = match episode {
                Value::String(key) => key,
                other => other.to_string(),
            };
            library_v4::state::episode_state(
                title.as_ref(),
                season.as_ref(),
                &episode,
                take(&mut fields, "now")?,
            )
        }
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "season_state_v4" => {
            let title = optional::<Value>(&mut fields, "title")?
                .as_ref()
                .map(library_v4::readable)
                .transpose()?;
            let season = optional::<Value>(&mut fields, "season")?
                .as_ref()
                .map(library_v4::readable)
                .transpose()?;
            library_v4::state::season_state(
                title.as_ref(),
                season.as_ref(),
                take(&mut fields, "now")?,
            )
        }
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "film_state_v4" => library_v4::state::film_state(
            &library_v4::readable(&value(&mut fields, "title")?)?,
            take(&mut fields, "now")?,
        ),
        #[cfg(feature = "tracker-delivery")]
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "settle_v4" => {
            let entry = optional::<Value>(&mut fields, "entry")?;
            library_v4::delivery::settle(
                &value(&mut fields, "outcome")?,
                &value(&mut fields, "built_from")?,
                &value(&mut fields, "order")?,
                entry.as_ref().filter(|entry| !entry.is_null()),
            )
        }
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "lease" => lease(&value(&mut fields, "input")?),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "write_back_v4" => library_v4::write_back(
            &vector(&mut fields, "documents")?,
            &default_vector(&mut fields, "kept")?,
            &vector(&mut fields, "log")?,
            take(&mut fields, "now")?,
        ),
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "doc_decode" => Ok(library_v4::decode(
            &library_v4::codec::unbase64(&text(&mut fields, "plaintext")?)?,
            optional::<String>(&mut fields, "name")?.as_deref(),
        )),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "doc_encode" => library_v4::encode(
            &value(&mut fields, "document")?,
            default(&mut fields, "write")?,
        ),
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "doc_name" => Ok(json!(library_v4::name(&value(&mut fields, "document")?)?)),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "compaction_guard" => Ok(library_v4::compaction_guard(
            take(&mut fields, "unreadable")?,
            take(&mut fields, "rows")?,
        )),
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "doc_merge" => library_v4::doc_merge(&value(&mut fields, "a")?, &value(&mut fields, "b")?),
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "profiles_validate" => profiles::validate_input(
            &value(&mut fields, "document")?,
            default(&mut fields, "stored")?,
        ),
        #[cfg(feature = "profile-admin")]
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profiles_merge" => profiles::merge(&value(&mut fields, "a")?, &value(&mut fields, "b")?),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profiles_apply" => {
            let entry = optional::<Value>(&mut fields, "entry")?;
            let setting_value = optional::<Value>(&mut fields, "value")?;
            let at = optional::<Value>(&mut fields, "at")?;
            profiles::apply(
                &value(&mut fields, "document")?,
                &text(&mut fields, "operation")?,
                &text(&mut fields, "profile")?,
                entry.as_ref(),
                setting_value.as_ref(),
                at.as_ref(),
            )
        }
        #[cfg(feature = "profile-admin")]
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profiles_materialize" => profiles::materialize(&value(&mut fields, "document")?),
        #[cfg(feature = "profile-admin")]
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profiles_create_next" => profiles::create_next(&value(&mut fields, "input")?),

        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profiles_construct_directory" => {
            profiles::construct_directory(&value(&mut fields, "input")?)
        }
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profiles_snapshot_digest" => {
            profiles::snapshot_digest(&text(&mut fields, "kind")?, &value(&mut fields, "input")?)
        }

        #[cfg(feature = "profile-admin")]
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profiles_reset_next" => profiles::reset_next(&value(&mut fields, "input")?),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profiles_partition_preferences" => {
            profiles::partition_preferences(&value(&mut fields, "values")?)
        }
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "profiles_seed_preferences" => profiles::seed_preferences(&value(&mut fields, "input")?),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "preferences_subtitle_v1" => {
            let operation = text(&mut fields, "operation")?;
            let values = optional::<Value>(&mut fields, "values")?;
            let choice = optional::<Value>(&mut fields, "choice")?;
            preferences::subtitle_v1(&operation, values.as_ref(), choice.as_ref())
        }
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profile_household_write" => profiles::household_write(&value(&mut fields, "row")?),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profile_household_audit" => profiles::household_audit(&value(&mut fields, "rows")?),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profile_policy_normalize_legacy" => {
            profiles::normalize_legacy(optional::<String>(&mut fields, "legacy")?.as_deref())
        }
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "profile_policy_allows" => profiles::policy_allows(
            &value(&mut fields, "policy")?,
            optional::<Value>(&mut fields, "minimumAge")?.as_ref(),
            default(&mut fields, "explicitApproval")?,
        ),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "profile_policy_relation" => {
            profiles::policy_relation(&value(&mut fields, "a")?, &value(&mut fields, "b")?)
        }
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "kid_title_approvals_validate" => profiles::approvals_validate(
            &text(&mut fields, "logicalName")?,
            &value(&mut fields, "document")?,
            &value(&mut fields, "directory")?,
        ),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "kid_title_approvals_merge" => profiles::approvals_merge(
            &text(&mut fields, "logicalName")?,
            &value(&mut fields, "a")?,
            &value(&mut fields, "b")?,
            &value(&mut fields, "directory")?,
        ),
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "title_state" => Ok(library_v4::state::title_state(
            &library_v4::readable(&value(&mut fields, "title")?)?,
            take(&mut fields, "now")?,
        )),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "apply_write" => {
            let title = optional::<Value>(&mut fields, "title")?;
            let seasons = default_vector(&mut fields, "seasons")?;
            let receipts = default_vector(&mut fields, "receipts")?;
            library_v4::write::apply_write(
                &value(&mut fields, "write")?,
                &value(&mut fields, "target")?,
                title.as_ref(),
                &seasons,
                &receipts,
                take(&mut fields, "now")?,
            )
        }
        #[cfg(feature = "tracker-delivery")]
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "delivery_write" => {
            let document = optional::<Value>(&mut fields, "document")?;
            library_v4::delivery::delivery_write(
                document.as_ref(),
                &default(&mut fields, "identity")?,
                &vector(&mut fields, "commands")?,
            )
        }
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "download_merge" => download_merge(&value(&mut fields, "a")?, &value(&mut fields, "b")?),
        #[cfg(any(not(feature = "browser-deferred"), feature = "browser-startup"))]
        "download_status" => {
            let answer = optional::<Value>(&mut fields, "answer")?;
            let clock = optional::<Value>(&mut fields, "clock")?;
            download_status(
                &value(&mut fields, "row")?,
                answer.as_ref(),
                clock.as_ref(),
                take(&mut fields, "now")?,
            )
        }
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "download_next" => download_next(
            &value(&mut fields, "row")?,
            &default_vector(&mut fields, "releases")?,
            &text(&mut fields, "resolution")?,
            default(&mut fields, "complete")?,
        ),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "download_retry" => download_retry(&value(&mut fields, "row")?, take(&mut fields, "now")?),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "download_cancel_safe" => download_cancel_safe(
            &value(&mut fields, "row")?,
            &default_vector(&mut fields, "rows")?,
            optional::<String>(&mut fields, "identity")?.as_deref(),
        ),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "download_escalate" => {
            let releases = default_vector(&mut fields, "releases")?;
            let original = optional::<String>(&mut fields, "original")?;
            let preferred = optional::<String>(&mut fields, "preferred")?;
            let tried = default_vector(&mut fields, "tried")?;
            let in_flight = default_vector(&mut fields, "in_flight")?;
            download_escalate(
                &releases,
                original.as_deref(),
                preferred.as_deref(),
                &tried,
                &in_flight,
                &RaceState {
                    stage: default(&mut fields, "stage")?,
                    stage_at: take(&mut fields, "stage_at")?,
                    started_at: take(&mut fields, "started_at")?,
                    capped_at: optional(&mut fields, "capped_at")?,
                },
                take(&mut fields, "now")?,
            )
        }
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "download_prune" => download_prune(
            &vector(&mut fields, "rows")?,
            &default(&mut fields, "states")?,
            &default(&mut fields, "watched")?,
            take(&mut fields, "now")?,
        ),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "rank_releases" => {
            let original = optional::<String>(&mut fields, "original")?;
            let preferred = optional::<String>(&mut fields, "preferred")?;
            Ok(rank_releases(
                &vector(&mut fields, "releases")?,
                original.as_deref(),
                preferred.as_deref(),
                &default_vector(&mut fields, "tried")?,
            ))
        }
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "watch_name" => Ok(json!(watch_row_name(
            &text(&mut fields, "media")?,
            take(&mut fields, "id")?,
            take(&mut fields, "season")?,
            take(&mut fields, "episode")?,
        )?)),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "receipt_name" => Ok(json!(receipt_row_name(
            &text(&mut fields, "provider")?,
            &text(&mut fields, "account")?,
            &text(&mut fields, "target")?,
        )?)),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "recovery_code" => recovery::code(&text(&mut fields, "random")?),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "recovery_read" => recovery::read(&text(&mut fields, "text")?),
        #[cfg(feature = "recovery-kdf")]
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "recovery_derive" => recovery::derive(&text(&mut fields, "data")?),
        #[cfg(any(not(feature = "browser-startup"), feature = "browser-deferred"))]
        "retry" => {
            let attempts: u32 = take(&mut fields, "attempts")?;
            let now: u64 = take(&mut fields, "now")?;
            let delay = (5_000u64 * (1u64 << attempts.min(10))).min(1_800_000);
            let at = now
                .checked_add(delay)
                .ok_or("time_overflow")?
                .max(optional(&mut fields, "retry_after")?.unwrap_or(0));
            if at > wire::MAX_SAFE_INTEGER {
                return Err("time_overflow".into());
            }
            Ok(json!(at))
        }
        op if op.starts_with("assistant_") || op.starts_with("playback_receipt_") => {
            Err("assistant_module".into())
        }
        _ => Err(INVALID.into()),
    }
}
