//! Pure library state and delivery decisions. No clocks, storage, network, credentials, or async runtime.
//! All time and provider facts arrive as inputs; bindings return the same versioned JSON envelope.

mod assistant;
mod delivery;
mod downloads;
mod episodes;
mod events;
mod library_v4;
mod profiles;
mod projection;
mod recovery;
mod series;
mod watch;
mod web_core;
mod wire;

use serde::Deserialize;
use serde_json::{json, Value};

// Whole-library operations keep the boundary bounded, but large enough for den-edge's 32 MiB stored-library limit
// plus JSON field names and request framing.
const MAX_REQUEST_BYTES: usize = 64 * 1024 * 1024;

pub use delivery::{decide, Action, Command, Decision, Kind, Remote, RemoteRating, RemoteTime};
pub use downloads::{
    download_cancel_safe, download_escalate, download_merge, download_next, download_prune,
    download_retry, download_status, rank_releases, RaceState,
};
pub use episodes::episode_mark;
pub use events::commands;
pub use profiles::{
    apply as profiles_apply, approvals_merge, approvals_validate,
    create_next as profiles_create_next, household_audit as profile_household_audit,
    household_write as profile_household_write, materialize as profiles_materialize,
    migration_authorization as profiles_migration_authorization,
    migration_authorization_fingerprint as profiles_migration_authorization_fingerprint,
    migration_construct_directory as profiles_construct_directory,
    migration_construct_directory as profiles_migration_construct_directory,
    migration_digest as profiles_migration_digest,
    migration_materialize_plaintext as profiles_migration_materialize_plaintext,
    migration_next as profiles_migration_next,
    migration_partition_rows as profiles_migration_partition_rows,
    partition_preferences as profiles_partition_preferences, policy_relation,
    profile_capability as profiles_capability, profiles_merge, profiles_validate,
    reset_next as profiles_reset_next, seed_preferences as profiles_seed_preferences,
    snapshot_digest as profiles_snapshot_digest, validate_input as profiles_validate_input,
};
pub use series::{
    aired_episodes, continue_entry, continue_target, episode_after, is_aired, series_state,
    ContinueInput, ContinueMark, Coord, LastPlayed, SeasonCount,
};
pub use watch::{lease, receipt_row_name, watch_row_name};
pub use wire::{capture, merge, Stamp};

