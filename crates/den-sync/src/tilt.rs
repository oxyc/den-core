//! The taste tilt: how a household's taste reorders an already-loaded page of titles.
//!
//! This is the COMPOSITION only — the arithmetic that turns per-candidate signals into a new order. The
//! expensive half, the cosine of a title against the household's centroid, is not here and never will be:
//! it needs the 48 MB vector space, which is why it lives in den-atlas (`POST /index/score`).
//!
//! # Why the split is exactly here
//!
//! The cosine is **not a lever**. `cos(title, centroid)` is a fixed property of the data; nobody tunes it.
//! The weights are the entire set of levers, and they are three multiplications and a Gaussian. Putting
//! the levers in this crate means:
//!
//! - the tvOS app and Den Web run the SAME arithmetic rather than each carrying a copy of 0.15 / 0.35 /
//!   0.10, the dislike squaring and the era curve — the drift this crate exists to prevent;
//! - the Phase 5 tuning playground gets real sliders over real production code for free, because
//!   `bindings/web` already compiles this to WASM. The cosines are a fixed input, the weights move, and
//!   the reorder is instant and local with no round trip per slider.
//!
//! # The weights are INPUTS
//!
//! Every constant below is a default, not a fact. A scorer with its weights baked in cannot be tuned
//! without a release, and a tuner that cannot reach the real weights is a toy. `Weights::default()` is
//! what ships; the playground sends its own.
//!
//! # What the tilt may and may not do
//!
//! It only ever REORDERS. It never fetches, filters or drops. That invariant is what lets an infinitely
//! scrolling row work at all: a page that came back with 24 titles still has 24 titles, in a different
//! order, so paging state stays valid. Anything that removes a title belongs in the hide rules, not here.

use serde::{Deserialize, Serialize};

/// How hard each signal pulls, as a fraction of the page's own rank span.
///
/// The baseline a boost competes against is `1 - i/n` over the page, so a term of weight `w` can move a
/// title about `w * n` slots — on a 24-card page, the shipped weights are worth about six. They are
/// deliberately gentle: the tilt is a nudge, and what a row is ABOUT still leads.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Weights {
    /// The liked centroid — theme, tone and type. Gentle: taste informs the order, it does not choose it.
    pub embedding: f64,
    /// Bigger than `embedding` because a dislike is a deliberate act where a like is often passive.
    /// "Not interested" that moves a title two slots reads as broken.
    pub dislike: f64,
    /// Release-year affinity. Orthogonal to the embedding, which is content-based and year-blind.
    pub era: f64,
    /// Whether the dislike penalty is squared. It is, and that is what makes a large weight safe:
    /// squaring concentrates the penalty where the signal actually is. A near-duplicate of a rejected
    /// title takes almost the full weight (cos 0.9 → 0.81) while a loosely-related one in the same genre
    /// is barely touched (0.4 → 0.16). Linear at this weight would spread one tap across a whole genre,
    /// because the score is taken against the disliked CENTROID — with a single dislike on file, that is
    /// just that title's neighbourhood.
    pub square_dislike: bool,
}

impl Default for Weights {
    fn default() -> Self {
        Weights {
            embedding: 0.15,
            dislike: 0.35,
            era: 0.10,
            square_dislike: true,
        }
    }
}

/// The household's era preference: a Gaussian over release years, learned from what they watch.
///
/// `center` is the weighted-average year, `spread` how era-focused they are — tight means a strong pull to
/// one era, wide means they watch across eras so the pull is weak. Time is orthogonal to the embedding
/// space, which is content-based and year-blind, so this adds a genuinely separate dimension rather than
/// re-weighting one that is already there.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Era {
    pub center: f64,
    pub spread: f64,
}

impl Era {
    /// Affinity for a year in (0, 1] — 1 at the centre, decaying with distance. An unknown year is 0: the
    /// title is simply not lifted by this term, NOT demoted. A missing fact is not a negative.
    pub fn boost(&self, year: Option<i32>) -> f64 {
        let Some(year) = year else { return 0.0 };
        if self.spread <= 0.0 {
            return 0.0;
        }
        let d = f64::from(year) - self.center;
        (-(d * d) / (2.0 * self.spread * self.spread)).exp()
    }

