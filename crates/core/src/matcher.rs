//! Match Deezer tracks against local files: ISRC first, then a fuzzy score on
//! normalised artist + title + version + duration. See docs/DESIGN.md.

use std::collections::{HashMap, HashSet};

use strsim::normalized_levenshtein as similarity;

use crate::model::{
    Candidate, DeezerTrack, LocalTrack, MatchMethod, MatchResult, MatchStatus, ScoreBreakdown,
};
use crate::normalize::{normalize, normalize_isrc, split_artists, split_title, TitleParts};

/// Thresholds and tolerances for match decisions.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MatchConfig {
    /// Minimum confidence to auto-accept a fuzzy match whose duration is within tolerance.
    pub owned_threshold: f64,
    /// Minimum confidence to auto-accept when one side has no duration.
    pub owned_threshold_no_duration: f64,
    /// Minimum confidence to report a candidate for review instead of "missing".
    pub uncertain_threshold: f64,
    /// Durations within this many seconds count as equal.
    pub duration_tolerance_secs: u32,
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            owned_threshold: 0.92,
            owned_threshold_no_duration: 0.95,
            uncertain_threshold: 0.70,
            duration_tolerance_secs: 3,
        }
    }
}

pub const W_TITLE: f64 = 0.40;
pub const W_ARTIST: f64 = 0.25;
pub const W_VERSION: f64 = 0.20;
pub const W_DURATION: f64 = 0.15;
const TITLE_GATE: f64 = 0.80;
const ARTIST_GATE: f64 = 0.70;
/// Durations further apart than this score 0.
const DURATION_ZERO_AT_SECS: u32 = 15;

#[derive(Debug, Clone)]
struct Prepared {
    title: TitleParts,
    /// Local files only: a title parsed from the file name when it differs
    /// from the tag (catches mistagged titles such as "Artist - Title…").
    alt_title: Option<TitleParts>,
    artists: Vec<String>,
    artist_full: String,
    isrc: Option<String>,
    duration: Option<u32>,
}

impl Prepared {
    fn new(title: &str, artist: &str, isrc: Option<&str>, duration: Option<u32>) -> Self {
        Self {
            title: split_title(title),
            alt_title: None,
            artists: split_artists(artist),
            artist_full: normalize(artist),
            isrc: isrc.and_then(normalize_isrc),
            duration,
        }
    }

    fn from_local(t: &LocalTrack) -> Self {
        let stem = t
            .path
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        // Drop a leading track number like "01 ", "1-13 " or "01. ".
        let stem = stem
            .trim_start_matches(|c: char| c.is_ascii_digit() || c == '-')
            .trim_start_matches(['.', ' ', '-', '_']);
        // "Artist - Title" (or "Artist - Album - Title"): the last part is the title.
        let (name_artist, name_title) = match stem.split_once(" - ") {
            Some((a, rest)) => (Some(a), rest.rsplit(" - ").next().unwrap_or(rest)),
            None => (None, stem),
        };

        let artist = t.artist.as_deref().or(name_artist).unwrap_or("");
        let mut p = Self::new(
            t.title.as_deref().unwrap_or(name_title),
            artist,
            t.isrc.as_deref(),
            t.duration_secs(),
        );
        if t.title.is_some() {
            let alt = split_title(name_title);
            if !alt.base.is_empty() && alt.base != p.title.base {
                p.alt_title = Some(alt);
            }
        }
        p
    }

    fn titles(&self) -> impl Iterator<Item = &TitleParts> {
        std::iter::once(&self.title).chain(self.alt_title.as_ref())
    }

    fn all_names(&self) -> impl Iterator<Item = &String> {
        self.artists.iter().chain(self.title.featured.iter())
    }
}

/// Matches Deezer tracks against a fixed set of local tracks. Build once per
/// scan; `match_track` is cheap thanks to the ISRC map and title-token index.
pub struct Matcher {
    config: MatchConfig,
    prepared: Vec<Prepared>,
    by_isrc: HashMap<String, Vec<usize>>,
    by_token: HashMap<String, Vec<usize>>,
}