/// The former typed core boundary, kept in tests as a parity oracle for the compact boundary.
#[cfg(test)]
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum Request {
    Name {
        row: Value,
    },
    Newest {
        row: Value,
    },
    Merge {
        a: Value,
        b: Value,
    },
    Capture {
        before: Value,
        after: Value,
        at: Stamp,
        id: String,
    },
    #[cfg(feature = "tracker-delivery")]
    Commands {
        event: Value,
        current: Value,
    },
    EpisodeMark {
        row: Value,
        mark: Value,
        /// Absent means "reconstructed", which is the conservative reading.
        #[serde(default)]
        authoritative: bool,
    },
    AiredEpisodes {
        seasons: Vec<SeasonCount>,
        last_aired: Option<Coord>,
    },
    SeriesState {
        seasons: Vec<SeasonCount>,
        last_aired: Option<Coord>,
        watched: Vec<Coord>,
    },
    EpisodeAfter {
        seasons: Vec<SeasonCount>,
        last_aired: Option<Coord>,
        at: Coord,
    },
    ContinueTarget {
        seasons: Vec<SeasonCount>,
        last_aired: Option<Coord>,
        last_played: Option<LastPlayed>,
    },
    /// The request's own fields are the input's (`tagged` reads it from the whole request, `op` ignored).
    ContinueEntry(ContinueInput),
    Issue {
        last: Stamp,
        seen: Option<Stamp>,
        now: i64,
        device: String,
    },
    #[cfg(feature = "tracker-delivery")]
    Decide {
        command: Command,
        remote: Remote,
    },
    /// Library v4 §7: an episode's state from its series title and season documents.
    EpisodeStateV4 {
        #[serde(default)]
        title: Option<Value>,
        #[serde(default)]
        season: Option<Value>,
        episode: Value,
        now: i64,
    },
    /// Library v4 §7: a film's state from its title document.
    FilmStateV4 {
        title: Value,
        now: i64,
    },
    /// Library v4 §9: also the entry it replaces (absent or null for none), whose lasting elements it keeps.
    #[cfg(feature = "tracker-delivery")]
    SettleV4 {
        outcome: Value,
        built_from: Value,
        order: Value,
        #[serde(default)]
        entry: Option<Value>,
    },
    Lease {
        input: Value,
    },
    /// Library v4 §11: held documents and kept ops on the new log.
    WriteBackV4 {
        documents: Vec<Value>,
        #[serde(default)]
        kept: Vec<Value>,
        log: Vec<Value>,
        now: i64,
    },
    /// Library v4 §4: a row's opened plaintext (base64) → document, JSON row, unreadable or newer.
    DocDecode {
        plaintext: String,
        #[serde(default)]
        name: Option<String>,
    },
    /// Library v4 §4: document → plaintext (base64url), or `too_large` (224 KiB for a §8 write, else 256 KiB).
    DocEncode {
        document: Value,
        #[serde(default)]
        write: bool,
    },
    DocName {
        document: Value,
    },
    /// Library v4 §4 *Unreadable rows*: whether a compaction may remove `unreadable` rows of the `rows` read through
    /// its base.
    CompactionGuard {
        unreadable: u64,
        rows: u64,
    },
    DocMerge {
        a: Value,
        b: Value,
    },
    /// Profiles v1: validate one logical profile-directory document without reading any external state.
    ProfilesValidate {
        document: Value,
        #[serde(default)]
        stored: bool,
    },
    /// Profiles v1: converge two valid versions of one profile-directory document.
    #[cfg(feature = "profile-admin")]
    ProfilesMerge {
        a: Value,
        b: Value,
    },
    /// Profiles v1: construct one owner-authorized directory edit from caller-issued identity, key, and stamp input.
    ProfilesApply {
        document: Value,
        operation: String,
        profile: String,
        #[serde(default)]
        entry: Option<Value>,
        #[serde(default)]
        value: Option<Value>,
        #[serde(default)]
        at: Option<Value>,
    },
    /// Profiles v1: turn one stored-valid directory into canonical library-v4 document bytes.
    #[cfg(feature = "profile-admin")]
    ProfilesMaterialize {
        document: Value,
    },
    /// Profiles v1: choose the next crash-safe persistent Kid creation effect.
    #[cfg(feature = "profile-admin")]
    ProfilesCreateNext {
        input: Value,
    },
    /// Profiles v1: choose the next initial-split effect from validated durable observations.
    #[cfg(feature = "profile-admin")]
    ProfilesMigrationNext {
        input: Value,
    },
    /// Profiles v1: construct one exact-attempt split authorization or bind it to verified commit evidence.
    #[cfg(feature = "profile-admin")]
    ProfilesMigrationAuthorization {
        input: Value,
    },
    /// Profiles v1: calculate one canonical migration evidence digest without storage or network effects.
    #[cfg(feature = "profile-admin")]
    ProfilesMigrationDigest {
        kind: String,
        input: Value,
    },
    /// Profiles v1: construct and stored-validate the first Owner-only directory.
    #[cfg(feature = "profile-admin")]
    ProfilesMigrationConstructDirectory {
        input: Value,
    },
    /// Profiles v1 §6.2: `profiles_migration_construct_directory` under the name that outlives the split.
    ProfilesConstructDirectory {
        input: Value,
    },
    /// Profiles v1 §1.2: the `source` and `owner` digests of `profiles_migration_digest`, under the name that
    /// outlives the split.
    ProfilesSnapshotDigest {
        kind: String,
        input: Value,
    },
    /// Profiles v1: validate a materialized projection and return its canonical seal-ready bytes.
    #[cfg(feature = "profile-admin")]
    ProfilesMigrationMaterializePlaintext {
        input: Value,
    },
    /// Profiles v1: reproduce the provider's non-secret backup authorization representation.
    #[cfg(feature = "profile-admin")]
    ProfilesMigrationAuthorizationFingerprint {
        input: Value,
    },
    /// Profiles v1: resolve one household's off/prepare/derived-runtime capability mode.
    ProfilesCapability {
        input: Value,
    },
    /// Profiles v1: choose the next compound reset/revocation effect from validated move observations.
    #[cfg(feature = "profile-admin")]
    ProfilesResetNext {
        input: Value,
    },
    /// Profiles v1: partition the legacy `set:prefs.values` object for household and Owner destinations.
    ProfilesPartitionPreferences {
        values: Value,
    },
    /// Profiles v1 §6.3: the `set:prefs` values a profile library is given from an older household's.
    ProfilesSeedPreferences {
        input: Value,
    },
    /// Profiles v1 §8.2: whether one row (`{name[, values]}`) may be written to a profile-aware household.
    ProfileHouseholdWrite {
        row: Value,
    },
    /// Profiles v1 §8.2: the same rule over stored household rows, answering which to remove and rewrite.
    ProfileHouseholdAudit {
        rows: Value,
    },
    /// Profiles v1: translate the legacy Owner maturity ceiling into the normalized policy.
    ProfilePolicyNormalizeLegacy {
        legacy: Option<String>,
    },
    /// Profiles v1: decide one title maturity signal against one normalized policy.
    ProfilePolicyAllows {
        policy: Value,
        #[serde(default, rename = "minimumAge")]
        minimum_age: Option<Value>,
        #[serde(default, rename = "explicitApproval")]
        explicit_approval: bool,
    },
    /// Profiles v1: compare policies in their product partial order.
    ProfilePolicyRelation {
        a: Value,
        b: Value,
    },
    /// Profiles v1: validate a per-Kid approval document in its directory/name context.
    KidTitleApprovalsValidate {
        #[serde(rename = "logicalName")]
        logical_name: String,
        document: Value,
        directory: Value,
    },
    /// Profiles v1: converge two versions of a per-Kid approval document.
    KidTitleApprovalsMerge {
        #[serde(rename = "logicalName")]
        logical_name: String,
        a: Value,
        b: Value,
        directory: Value,
    },
    TitleState {
        title: Value,
        now: i64,
    },
    /// Library v4 §8: one write on the documents it touches → the documents to write.
    ApplyWrite {
        write: Value,
        target: Value,
        #[serde(default)]
        title: Option<Value>,
        #[serde(default)]
        seasons: Vec<Value>,
        #[serde(default)]
        receipts: Vec<Value>,
        now: i64,
    },
    /// Library v4 §9 *Fit before sending*.
    #[cfg(feature = "tracker-delivery")]
    DeliveryWrite {
        #[serde(default)]
        document: Option<Value>,
        #[serde(default)]
        identity: Value,
        commands: Vec<Value>,
    },
    /// Library v4 §17: two versions of one `set:download:<content>` row.
    DownloadMerge {
        a: Value,
        b: Value,
    },
    /// Library v4 §17: one den-scout answer (or none yet) for a download row.
    DownloadStatus {
        row: Value,
        #[serde(default)]
        answer: Option<Value>,
        #[serde(default)]
        clock: Option<Value>,
        now: i64,
    },
    /// Library v4 §17: the release a stalled download moves on to, from a fresh resolve.
    DownloadNext {
        row: Value,
        #[serde(default)]
        releases: Vec<Value>,
        resolution: String,
        #[serde(default)]
        complete: bool,
    },
    /// Library v4 §17: whether an exhausted download searches again now, and when.
    DownloadRetry {
        row: Value,
        now: i64,
    },
    /// Library v4 §17: whether a row's release may be cancelled at the debrid, given every other download row.
    DownloadCancelSafe {
        row: Value,
        #[serde(default)]
        rows: Vec<Value>,
        /// One of the row's race releases, instead of its `release`.
        #[serde(default)]
        identity: Option<String>,
    },
    /// Library v4 §17: one pass of a download's race — what to take, add and cancel. Row-free, so den-hearth calls it.
    DownloadEscalate {
        #[serde(default)]
        releases: Vec<Value>,
        #[serde(default)]
        original: Option<String>,
        #[serde(default)]
        preferred: Option<String>,
        #[serde(default)]
        tried: Vec<String>,
        #[serde(default)]
        in_flight: Vec<Value>,
        #[serde(default)]
        stage: i64,
        stage_at: i64,
        started_at: i64,
        #[serde(default)]
        capped_at: Option<i64>,
        now: i64,
    },
    /// Library v4 §17: the live download rows to tombstone now.
    DownloadPrune {
        rows: Vec<Value>,
        #[serde(default)]
        states: serde_json::Map<String, Value>,
        /// By row name: whether the caller's own library holds that row's episode or film as watched — never
        /// the series' standing, which says nothing about one episode.
        #[serde(default)]
        watched: serde_json::Map<String, Value>,
        now: i64,
    },
    /// Library v4 §17: the TV's release ranking — play order, the quality badge's release, and a download's pick.
    RankReleases {
        releases: Vec<Value>,
        #[serde(default)]
        original: Option<String>,
        #[serde(default)]
        preferred: Option<String>,
        #[serde(default)]
        tried: Vec<String>,
    },
    WatchName {
        media: String,
        id: u64,
        season: u64,
        episode: u64,
    },
    ReceiptName {
        provider: String,
        account: String,
        target: String,
    },
    Retry {
        attempts: u32,
        now: u64,
        retry_after: Option<u64>,
    },
    /// Recovery code §2: 22 random bytes (hex) from the platform's CSPRNG → `{code, data}`.
    RecoveryCode {
        random: String,
    },
    /// Recovery code §2: typed text → `{data}`, or `mistyped` / `checksum`, before any derivation or request.
    RecoveryRead {
        text: String,
    },
    /// Recovery code §3: data characters → `{locator, wrapKey}` (hex). One Argon2id at 64 MiB: run it off the main
    /// thread.
    #[cfg(feature = "recovery-kdf")]
    RecoveryDerive {
        data: String,
    },
}

