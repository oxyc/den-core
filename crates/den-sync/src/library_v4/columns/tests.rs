use super::*;
use crate::library_v4::{readable, state};

const NOW: i64 = 4_000;
const DAY: i64 = 86_400_000;

/// Every reading the typed registers make a decision on.
pub(crate) fn documents() -> Vec<Value> {
    let d = "a1b2c3d4e5f60718";
    let future = NOW + 2 * DAY;
    vec![
        // policy-v4's season projection case: resets on both levels, imported plays, cleared plays, a future stamp,
        // extension keys that are no episodes.
        json!({"format":4,"kind":"title","title":{"id":1399,"type":"tv"},"episodesReset":[2000,0,d]}),
        json!({"episodes":{"01":{"futureExtension":true},
            "1":{"cleared":null,"imported":false,"plays":{"0":1000},"progress":{"at":[1000,0,d],"value":1,"viewing":0}},
            "100000":{"futureExtension":true},
            "2":{"cleared":null,"imported":true,"plays":{"-9007199254737992":3000}},
            "3":{"cleared":[0,[2500,0,d]],"imported":false,"plays":{"0":1000,"1":3000},
                 "progress":{"at":[3000,0,d],"value":0.4,"viewing":1,"seconds":120,"playing":false}},
            "4":{"cleared":null,"imported":false,"plays":{},"progress":{"at":[864000000,0,d],"value":1,"viewing":4}},
            "5":{}},
            "format":4,"kind":"season","season":1,"seasonReset":[1500,0,d],"title":{"id":1399,"type":"tv"}}),
        json!({"episodes":{
            "1":{"cleared":null,"imported":true,"plays":{}},
            "2":{"cleared":null,"imported":false,"plays":{"0":3500,"1":3500},"progress":{"at":[3500,0,d],"value":1,"viewing":1}},
            "3":{"cleared":null,"imported":false,"plays":{},"progress":{"at":[3600,0,""],"value":0.01,"viewing":0}},
            "10":{"cleared":null,"imported":false,"plays":{"0":future},"progress":{"at":[3700,0,d],"value":1,"viewing":0}}},
            "format":4,"kind":"season","season":0,"seasonReset":null,"title":{"id":1399,"type":"tv"}}),
        // A series with no title document.
        json!({"episodes":{"7":{"cleared":null,"imported":false,"plays":{"0":10},"progress":{"at":[10,0,d],"value":1,"viewing":0}}},
            "format":4,"kind":"season","season":2,"seasonReset":null,"title":{"id":7,"type":"tv"}}),
        // Future stamps on every title register, and a future reset.
        json!({"format":4,"kind":"title","title":{"id":5,"type":"movie"},
            "status":{"value":"inProgress","at":[future,0,d]},
            "resume":{"value":0.5,"at":[future,0,d],"viewing":2,"seconds":30,"playing":true},
            "reaction":{"value":"love","at":[future,0,d]},
            "deleted":{"value":false,"at":[future,0,d]},
            "dismissed":{"value":true,"at":[future,0,d]},
            "episodesReset":[future,0,d],"addedAt":12,"watchedAt":null}),
        json!({"format":4,"kind":"title","title":{"id":6,"type":"movie"},
            "status":{"value":"watched","at":[300,0,d]},
            "resume":{"value":0.3,"at":[250,0,d],"viewing":0,"seconds":30},
            "reaction":{"value":"dislike","at":[200,0,d]},
            "deleted":{"value":true,"at":[400,0,d]},
            "dismissed":{"value":true,"at":[100,0,d]},
            "watch":{"cleared":[0,[150,0,d]],"imported":false,"plays":{"0":120,"1":290,"-9007199254740991":5}},
            "addedAt":10,"watchedAt":90}),
        json!({"format":4,"kind":"title","title":{"id":8,"type":"tv"}}),
    ]
}

fn index(documents: &[Value]) -> Columns {
    let mut columns = Columns::new();
    columns
        .upsert_text(&Value::Array(documents.to_vec()).to_string())
        .unwrap();
    columns
}

fn doc(slot: &Slot) -> Map<String, Value> {
    readable(&serde_json::from_str(slot.text.get()).unwrap()).unwrap()
}