    /// Build from (year, weight) samples — the years of positively-engaged titles, weighted by engagement.
    ///
    /// Empty gives a recency prior centred on the current year, which is the "recent by default" behaviour
    /// a household with no history should get. The spread is clamped so a one-title library is not
    /// razor-thin and a hugely varied one is not completely flat.
    pub fn from_samples(samples: &[(i32, f64)], current_year: i32) -> Era {
        let total: f64 = samples.iter().map(|(_, w)| w).sum();
        if total <= 0.0 {
            return Era {
                center: f64::from(current_year),
                spread: 15.0,
            };
        }
        let mean = samples.iter().map(|(y, w)| f64::from(*y) * w).sum::<f64>() / total;
        let variance = samples
            .iter()
            .map(|(y, w)| w * (f64::from(*y) - mean).powi(2))
            .sum::<f64>()
            / total;
        Era {
            center: mean,
            spread: variance.sqrt().clamp(8.0, 25.0),
        }
    }
}

/// One candidate's signals: what den-atlas measured, plus the year the client already has.
///
/// `taste` and `dislike` are cosines clamped at 0 — `POST /index/score` answers exactly this pair, and
/// answers 0 for a title the corpus does not hold. A title atlas has never seen is therefore not lifted
/// and not demoted; it keeps its incoming rank, which is the same thing the on-device tilt did.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Signals {
    pub taste: f64,
    pub dislike: f64,
    pub year: Option<i32>,
}

/// The composite boost for one candidate: negative demotes, positive lifts, 0 keeps its incoming rank.
///
/// `include_era` is false for a row already fixed to an era — a decade row — where the term would be
/// redundant and would just compress the order toward the middle of that decade.
pub fn boost(signals: &Signals, weights: &Weights, era: &Era, include_era: bool) -> f64 {
    let dislike = signals.dislike.max(0.0);
    let penalty = if weights.square_dislike {
        dislike * dislike
    } else {
        dislike
    };
    let mut total = weights.embedding * signals.taste.max(0.0) - weights.dislike * penalty;
    if include_era {
        total += weights.era * era.boost(signals.year);
    }
    total
}