/// The assistant ops (den-spec `wire/assistant-v1.md`), apart from the rest so the web can load them as a module of
/// their own (`evaluate_assistant`): they carry the assistant crypto, which every other op leaves out. `merge` of the
/// assistant rows stays with the other ops, since every row merges there.
// The variant names are the wire op names (`assistant_keygen_dropbox`, ...), so the shared prefix stays.
#[allow(clippy::enum_variant_names)]
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum AssistantRequest {
    /// Assistant v1 §4: 32 random bytes (hex) → a drop-box key and the `set:assistant` setting holding it.
    AssistantKeygenDropbox { random: String },
    /// Assistant v1 §8: the drop-box public key den-edge should hold for a `set:assistant` row, or null.
    AssistantDropbox {
        #[serde(default)]
        assistant: Option<Value>,
    },
    /// Assistant v1 §4: 32 random bytes (hex) and the consent → a grant key and its `set:assistant-grants` setting.
    AssistantKeygenGrant {
        random: String,
        client: String,
        ops: Vec<String>,
        cap: u64,
        now: u64,
    },
    /// Assistant v1 §5: a grant's setting value with `revokedAt` set.
    AssistantRevoke {
        grant: String,
        value: Value,
        now: u64,
    },
    /// Assistant v1 §5: a grant's setting value with `expiresAt` moved to `now` + 30 days.
    AssistantRenew {
        grant: String,
        value: Value,
        now: u64,
    },
    /// Assistant v1 §5, §15: the grants a `set:assistant-grants` row holds and the read records a `set:assistant-read`
    /// row holds, with their state, for Settings.
    AssistantGrants {
        #[serde(default)]
        grants: Option<Value>,
        #[serde(default)]
        read: Option<Value>,
        #[serde(default)]
        play: Option<Value>,
        #[serde(default, rename = "localRevoked")]
        local_revoked: Vec<String>,
        now: u64,
    },
    /// Assistant v1 §6: a sealed request against the library's three assistant rows and the device's own records →
    /// accept or reject.
    AssistantOpen {
        library: String,
        device: String,
        sealed: String,
        #[serde(default)]
        assistant: Option<Value>,
        #[serde(default)]
        grants: Option<Value>,
        #[serde(default)]
        applied: Option<Value>,
        #[serde(default, rename = "localRevoked")]
        local_revoked: Vec<String>,
        #[serde(default, rename = "localApplied")]
        local_applied: serde_json::Map<String, Value>,
        /// §16: the play row, the queue drained (`library`, the default, or `tv`), and for `tv` when the TV last woke.
        #[serde(default)]
        play: Option<Value>,
        #[serde(default)]
        queue: Option<String>,
        #[serde(default, rename = "awakeSince")]
        awake_since: Option<u64>,
        now: u64,
    },
    /// Assistant v1 §16: a connection's `set:assistant-play` record.
    AssistantKeygenPlay {
        grant: String,
        client: String,
        now: u64,
    },
    /// Assistant v1 §15: 32 random bytes (hex) → a grant key and id with no grants row setting, for a read-only
    /// connection.
    AssistantGrantKey { random: String },
    /// Assistant v1 §15: 32 random bytes (hex) → a connection's read key and its `set:assistant-read` record.
    AssistantKeygenRead {
        grant: String,
        random: String,
        client: String,
        now: u64,
    },
    /// Assistant v1 §5: the applied entries to drop, from the row and the device's own record.
    AssistantPrune {
        #[serde(default)]
        applied: Option<Value>,
        #[serde(default, rename = "localApplied")]
        local_applied: serde_json::Map<String, Value>,
        now: u64,
    },
}

