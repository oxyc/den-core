//! The audio track a playback starts on: one rule for the Apple TV and iPhone players (through `evaluate`) and for
//! Hearth's Remux and Scout, which link the crate.

use crate::downloads::canonical;
use serde::Deserialize;
use serde_json::{json, Value};
use std::cmp::Reverse;

/// What the rule reads of one audio track. A caller that does not know a fact leaves it out.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct AudioTrack {
    /// The container's tag, in any of its spellings; absent, empty or `und` for an unlabelled track.
    #[serde(default)]
    pub language: Option<String>,
    /// Flagged default by the container.
    #[serde(default)]
    pub default: bool,
    #[serde(default)]
    pub commentary: bool,
    #[serde(default)]
    pub channels: u32,
    #[serde(default)]
    pub atmos: bool,
}

/// Why `audio_pick` chose its track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AudioReason {
    /// In the language already being heard this session.
    Kept,
    /// In a wanted language, the first of them a track is in.
    Wanted,
    /// No track is in a wanted language, and this unlabelled track was taken over one labelled with an unwanted
    /// language.
    Unlabelled,
    /// No wanted language decided it: the container's default-flagged track.
    Default,
    /// No wanted language decided it and nothing is flagged default: the first track.
    First,
}

impl AudioReason {
    pub fn as_str(self) -> &'static str {
        match self {
            AudioReason::Kept => "kept",
            AudioReason::Wanted => "wanted",
            AudioReason::Unlabelled => "unlabelled",
            AudioReason::Default => "default",
            AudioReason::First => "first",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AudioPick {
    /// The position in the tracks the caller passed.
    pub index: usize,
    pub reason: AudioReason,
}

fn labelled(language: &str) -> bool {
    !language.is_empty() && language != "und"
}

/// The audio track to play, or `None` for no tracks.
///
/// The languages tried, in order: `kept` (the one already being heard this session), then `wanted` (the profile's
/// preferred language, else the title's original; a browser passes its whole list). Among the tracks in the first of
/// them any track is in, never a commentary: the best main mix (Atmos, then the most channels, so the surround mix
/// over its stereo downmix), then the default-flagged one, then the first.
///
/// With no track in any of them, a track labelled with another language goes last: an unlabelled track is more often
/// the original than a tagged dub is. Then the default-flagged one, then the first, never a commentary while there is
/// another. Every track a commentary: the first.
pub fn audio_pick(
    tracks: &[AudioTrack],
    kept: Option<&str>,
    wanted: &[String],
) -> Option<AudioPick> {
    if tracks.is_empty() {
        return None;
    }
    let languages: Vec<Option<String>> = tracks
        .iter()
        .map(|t| t.language.as_deref().map(canonical).filter(|l| labelled(l)))
        .collect();
    let mut targets: Vec<(String, AudioReason)> = Vec::new();
    let asked = kept
        .map(|k| (k, AudioReason::Kept))
        .into_iter()
        .chain(wanted.iter().map(|w| (w.as_str(), AudioReason::Wanted)));
    for (language, reason) in asked {
        let language = canonical(language);
        if labelled(&language) && !targets.iter().any(|(t, _)| *t == language) {
            targets.push((language, reason));
        }
    }
    let main = |i: &usize| !tracks[*i].commentary;
    for (target, reason) in &targets {
        let best = (0..tracks.len())
            .filter(main)
            .filter(|&i| languages[i].as_deref() == Some(target.as_str()))
            .min_by_key(|&i| {
                let t = &tracks[i];
                (Reverse(t.atmos), Reverse(t.channels), !t.default, i)
            });
        if let Some(index) = best {
            return Some(AudioPick {
                index,
                reason: *reason,
            });
        }
    }
    let unwanted = |i: usize| !targets.is_empty() && languages[i].is_some();
    let Some(index) = (0..tracks.len())
        .filter(main)
        .min_by_key(|&i| (unwanted(i), !tracks[i].default, i))
    else {
        return Some(AudioPick {
            index: 0,
            reason: AudioReason::First,
        });
    };
    let reason = if !unwanted(index) && (0..tracks.len()).filter(main).any(unwanted) {
        AudioReason::Unlabelled
    } else if tracks[index].default {
        AudioReason::Default
    } else {
        AudioReason::First
    };
    Some(AudioPick { index, reason })
}

/// `audio_pick` as the JSON boundary answers it: `{index, reason}`, or `null` for no tracks.
pub(crate) fn answer(tracks: &[AudioTrack], kept: Option<&str>, wanted: &[String]) -> Value {
    match audio_pick(tracks, kept, wanted) {
        Some(pick) => json!({"index": pick.index, "reason": pick.reason.as_str()}),
        None => Value::Null,
    }
}