impl Matcher {
    pub fn new(locals: &[LocalTrack], config: MatchConfig) -> Self {
        let prepared: Vec<Prepared> = locals.iter().map(Prepared::from_local).collect();
        let mut by_isrc: HashMap<String, Vec<usize>> = HashMap::new();
        let mut by_token: HashMap<String, Vec<usize>> = HashMap::new();
        for (i, p) in prepared.iter().enumerate() {
            if let Some(isrc) = &p.isrc {
                by_isrc.entry(isrc.clone()).or_default().push(i);
            }
            let tokens: HashSet<&str> = p
                .titles()
                .flat_map(|t| t.base.split(' '))
                .filter(|t| !t.is_empty())
                .collect();
            for tok in tokens {
                by_token.entry(tok.to_string()).or_default().push(i);
            }
        }
        Self {
            config,
            prepared,
            by_isrc,
            by_token,
        }
    }

    pub fn match_track(&self, track: &DeezerTrack) -> MatchResult {
        let dz = Prepared::new(
            &track.title,
            &track.artist,
            track.isrc.as_deref(),
            track.duration,
        );

        if let Some(isrc) = &dz.isrc {
            if let Some(&local_index) = self.by_isrc.get(isrc).and_then(|v| v.first()) {
                return MatchResult {
                    status: MatchStatus::Owned,
                    candidate: Some(Candidate {
                        local_index,
                        method: MatchMethod::Isrc,
                        confidence: 1.0,
                        scores: None,
                    }),
                };
            }
        }

        let best = self
            .candidates(&dz)
            .into_iter()
            .filter_map(|i| {
                let local = &self.prepared[i];
                local
                    .titles()
                    .filter_map(|title| self.score(&dz, local, title))
                    .max_by(|a, b| a.0.total_cmp(&b.0))
                    .map(|s| (i, s))
            })
            .max_by(|a, b| a.1 .0.total_cmp(&b.1 .0));

        let Some((local_index, (confidence, scores))) = best else {
            return MatchResult {
                status: MatchStatus::Missing,
                candidate: None,
            };
        };

        let within_tolerance = scores
            .duration_delta
            .map(|d| d <= self.config.duration_tolerance_secs);
        let owned = match within_tolerance {
            Some(true) => confidence >= self.config.owned_threshold,
            Some(false) => false,
            None => confidence >= self.config.owned_threshold_no_duration,
        };
        let status = if owned {
            MatchStatus::Owned
        } else if confidence >= self.config.uncertain_threshold {
            MatchStatus::Uncertain
        } else {
            MatchStatus::Missing
        };
        MatchResult {
            status,
            candidate: (status != MatchStatus::Missing).then_some(Candidate {
                local_index,
                method: MatchMethod::Fuzzy,
                confidence,
                scores: Some(scores),
            }),
        }
    }

    /// Local tracks sharing one of the two rarest base-title tokens.
    fn candidates(&self, dz: &Prepared) -> Vec<usize> {
        let mut postings: Vec<&Vec<usize>> = dz
            .title
            .base
            .split(' ')
            .filter_map(|t| self.by_token.get(t))
            .collect();
        postings.sort_by_key(|p| p.len());
        let set: HashSet<usize> = postings.into_iter().take(2).flatten().copied().collect();
        let mut out: Vec<usize> = set.into_iter().collect();
        out.sort_unstable(); // deterministic tie-breaking
        out
    }

    fn score(
        &self,
        dz: &Prepared,
        local: &Prepared,
        local_title: &TitleParts,
    ) -> Option<(f64, ScoreBreakdown)> {
        let title = similarity(&dz.title.base, &local_title.base);
        if title < TITLE_GATE {
            return None;
        }
        let artist = artist_score(dz, local);
        if artist < ARTIST_GATE {
            return None;
        }
        let version = version_score(&dz.title.version, &local_title.version);
        let duration_delta = match (dz.duration, local.duration) {
            (Some(a), Some(b)) => Some(a.abs_diff(b)),
            _ => None,
        };
        let duration = match duration_delta {
            None => 0.7,
            Some(d) if d <= self.config.duration_tolerance_secs => 1.0,
            Some(d) if d >= DURATION_ZERO_AT_SECS => 0.0,
            Some(d) => {
                let span = f64::from(DURATION_ZERO_AT_SECS - self.config.duration_tolerance_secs);
                1.0 - f64::from(d - self.config.duration_tolerance_secs) / span
            }
        };
        let confidence =
            W_TITLE * title + W_ARTIST * artist + W_VERSION * version + W_DURATION * duration;
        Some((
            confidence,
            ScoreBreakdown {
                title,
                artist,
                version,
                duration,
                duration_delta,
            },
        ))
    }
}