/// Assistant v1 §15: the library's documents → the projection for every live read grant, sealed, and the read grants
/// whose projection den-edge should drop. Read apart from `AssistantRequest`: an internally tagged enum buffers its
/// fields, so it cannot leave the documents and layouts as slices of the request. Each is parsed only when it is used
/// (`projection::lists`), so the whole library is never held as `Value`s at once.
#[derive(Deserialize)]
struct ProjectionRequest<'a> {
    library: String,
    #[serde(default, borrow)]
    documents: Vec<&'a serde_json::value::RawValue>,
    #[serde(default, borrow)]
    layouts: std::collections::BTreeMap<String, &'a serde_json::value::RawValue>,
    head: u64,
    #[serde(default)]
    read: Option<Value>,
    #[serde(default)]
    grants: Option<Value>,
    /// The `set:prefs` row, for the projection's hide rules.
    #[serde(default)]
    prefs: Option<Value>,
    #[serde(default, rename = "localRevoked")]
    local_revoked: Vec<String>,
    random: String,
    now: u64,
}

/// `assistant_projection`'s answer when `input` names it, else `None`.
fn projection_op(input: &str) -> Option<Result<Value, String>> {
    #[derive(Deserialize)]
    struct Op<'a> {
        #[serde(borrow)]
        op: std::borrow::Cow<'a, str>,
    }
    // Every op comes through here: only one that could be a projection is parsed for its name.
    if !input.contains("assistant_projection") {
        return None;
    }
    if serde_json::from_str::<Op>(input).ok()?.op != "assistant_projection" {
        return None;
    }
    Some(
        serde_json::from_str::<ProjectionRequest>(input)
            .ok()
            // Refused as a whole when a document or layout is no JSON value (a number out of range, a lone surrogate),
            // as when they were parsed with the request: each is parsed and dropped, so only one is held at a time.
            .filter(|request| {
                request
                    .documents
                    .iter()
                    .chain(request.layouts.values())
                    .all(|d| serde_json::from_str::<Value>(d.get()).is_ok())
            })
            .ok_or_else(|| "invalid_request".to_string())
            .and_then(|request| {
                assistant::projection(
                    &projection::Library {
                        library: &request.library,
                        documents: &request.documents,
                        layouts: &request.layouts,
                        head: request.head,
                        prefs: request.prefs.as_ref(),
                    },
                    request.read.as_ref(),
                    request.grants.as_ref(),
                    &request.local_revoked,
                    &request.random,
                    request.now,
                )
            }),
    )
}

/// Library v4 §9 `pending_targets_v4`: the documents, delivery documents included, and the account's `set:deliver`
/// facts.
#[cfg(feature = "tracker-delivery")]
#[derive(Deserialize)]
struct PendingTargetsRequest<'a> {
    #[serde(borrow)]
    documents: Vec<&'a serde_json::value::RawValue>,
    deliver: Value,
    now: i64,
}

/// Profiles v1: account every authenticated legacy row into household/Owner migration material.
#[cfg(feature = "profile-admin")]
#[derive(Deserialize)]
struct PartitionRequest<'a> {
    #[serde(borrow)]
    input: &'a serde_json::value::RawValue,
}

/// The answer, enveloped, of an op Den Web sends a whole library to, when `input` names one, else `None`. They are
/// read apart from `Request` for the reason `ProjectionRequest` is: each document or row stays a slice of the request
/// until it is used. Their answers are written straight from what they hold, not built as `Value`s first.
fn library_op(input: &str) -> Option<String> {
    #[derive(Deserialize)]
    struct Op<'a> {
        #[serde(borrow)]
        op: std::borrow::Cow<'a, str>,
    }
    #[derive(serde::Serialize)]
    struct Answer<T> {
        ok: T,
        version: u8,
    }
    fn answer<T: serde::Serialize>(result: Result<T, String>) -> String {
        match result {
            Ok(ok) => serde_json::to_string(&Answer { ok, version: 1 })
                .expect("an answer always serializes"),
            Err(error) => envelope(Err(error)),
        }
    }
    fn read<'a, T: Deserialize<'a>>(input: &'a str) -> Result<T, String> {
        serde_json::from_str(input).map_err(|_| "invalid_request".to_string())
    }
    const OPS: [&str; 2] = ["pending_targets_v4", "profiles_migration_partition_rows"];
    if bounded(input).is_err() || !OPS.iter().any(|op| input.contains(op)) {
        return None;
    }
    let op = serde_json::from_str::<Op>(input).ok()?.op;
    Some(match &*op {
        #[cfg(feature = "tracker-delivery")]
        "pending_targets_v4" => answer(read(input).and_then(|r: PendingTargetsRequest| {
            library_v4::delivery::pending(
                r.documents
                    .iter()
                    .map(|d| library_v4::delivery::Source::Raw(d)),
                &r.deliver,
                r.now,
            )
        })),
        #[cfg(feature = "profile-admin")]
        "profiles_migration_partition_rows" => answer(
            read(input).and_then(|r: PartitionRequest| profiles::migration_partition(r.input)),
        ),
        _ => return None,
    })
}