/// The order `items` should be shown in, as indices into the input.
///
/// Indices rather than the items themselves so a caller keeps whatever type it is holding — a `MediaItem`
/// on tvOS, a `Title` in the browser — and so this crate never needs to know what a title is.
///
/// The baseline is the INCOMING rank, `1 - i/n`, so a candidate with no signal at all keeps its place and
/// the source's own ordering still leads. Ties break on the incoming index, which keeps the result stable
/// across pages: the same inputs always give the same order.
pub fn order(signals: &[Signals], weights: &Weights, era: &Era, include_era: bool) -> Vec<usize> {
    let n = signals.len();
    if n < 2 {
        return (0..n).collect();
    }
    let mut scored: Vec<(usize, f64)> = signals
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let baseline = 1.0 - (i as f64) / (n as f64);
            (i, baseline + boost(s, weights, era, include_era))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.0.cmp(&b.0))
    });
    scored.into_iter().map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn era() -> Era {
        Era {
            center: 2010.0,
            spread: 10.0,
        }
    }

    #[test]
    fn a_candidate_with_no_signal_keeps_its_place() {
        let signals = vec![Signals::default(); 5];
        assert_eq!(
            order(&signals, &Weights::default(), &era(), true),
            vec![0, 1, 2, 3, 4]
        );
    }

    #[test]
    fn the_tilt_only_ever_reorders() {
        // The invariant infinite scroll depends on: 24 in, 24 out, every index exactly once.
        let signals: Vec<Signals> = (0..24)
            .map(|i| Signals {
                taste: f64::from(i) / 24.0,
                dislike: 0.0,
                year: Some(2000 + i),
            })
            .collect();
        let got = order(&signals, &Weights::default(), &era(), true);
        assert_eq!(got.len(), 24);
        let mut seen = got.clone();
        seen.sort_unstable();
        assert_eq!(seen, (0..24).collect::<Vec<_>>());
    }

    #[test]
    fn a_strongly_liked_title_at_the_back_moves_forward() {
        let mut signals = vec![Signals::default(); 24];
        signals[23].taste = 1.0;
        let got = order(&signals, &Weights::default(), &era(), true);
        // 0.15 over a 24-item page is worth about 3.6 slots, so it climbs but does not take the lead.
        let moved = got.iter().position(|&i| i == 23).unwrap();
        assert!(moved < 23, "it moved forward: {moved}");
        assert!(
            moved > 0,
            "a gentle weight must not let one signal take the lead: {moved}"
        );
    }

    #[test]
    fn a_dislike_pushes_a_title_below_its_incoming_rank() {
        let mut signals = vec![Signals::default(); 10];
        signals[0].dislike = 1.0;
        let got = order(&signals, &Weights::default(), &era(), true);
        assert_ne!(got[0], 0, "the disliked title no longer leads");
        // 0.35 over a 10-item page is worth about 3.5 slots, so it drops by that much and no further.
        // It does NOT go to the back: the tilt is a reorder, and burying a title outright is a hide rule's
        // job, not this one's.
        let moved = got.iter().position(|&i| i == 0).unwrap();
        assert!(
            (3..=4).contains(&moved),
            "dropped about 3.5 slots, not to the back: {moved}"
        );
    }

    #[test]
    fn squaring_concentrates_the_penalty_where_the_signal_is() {
        let w = Weights::default();
        let linear = Weights {
            square_dislike: false,
            ..w
        };
        let near = Signals {
            taste: 0.0,
            dislike: 0.9,
            year: None,
        };
        let loose = Signals {
            taste: 0.0,
            dislike: 0.4,
            year: None,
        };
        // What share of its linear penalty each keeps once squared. A near-duplicate of a rejection keeps
        // almost all of it; a loosely-related title in the same genre keeps well under half. That gap is
        // the whole reason a 0.35 weight is safe — one tap suppresses a neighbourhood, not a genre.
        let kept = |s: &Signals| boost(s, &w, &era(), false) / boost(s, &linear, &era(), false);
        assert!(
            kept(&near) > 0.85,
            "a near-duplicate keeps its penalty: {}",
            kept(&near)
        );
        assert!(
            kept(&loose) < 0.5,
            "a loosely-related one mostly escapes it: {}",
            kept(&loose)
        );
    }

    #[test]
    fn an_unknown_year_is_not_demoted() {
        let known = Signals {
            taste: 0.0,
            dislike: 0.0,
            year: Some(2010),
        };
        let unknown = Signals {
            taste: 0.0,
            dislike: 0.0,
            year: None,
        };
        assert!(boost(&known, &Weights::default(), &era(), true) > 0.0);
        assert_eq!(
            boost(&unknown, &Weights::default(), &era(), true),
            0.0,
            "absent is not negative"
        );
    }

    #[test]
    fn an_era_scoped_row_drops_the_era_term() {
        let s = Signals {
            taste: 0.0,
            dislike: 0.0,
            year: Some(2010),
        };
        assert_eq!(boost(&s, &Weights::default(), &era(), false), 0.0);
    }

    #[test]
    fn an_empty_library_centres_the_era_on_now() {
        let e = Era::from_samples(&[], 2026);
        assert_eq!(e.center, 2026.0);
        assert!(
            e.boost(Some(2026)) > e.boost(Some(1990)),
            "recent by default"
        );
    }

    #[test]
    fn the_era_spread_is_clamped_at_both_ends() {
        let one = Era::from_samples(&[(1999, 3.0)], 2026);
        assert_eq!(one.spread, 8.0, "a one-title library is not razor-thin");
        let varied = Era::from_samples(&[(1930, 1.0), (2026, 1.0)], 2026);
        assert_eq!(varied.spread, 25.0, "…and a hugely varied one is not flat");
    }

    #[test]
    fn the_weights_are_inputs_so_a_tuner_can_move_them() {
        let mut signals = vec![Signals::default(); 24];
        signals[23].taste = 1.0;
        let shipped = order(&signals, &Weights::default(), &era(), true);
        let loud = Weights {
            embedding: 2.0,
            ..Weights::default()
        };
        let tuned = order(&signals, &loud, &era(), true);
        assert_ne!(shipped, tuned);
        assert_eq!(tuned[0], 23, "a big enough weight does take the lead");
    }

    #[test]
    fn ties_break_on_the_incoming_order_so_the_result_is_stable() {
        let signals = vec![
            Signals {
                taste: 0.5,
                dislike: 0.0,
                year: Some(2010)
            };
            6
        ];
        assert_eq!(
            order(&signals, &Weights::default(), &era(), true),
            vec![0, 1, 2, 3, 4, 5]
        );
    }
}