#[test]
fn typed_registers_derive_as_the_rules_do() {
    let columns = index(&documents());
    for now in [NOW, 864_000_000 - DAY, 864_000_000 + 1, NOW + 2 * DAY] {
        // Titles against `title_state`.
        let facts = columns.titles(now);
        let docs: Vec<Map<String, Value>> = columns
            .held()
            .filter(|s| matches!(s.body, Body::Title(_)))
            .map(doc)
            .collect();
        assert_eq!(docs.len(), facts.len());
        let strings = &columns.strings;
        let t = |v: &Value| v.get(0).and_then(Value::as_i64).unwrap_or(0);
        for (d, f) in docs.iter().zip(&facts) {
            let s = state::title_state(d, now);
            let key = f.code();
            assert_eq!(
                strings.get(f.status),
                s["status"]["value"].as_str().unwrap_or("none"),
                "{key}"
            );
            assert_eq!(f.status_at, t(&s["status"]["at"]), "{key}");
            assert_eq!(
                f.progress,
                s["resume"]["value"].as_f64().unwrap_or(0.0),
                "{key}"
            );
            assert_eq!(f.progress_at, t(&s["resume"]["at"]), "{key}");
            assert_eq!(
                strings.get(f.by),
                s["resume"]["at"][2].as_str().unwrap_or(""),
                "{key}"
            );
            assert_eq!(f.seconds, s["resume"]["seconds"].as_f64());
            assert_eq!(f.playing, s["resume"]["playing"].as_bool());
            assert_eq!(f.deleted, s["deleted"]["value"] == true);
            assert_eq!(f.deleted_at, t(&s["deleted"]["at"]));
            assert_eq!(
                f.dismissed,
                (s["dismissed"]["value"] == true).then(|| t(&s["dismissed"]["at"]))
            );
            assert_eq!(
                strings.get(f.reaction),
                s["reaction"]["value"].as_str().unwrap_or("")
            );
            assert_eq!(f.reaction_at, t(&s["reaction"]["at"]));
            assert_eq!(
                f.reset,
                s["episodesReset"]
                    .is_array()
                    .then(|| t(&s["episodesReset"])),
                "{key}"
            );
            assert_eq!(f.added_at, s["addedAt"].as_f64().unwrap_or(0.0));
            assert_eq!(f.doc.watched_at, s["watchedAt"].as_f64());
            // A film's plays against `film_state` (the assistant's Seen rows).
            if !f.doc.tv {
                let film = state::film_state(d, now).unwrap();
                let status = f
                    .doc
                    .status
                    .as_ref()
                    .map_or("none", |s| strings.get(s.value));
                let typed = match (&f.doc.watch, status) {
                    (_, "none" | "watchlist") | (None, _) => vec![],
                    (Some(w), _) => derive::plays(w, 0, now),
                };
                let ruled: Vec<(i64, u64)> = film["plays"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|p| (p[0].as_i64().unwrap(), p[1].as_u64().unwrap()))
                    .collect();
                assert_eq!(typed, ruled, "{key} at {now}");
            }
        }
        // Episodes against `season_state`, through den-edge's `episodeRow`.
        let mut typed = Vec::new();
        columns.rows(now, |id, season, episode, r| {
            typed.push((
                id,
                season,
                episode,
                r.value,
                r.at,
                strings.get(r.by).to_owned(),
                r.seconds,
                r.playing,
            ))
        });
        let mut ruled = Vec::new();
        for slot in columns.held() {
            let Body::Season(s) = &slot.body else {
                continue;
            };
            let title = columns
                .held()
                .find(|t| t.name == format!("title:tv:{}", s.id))
                .map(doc);
            let states = state::season_state(title.as_ref(), Some(&doc(slot)), now).unwrap();
            let mut numbered: Vec<(u64, &Value)> = states
                .as_object()
                .unwrap()
                .iter()
                .map(|(k, v)| (k.parse().unwrap(), v))
                .collect();
            numbered.sort_by_key(|(n, _)| *n);
            for (episode, st) in numbered {
                let row = match st["resume"].as_object() {
                    Some(r) => (
                        r["value"].as_f64().unwrap(),
                        r["at"][0].as_i64().unwrap(),
                        r["at"][2].as_str().unwrap().to_owned(),
                        r.get("seconds").and_then(Value::as_f64),
                        r.get("playing").and_then(Value::as_bool),
                    ),
                    None => (
                        if st["watched"] == true { 1.0 } else { 0.0 },
                        st["watched_at"].as_i64().unwrap_or(0),
                        String::new(),
                        None,
                        None,
                    ),
                };
                ruled.push((s.id, s.season, episode, row.0, row.1, row.2, row.3, row.4));
            }
        }
        assert!(ruled.len() >= 9);
        assert_eq!(typed, ruled, "at {now}");
    }
}

#[test]
fn key_cmp_is_the_byte_order_of_the_keys() {
    let keys = [
        (false, 9),
        (false, 10),
        (false, 100),
        (true, 2),
        (true, 19),
        (true, 1),
    ];
    for a in keys {
        for b in keys {
            let text = |(tv, id): (bool, u64)| format!("{}:{id}", if tv { "tv" } else { "movie" });
            assert_eq!(
                key_cmp(code(a.0, a.1), code(b.0, b.1)),
                text(a).cmp(&text(b)),
                "{a:?} {b:?}"
            );
        }
    }
}

#[test]
fn a_document_that_cannot_be_read_refuses_its_batch() {
    let mut columns = index(&documents());
    let held = columns.len();
    let bad = json!([{"format":4,"kind":"title","title":{"id":9,"type":"movie"}}, {"format":3,"kind":"title"}]);
    assert!(columns.upsert_text(&bad.to_string()).is_err());
    assert_eq!(columns.len(), held, "nothing of the batch is held");
    assert!(columns.document("title:movie:9").is_none());
}

#[test]
fn a_removed_place_is_reused_by_a_compacted_index() {
    let mut columns = Columns::new();
    let film = |id: u64| json!({"format":4,"kind":"title","title":{"id":id,"type":"movie"}});
    columns
        .upsert_text(&Value::Array((1..=200).map(film).collect()).to_string())
        .unwrap();
    for id in 1..=150 {
        assert!(columns.remove(&format!("title:movie:{id}")));
    }
    assert_eq!(columns.len(), 50);
    assert!(columns.slots.len() < 200, "compacted");
    assert!(columns.document("title:movie:200").is_some());
    assert!(!columns.remove("title:movie:3"));
}