/// Versioned, non-throwing FFI boundary. An error is never an empty snapshot or an acknowledgement.
///
/// Every op, the assistant's included: the native binding's one entry. The web splits it in two, `evaluate_core` and
/// `evaluate_assistant`, so the assistant crypto loads only where it is used.
pub fn evaluate(input: &str) -> String {
    if let Some(answer) = library_op(input) {
        return answer;
    }
    envelope(bounded(input).and_then(|()| {
        if let Some(answer) = projection_op(input) {
            return answer;
        }
        match web_core::run(input) {
            Err(error) if error == "assistant_module" => match tagged::<AssistantRequest>(input) {
                Ok(request) => run_assistant(request),
                Err(_) => Err("invalid_request".into()),
            },
            answer => answer,
        }
    }))
}

#[cfg(test)]
fn evaluate_legacy(input: &str) -> String {
    if let Some(answer) = library_op(input) {
        return answer;
    }
    envelope(bounded(input).and_then(|()| {
        if let Some(answer) = projection_op(input) {
            return answer;
        }
        match tagged::<Request>(input) {
            Ok(request) => run(request),
            Err(_) => match tagged::<AssistantRequest>(input) {
                Ok(request) => run_assistant(request),
                Err(_) => Err("invalid_request".into()),
            },
        }
    }))
}

/// Every op but the assistant's: the web's main module. An assistant op is `assistant_module`, so a caller that sent
/// it to the wrong module says so rather than `invalid_request`.
pub fn evaluate_core(input: &str) -> String {
    if let Some(answer) = library_op(input) {
        return answer;
    }
    envelope(bounded(input).and_then(|()| web_core::run(input)))
}

/// The assistant ops alone (den-spec `wire/assistant-v1.md`): the web's assistant module.
pub fn evaluate_assistant(input: &str) -> String {
    envelope(bounded(input).and_then(|()| {
        if let Some(answer) = projection_op(input) {
            return answer;
        }
        tagged::<AssistantRequest>(input)
            .map_err(|_| "invalid_request".to_string())
            .and_then(run_assistant)
    }))
}

/// Recovery code §3's memory-hard KDF alone: the web loads this module only while making or redeeming a code.
#[cfg(feature = "recovery-kdf")]
pub fn evaluate_recovery_kdf(input: &str) -> String {
    #[derive(Deserialize)]
    struct RecoveryKdfRequest {
        op: String,
        data: String,
    }

    envelope(bounded(input).and_then(|()| {
        let request: RecoveryKdfRequest =
            serde_json::from_str(input).map_err(|_| "invalid_request".to_string())?;
        if request.op != "recovery_derive" {
            return Err("invalid_request".into());
        }
        recovery::derive(&request.data)
    }))
}

fn bounded(input: &str) -> Result<(), String> {
    if input.len() > MAX_REQUEST_BYTES {
        return Err("request_too_large".into());
    }
    Ok(())
}

fn envelope(result: Result<Value, String>) -> String {
    match result {
        Ok(value) => json!({ "version": 1, "ok": value }).to_string(),
        Err(error) => json!({ "version": 1, "error": error }).to_string(),
    }
}

/// A request, `{"op": "<variant>", <its fields>...}`, as `T`'s variant: `op` is read first, skipping the rest, and the
/// variant is then read straight from the request, where `op` is a field it ignores.
///
/// Serde's own internal tagging (`tag = "op"`) buffers the request into an intermediate tree before it reads the
/// variant from that. It held every request twice, and the web module carried every field type's deserializer twice.
fn tagged<'de, T: Deserialize<'de>>(input: &'de str) -> Result<T, serde_json::Error> {
    #[derive(Deserialize)]
    struct Op<'a> {
        #[serde(borrow)]
        op: std::borrow::Cow<'a, str>,
    }
    let op = serde_json::from_str::<Op>(input)?.op;
    T::deserialize(Tagged { op: &op, input })
}

struct Tagged<'a, 'de> {
    op: &'a str,
    input: &'de str,
}

impl<'de> serde::Deserializer<'de> for Tagged<'_, 'de> {
    type Error = serde_json::Error;

    fn deserialize_any<V: serde::de::Visitor<'de>>(self, _: V) -> Result<V::Value, Self::Error> {
        Err(serde::de::Error::custom("a request is read as an enum"))
    }

    fn deserialize_enum<V: serde::de::Visitor<'de>>(
        self,
        _: &'static str,
        _: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        visitor.visit_enum(self)
    }

    serde::forward_to_deserialize_any! {
        bool i8 i16 i32 i64 i128 u8 u16 u32 u64 u128 f32 f64 char str string bytes byte_buf option unit
        unit_struct newtype_struct seq tuple tuple_struct map struct identifier ignored_any
    }
}

impl<'de> serde::de::EnumAccess<'de> for Tagged<'_, 'de> {
    type Error = serde_json::Error;
    type Variant = Self;

    fn variant_seed<S: serde::de::DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<(S::Value, Self), Self::Error> {
        let op = serde::de::value::StrDeserializer::<serde_json::Error>::new(self.op);
        Ok((seed.deserialize(op)?, self))
    }
}

impl<'de> serde::de::VariantAccess<'de> for Tagged<'_, 'de> {
    type Error = serde_json::Error;

    fn unit_variant(self) -> Result<(), Self::Error> {
        Err(serde::de::Error::custom("every op has fields"))
    }

    fn newtype_variant_seed<S: serde::de::DeserializeSeed<'de>>(
        self,
        seed: S,
    ) -> Result<S::Value, Self::Error> {
        self.body(|request| seed.deserialize(request))
    }

    fn tuple_variant<V: serde::de::Visitor<'de>>(
        self,
        _: usize,
        _: V,
    ) -> Result<V::Value, Self::Error> {
        Err(serde::de::Error::custom("every op has named fields"))
    }

    fn struct_variant<V: serde::de::Visitor<'de>>(
        self,
        fields: &'static [&'static str],
        visitor: V,
    ) -> Result<V::Value, Self::Error> {
        self.body(|request| serde::Deserializer::deserialize_struct(request, "", fields, visitor))
    }
}