fn artist_score(a: &Prepared, b: &Prepared) -> f64 {
    if a.artists.is_empty() || b.artists.is_empty() {
        return 0.5;
    }
    let a_all: HashSet<&String> = a.all_names().collect();
    let b_all: HashSet<&String> = b.all_names().collect();
    if a.artists.iter().any(|n| b_all.contains(n)) || b.artists.iter().any(|n| a_all.contains(n)) {
        return 1.0;
    }
    let mut best = similarity(&a.artist_full, &b.artist_full);
    for x in &a.artists {
        for y in &b.artists {
            best = best.max(similarity(x, y));
        }
    }
    best
}

fn version_score(a: &str, b: &str) -> f64 {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => 1.0,
        (false, false) => {
            let s = similarity(a, b);
            if s >= 0.85 {
                1.0
            } else {
                s
            }
        }
        _ => 0.3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dz(title: &str, artist: &str, duration: u32, isrc: Option<&str>) -> DeezerTrack {
        DeezerTrack {
            id: 1,
            title: title.into(),
            artist: artist.into(),
            album_id: None,
            album: "Album".into(),
            duration: Some(duration),
            isrc: isrc.map(Into::into),
            link: String::new(),
        }
    }

    fn local(path: &str, title: Option<&str>, artist: Option<&str>, secs: u64) -> LocalTrack {
        LocalTrack {
            path: PathBuf::from(path),
            title: title.map(Into::into),
            artist: artist.map(Into::into),
            album: None,
            duration_ms: Some(secs * 1000),
            isrc: None,
        }
    }

    fn run(locals: &[LocalTrack], t: &DeezerTrack) -> MatchResult {
        Matcher::new(locals, MatchConfig::default()).match_track(t)
    }

    #[test]
    fn isrc_match_wins_even_with_different_tags() {
        let mut l = local(
            "/m/a.flac",
            Some("Completely Different"),
            Some("Nobody"),
            10,
        );
        l.isrc = Some("xx-aaa-26-00001".into());
        let r = run(
            &[l],
            &dz("Night Drive", "Vela Nine", 300, Some("XXAAA2600001")),
        );
        assert_eq!(r.status, MatchStatus::Owned);
        assert_eq!(r.candidate.unwrap().method, MatchMethod::Isrc);
    }

    #[test]
    fn fuzzy_exact_tags_are_owned() {
        let l = local("/m/a.mp3", Some("Night Drive"), Some("Vela Nine"), 301);
        let r = run(&[l], &dz("Night Drive", "Vela Nine", 300, None));
        assert_eq!(r.status, MatchStatus::Owned);
        assert!(r.candidate.unwrap().confidence > 0.99);
    }

    #[test]
    fn accents_feat_and_remaster_are_ignored() {
        let l = local(
            "/m/a.mp3",
            Some("Café Nuit (feat. Ana Luz)"),
            Some("Élan Vital"),
            245,
        );
        let r = run(
            &[l],
            &dz("Cafe Nuit (Remastered 2015)", "Elan Vital", 244, None),
        );
        assert_eq!(r.status, MatchStatus::Owned);
    }

    #[test]
    fn remix_vs_original_is_uncertain_not_owned() {
        let l = local(
            "/m/a.mp3",
            Some("Low Tide (Kora Blue Remix)"),
            Some("Vela Nine"),
            300,
        );
        let r = run(&[l], &dz("Low Tide", "Vela Nine", 300, None));
        assert_eq!(r.status, MatchStatus::Uncertain);
    }

    #[test]
    fn duration_outside_tolerance_is_uncertain() {
        let l = local("/m/a.mp3", Some("Night Drive"), Some("Vela Nine"), 306);
        let r = run(&[l], &dz("Night Drive", "Vela Nine", 300, None));
        assert_eq!(r.status, MatchStatus::Uncertain);
        let s = r.candidate.unwrap().scores.unwrap();
        assert_eq!(s.duration_delta, Some(6));
    }

    #[test]
    fn different_artist_is_missing() {
        let l = local(
            "/m/a.mp3",
            Some("Night Drive"),
            Some("Completely Other Band"),
            300,
        );
        let r = run(&[l], &dz("Night Drive", "Vela Nine", 300, None));
        assert_eq!(r.status, MatchStatus::Missing);
        assert!(r.candidate.is_none());
    }

    #[test]
    fn untagged_file_uses_filename() {
        let l = local("/m/03 - Vela Nine - Night Drive.mp3", None, None, 300);
        // "03 - " prefix is stripped, then "Artist - Title" is parsed.
        let r = run(&[l], &dz("Night Drive", "Vela Nine", 300, None));
        assert_eq!(r.status, MatchStatus::Owned);
    }

    #[test]
    fn collaboration_credit_matches_primary_artist() {
        let l = local(
            "/m/a.mp3",
            Some("Glass Rooms"),
            Some("Mira Sol & Vela Nine"),
            200,
        );
        let r = run(&[l], &dz("Glass Rooms", "Vela Nine", 200, None));
        assert_eq!(r.status, MatchStatus::Owned);
    }

    #[test]
    fn shared_prefix_is_not_enough() {
        // Regression: Jaro-Winkler's prefix bonus made these look similar.
        let l = local("/m/a.mp3", Some("I Can't Stay"), Some("Vela Nine"), 224);
        let r = run(&[l], &dz("(I Can't Get No) Sleep", "Vela Nine", 224, None));
        assert_eq!(r.status, MatchStatus::Missing);
        let l = local("/m/a.mp3", Some("The Fence"), Some("Kora Blue"), 112);
        let r = run(&[l], &dz("The Lantern", "Kona Blues", 112, None));
        assert_eq!(r.status, MatchStatus::Missing);
    }

    #[test]
    fn cover_by_other_artist_is_missing() {
        let l = local(
            "/m/a.mp3",
            Some("Blue Meadows"),
            Some("Tessa Crane and the Harbour Lights"),
            174,
        );
        let r = run(
            &[l],
            &dz("Blue Meadows", "Ronan V. & the K.L.'s", 174, None),
        );
        assert_eq!(r.status, MatchStatus::Missing);
    }

    #[test]
    fn truncated_tag_title_still_matches() {
        let l = local(
            "/m/a.mp3",
            Some("paper lanterns over the rive"),
            Some("Ana Luz"),
            162,
        );
        let r = run(
            &[l],
            &dz("Paper Lanterns Over the River", "Ana Luz", 162, None),
        );
        assert_eq!(r.status, MatchStatus::Owned);
    }

    #[test]
    fn mistagged_title_falls_back_to_file_name() {
        let l = local(
            "/m/Ana Luz - Tout Va Bien Ce Soir.mp3",
            Some("Ana Luz - Tout Va Bien Ce"),
            Some("Ana Luz"),
            142,
        );
        let r = run(&[l], &dz("Tout va bien ce soir", "Ána Luz", 140, None));
        assert_eq!(r.status, MatchStatus::Owned);
    }

    #[test]
    fn picks_best_of_several_candidates() {
        let locals = [
            local(
                "/m/1.mp3",
                Some("Night Drive (Radio Edit)"),
                Some("Vela Nine"),
                210,
            ),
            local("/m/2.mp3", Some("Night Drive"), Some("Vela Nine"), 300),
            local("/m/3.mp3", Some("Night Moves"), Some("Vela Nine"), 300),
        ];
        let r = run(&locals, &dz("Night Drive", "Vela Nine", 300, None));
        assert_eq!(r.candidate.unwrap().local_index, 1);
    }
}