impl<'de> Tagged<'_, 'de> {
    /// The variant's fields from the request itself. Only an object: an internally tagged enum also took
    /// `["<op>", <fields in order>...]`, which no client sends.
    fn body<T>(
        self,
        read: impl FnOnce(
            &mut serde_json::Deserializer<serde_json::de::StrRead<'de>>,
        ) -> Result<T, serde_json::Error>,
    ) -> Result<T, serde_json::Error> {
        let json = self.input.trim_start_matches([' ', '\t', '\n', '\r']);
        if !json.starts_with('{') {
            return Err(serde::de::Error::custom("a request is an object"));
        }
        let mut request = serde_json::Deserializer::from_str(self.input);
        let value = read(&mut request)?;
        request.end()?;
        Ok(value)
    }
}

fn run_assistant(request: AssistantRequest) -> Result<Value, String> {
    match request {
        AssistantRequest::AssistantKeygenDropbox { random } => assistant::keygen_dropbox(&random),
        AssistantRequest::AssistantDropbox { assistant } => assistant::dropbox(assistant.as_ref()),
        AssistantRequest::AssistantKeygenGrant {
            random,
            client,
            ops,
            cap,
            now,
        } => assistant::keygen_grant(&random, &client, &ops, cap, now),
        AssistantRequest::AssistantRevoke { grant, value, now } => {
            assistant::revoke(&grant, &value, now)
        }
        AssistantRequest::AssistantRenew { grant, value, now } => {
            assistant::renew(&grant, &value, now)
        }
        AssistantRequest::AssistantGrants {
            grants,
            read,
            play,
            local_revoked,
            now,
        } => assistant::list(
            grants.as_ref(),
            read.as_ref(),
            play.as_ref(),
            &local_revoked,
            now,
        ),
        AssistantRequest::AssistantKeygenPlay { grant, client, now } => {
            assistant::keygen_play(&grant, &client, now)
        }
        AssistantRequest::AssistantGrantKey { random } => assistant::grant_key(&random),
        AssistantRequest::AssistantKeygenRead {
            grant,
            random,
            client,
            now,
        } => assistant::keygen_read(&grant, &random, &client, now),
        AssistantRequest::AssistantOpen {
            library,
            device,
            sealed,
            assistant,
            grants,
            applied,
            local_revoked,
            local_applied,
            play,
            queue,
            awake_since,
            now,
        } => assistant::open(
            &sealed,
            &assistant::Library {
                library: &library,
                device: &device,
                dropbox: assistant.as_ref(),
                grants: grants.as_ref(),
                applied: applied.as_ref(),
                local_revoked: &local_revoked,
                local_applied: &local_applied,
                play: play.as_ref(),
                queue: match queue.as_deref() {
                    None | Some("library") => den_assistant::Queue::Library,
                    Some("tv") => den_assistant::Queue::Tv,
                    Some(_) => return Err("invalid_queue".into()),
                },
                awake_since,
            },
            now,
        ),
        AssistantRequest::AssistantPrune {
            applied,
            local_applied,
            now,
        } => assistant::prune(applied.as_ref(), &local_applied, now),
    }
}

#[cfg(test)]
fn run(request: Request) -> Result<Value, String> {
    match request {
        Request::Name { row } => Ok(json!(wire::name(&row)?)),
        Request::Newest { row } => Ok(json!(wire::newest(&row)?)),
        Request::Merge { a, b } => merge(&a, &b),
        Request::Capture {
            before,
            after,
            at,
            id,
        } => capture(&before, &after, &at, &id),
        #[cfg(feature = "tracker-delivery")]
        Request::Commands { event, current } => commands(&event, &current),
        Request::EpisodeMark {
            row,
            mark,
            authoritative,
        } => episode_mark(&row, &mark, authoritative),
        Request::AiredEpisodes {
            seasons,
            last_aired,
        } => Ok(aired_episodes(&seasons, last_aired)),
        Request::SeriesState {
            seasons,
            last_aired,
            watched,
        } => Ok(series_state(&seasons, last_aired, &watched)),
        Request::EpisodeAfter {
            seasons,
            last_aired,
            at,
        } => Ok(episode_after(at, &seasons, last_aired)),
        Request::ContinueTarget {
            seasons,
            last_aired,
            last_played,
        } => Ok(continue_target(&seasons, last_aired, last_played)),
        Request::ContinueEntry(input) => Ok(continue_entry(&input)),
        Request::Issue {
            last,
            seen,
            now,
            device,
        } => {
            let last = seen.map(|seen| last.clone().max(seen)).unwrap_or(last);
            Ok(json!(last.issue(now, device)?))
        }
        #[cfg(feature = "tracker-delivery")]
        Request::Decide { command, remote } => Ok(json!(decide(&command, &remote))),
        Request::EpisodeStateV4 {
            title,
            season,
            episode,
            now,
        } => {
            let title = title.as_ref().map(library_v4::readable).transpose()?;
            let season = season.as_ref().map(library_v4::readable).transpose()?;
            let episode = match &episode {
                Value::String(key) => key.clone(),
                other => other.to_string(),
            };
            library_v4::state::episode_state(title.as_ref(), season.as_ref(), &episode, now)
        }
        Request::FilmStateV4 { title, now } => {
            library_v4::state::film_state(&library_v4::readable(&title)?, now)
        }
        #[cfg(feature = "tracker-delivery")]
        Request::SettleV4 {
            outcome,
            built_from,
            order,
            entry,
        } => library_v4::delivery::settle(
            &outcome,
            &built_from,
            &order,
            entry.as_ref().filter(|e| !e.is_null()),
        ),
        Request::Lease { input } => lease(&input),
        Request::WriteBackV4 {
            documents,
            kept,
            log,
            now,
        } => library_v4::write_back(&documents, &kept, &log, now),
        Request::DocDecode { plaintext, name } => Ok(library_v4::decode(
            &library_v4::codec::unbase64(&plaintext)?,
            name.as_deref(),
        )),
        Request::DocEncode { document, write } => library_v4::encode(&document, write),
        Request::DocName { document } => Ok(json!(library_v4::name(&document)?)),
        Request::CompactionGuard { unreadable, rows } => {
            Ok(library_v4::compaction_guard(unreadable, rows))
        }
        Request::DocMerge { a, b } => library_v4::doc_merge(&a, &b),
        Request::ProfilesValidate { document, stored } => {
            profiles::validate_input(&document, stored)
        }
        #[cfg(feature = "profile-admin")]
        Request::ProfilesMerge { a, b } => profiles::merge(&a, &b),
        Request::ProfilesApply {
            document,
            operation,
            profile,
            entry,
            value,
            at,
        } => profiles::apply(
            &document,
            &operation,
            &profile,
            entry.as_ref(),
            value.as_ref(),
            at.as_ref(),
        ),
        #[cfg(feature = "profile-admin")]
        Request::ProfilesMaterialize { document } => profiles::materialize(&document),
        #[cfg(feature = "profile-admin")]
        Request::ProfilesCreateNext { input } => profiles::create_next(&input),
        #[cfg(feature = "profile-admin")]
        Request::ProfilesMigrationNext { input } => profiles::migration_next(&input),
        #[cfg(feature = "profile-admin")]
        Request::ProfilesMigrationAuthorization { input } => {
            profiles::migration_authorization(&input)
        }
        #[cfg(feature = "profile-admin")]
        Request::ProfilesMigrationDigest { kind, input } => {
            profiles::migration_digest(&kind, &input)
        }
        #[cfg(feature = "profile-admin")]
        Request::ProfilesMigrationConstructDirectory { input } => {
            profiles::migration_construct_directory(&input)
        }
        Request::ProfilesConstructDirectory { input } => {
            profiles::migration_construct_directory(&input)
        }
        Request::ProfilesSnapshotDigest { kind, input } => profiles::snapshot_digest(&kind, &input),
        #[cfg(feature = "profile-admin")]
        Request::ProfilesMigrationMaterializePlaintext { input } => {
            profiles::migration_materialize_plaintext(&input)
        }
        #[cfg(feature = "profile-admin")]
        Request::ProfilesMigrationAuthorizationFingerprint { input } => {
            profiles::migration_authorization_fingerprint(&input)
        }
        Request::ProfilesCapability { input } => profiles::profile_capability(&input),
        #[cfg(feature = "profile-admin")]
        Request::ProfilesResetNext { input } => profiles::reset_next(&input),
        Request::ProfilesPartitionPreferences { values } => {
            profiles::partition_preferences(&values)
        }
        Request::ProfilesSeedPreferences { input } => profiles::seed_preferences(&input),
        Request::ProfileHouseholdWrite { row } => profiles::household_write(&row),
        Request::ProfileHouseholdAudit { rows } => profiles::household_audit(&rows),
        Request::ProfilePolicyNormalizeLegacy { legacy } => {
            profiles::normalize_legacy(legacy.as_deref())
        }
        Request::ProfilePolicyAllows {
            policy,
            minimum_age,
            explicit_approval,
        } => profiles::policy_allows(&policy, minimum_age.as_ref(), explicit_approval),
        Request::ProfilePolicyRelation { a, b } => profiles::policy_relation(&a, &b),
        Request::KidTitleApprovalsValidate {
            logical_name,
            document,
            directory,
        } => profiles::approvals_validate(&logical_name, &document, &directory),
        Request::KidTitleApprovalsMerge {
            logical_name,
            a,
            b,
            directory,
        } => profiles::approvals_merge(&logical_name, &a, &b, &directory),
        Request::TitleState { title, now } => Ok(library_v4::state::title_state(
            &library_v4::readable(&title)?,
            now,
        )),
        Request::ApplyWrite {
            write,
            target,
            title,
            seasons,
            receipts,
            now,
        } => library_v4::write::apply_write(
            &write,
            &target,
            title.as_ref(),
            &seasons,
            &receipts,
            now,
        ),
        #[cfg(feature = "tracker-delivery")]
        Request::DeliveryWrite {
            document,
            identity,
            commands,
        } => library_v4::delivery::delivery_write(document.as_ref(), &identity, &commands),
        Request::DownloadMerge { a, b } => download_merge(&a, &b),
        Request::DownloadStatus {
            row,
            answer,
            clock,
            now,
        } => download_status(&row, answer.as_ref(), clock.as_ref(), now),
        Request::DownloadNext {
            row,
            releases,
            resolution,
            complete,
        } => download_next(&row, &releases, &resolution, complete),
        Request::DownloadRetry { row, now } => download_retry(&row, now),
        Request::DownloadCancelSafe {
            row,
            rows,
            identity,
        } => download_cancel_safe(&row, &rows, identity.as_deref()),
        Request::DownloadEscalate {
            releases,
            original,
            preferred,
            tried,
            in_flight,
            stage,
            stage_at,
            started_at,
            capped_at,
            now,
        } => download_escalate(
            &releases,
            original.as_deref(),
            preferred.as_deref(),
            &tried,
            &in_flight,
            &RaceState {
                stage,
                stage_at,
                started_at,
                capped_at,
            },
            now,
        ),
        Request::DownloadPrune {
            rows,
            states,
            watched,
            now,
        } => download_prune(&rows, &states, &watched, now),
        Request::RankReleases {
            releases,
            original,
            preferred,
            tried,
        } => Ok(rank_releases(
            &releases,
            original.as_deref(),
            preferred.as_deref(),
            &tried,
        )),
        Request::WatchName {
            media,
            id,
            season,
            episode,
        } => Ok(json!(watch_row_name(&media, id, season, episode)?)),
        Request::ReceiptName {
            provider,
            account,
            target,
        } => Ok(json!(receipt_row_name(&provider, &account, &target)?)),
        Request::RecoveryCode { random } => recovery::code(&random),
        Request::RecoveryRead { text } => recovery::read(&text),
        #[cfg(feature = "recovery-kdf")]
        Request::RecoveryDerive { data } => recovery::derive(&data),
        Request::Retry {
            attempts,
            now,
            retry_after,
        } => {
            // Same capped exponential delay as the outbox, with provider Retry-After as a lower bound.
            let delay = (5_000u64 * (1u64 << attempts.min(10))).min(1_800_000);
            let at = now
                .checked_add(delay)
                .ok_or("time_overflow")?
                .max(retry_after.unwrap_or(0));
            if at > wire::MAX_SAFE_INTEGER {
                return Err("time_overflow".into());
            }
            Ok(json!(at))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{evaluate, evaluate_core, evaluate_legacy};
    use serde_json::{json, Value};

    #[test]
    fn a_request_is_an_object_naming_its_op_anywhere() {
        let answer = |request: &str| -> Value { serde_json::from_str(&evaluate(request)).unwrap() };
        let guard = json!({"version": 1, "ok": {"compact": true}});
        for request in [
            r#"{"op":"compaction_guard","unreadable":1,"rows":2}"#,
            r#"{"unreadable":1,"rows":2,"op":"compaction_guard"}"#,
            " \n{\"rows\":2,\"op\":\"compaction_guard\",\"future\":[{}],\"unreadable\":1}\t",
        ] {
            assert_eq!(answer(request), guard, "{request}");
            assert_eq!(evaluate(request), evaluate_legacy(request), "{request}");
        }
        let refused = json!({"version": 1, "error": "invalid_request"});
        for request in [
            // Serde's internally tagged enums read an array as the op and then its fields in order.
            r#"["compaction_guard",1,2]"#,
            r#"{"op":"compaction_guard","op":"compaction_guard","unreadable":1,"rows":2}"#,
            r#"{"op":"compaction_guard","unreadable":1,"rows":2,"rows":2}"#,
            r#"{"op":"compaction_guard","unreadable":1,"rows":2} {}"#,
            r#"{"op":1,"unreadable":1,"rows":2}"#,
            r#"{"unreadable":1,"rows":2}"#,
            r#"{"op":"compaction_guard","unreadable":1}"#,
            r#"{"op":"no_such_op"}"#,
            "{}",
            "",
        ] {
            assert_eq!(answer(request), refused, "{request}");
            assert_eq!(evaluate(request), evaluate_legacy(request), "{request}");
        }
        assert_eq!(
            evaluate_core(r#"{"op":"assistant_prune","now":1}"#),
            json!({"version": 1, "error": "assistant_module"}).to_string()
        );
    }

    #[test]
    fn compact_boundary_matches_the_typed_boundary() {
        for fixture in [
            include_str!("../tests/fixtures/policy-v1.json"),
            include_str!("../tests/fixtures/policy-v4.json"),
            include_str!("../tests/fixtures/profiles-v1.json"),
        ] {
            let fixture: Value = serde_json::from_str(fixture).unwrap();
            for case in fixture["cases"].as_array().unwrap() {
                let request = case["request"].to_string();
                assert_eq!(
                    evaluate(&request),
                    evaluate_legacy(&request),
                    "{}",
                    case["name"]
                );
            }
        }
    }

    #[test]
    fn whole_library_operations_are_not_limited_to_one_megabyte() {
        // serde ignores the framing field, just as the boundary ignores future request fields. This
        // pins the transport limit without making the policy test construct thousands of real rows.
        let request = json!({
            "op": "compaction_guard",
            "unreadable": 0,
            "rows": 1,
            "framing": "x".repeat(2 * 1024 * 1024),
        });
        let response: Value = serde_json::from_str(&evaluate(&request.to_string())).unwrap();
        assert!(response.get("ok").is_some(), "{response}");
    }

    #[cfg(not(feature = "tracker-delivery"))]
    #[test]
    fn tracker_delivery_ops_compile_out() {
        let invalid = json!({"version": 1, "error": "invalid_request"}).to_string();
        for request in [
            r#"{"op":"commands","event":{},"current":{}}"#,
            r#"{"op":"decide","command":{},"remote":{}}"#,
            r#"{"op":"delivery_write","commands":[],"identity":{}}"#,
            r#"{"op":"pending_targets_v4","documents":[],"deliver":{},"now":0}"#,
            r#"{"op":"settle_v4","outcome":{},"built_from":{},"order":{}}"#,
        ] {
            assert_eq!(evaluate_core(request), invalid, "{request}");
        }
        assert_ne!(
            evaluate_core(r#"{"op":"rank_releases","releases":[],"tried":[]}"#),
            invalid
        );
    }

    #[cfg(not(feature = "profile-admin"))]
    #[test]
    fn profile_admin_ops_compile_out_but_viewer_policy_stays() {
        let invalid = json!({"version": 1, "error": "invalid_request"}).to_string();
        for request in [
            r#"{"op":"profiles_merge","a":{},"b":{}}"#,
            r#"{"op":"profiles_materialize","document":{}}"#,
            r#"{"op":"profiles_create_next","input":{}}"#,
            r#"{"op":"profiles_reset_next","input":{}}"#,
            r#"{"op":"profiles_migration_next","input":{}}"#,
            r#"{"op":"profiles_migration_partition_rows","input":{}}"#,
        ] {
            assert_eq!(evaluate_core(request), invalid, "{request}");
        }
        assert_ne!(
            evaluate_core(r#"{"op":"profile_policy_allows","policy":{"unrestricted":true}}"#),
            invalid
        );
    }
}
