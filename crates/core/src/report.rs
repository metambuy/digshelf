//! Combine playlists, library and match results into a report model that the
//! exporters (and later the GUI) render.

use std::collections::{HashMap, HashSet};

use serde::Serialize;

use crate::matcher::{MatchConfig, Matcher};
use crate::model::{
    DeezerTrack, LocalTrack, MatchMethod, MatchStatus, Playlist, PlaylistSource, ScoreBreakdown,
};
use crate::overrides::{Action, OverrideEntry, Overrides, TrackInfo};
use crate::stores::{album_links, track_links, StoreLinks};

/// The local file a Deezer track was matched to.
#[derive(Debug, Clone, Serialize)]
pub struct MatchedFile {
    pub file: LocalTrack,
    pub method: MatchMethod,
    pub confidence: f64,
    pub scores: Option<ScoreBreakdown>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TrackRow {
    /// Position in the Deezer playlist (0-based).
    pub position: usize,
    pub track: DeezerTrack,
    pub status: MatchStatus,
    pub matched: Option<MatchedFile>,
    pub links: StoreLinks,
    /// The user marked this track as not owned (reject without a file).
    pub rejected_by_user: bool,
    /// Ready-to-paste overrides for uncertain rows.
    pub snippets: Option<OverrideSnippets>,
}

/// TOML snippets a user can paste into the overrides file.
#[derive(Debug, Clone, Serialize)]
pub struct OverrideSnippets {
    /// Confirm the proposed file.
    pub accept: String,
    /// Reject the proposed file (another candidate may then win).
    pub reject_file: String,
    /// Mark the track as not owned.
    pub reject_track: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AlbumGroup {
    pub album: String,
    pub artist: String,
    pub links: StoreLinks,
    pub tracks: Vec<TrackRow>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlaylistReport {
    pub id: u64,
    pub title: String,
    pub source: PlaylistSource,
    pub link: String,
    /// Every track in playlist order.
    pub rows: Vec<TrackRow>,
    pub owned: usize,
    pub uncertain: usize,
    pub missing: usize,
    /// Missing tracks grouped by album, in order of first appearance.
    pub missing_albums: Vec<AlbumGroup>,
}

impl PlaylistReport {
    pub fn rows_with(&self, status: MatchStatus) -> impl Iterator<Item = &TrackRow> {
        self.rows.iter().filter(move |r| r.status == status)
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub playlists: Vec<PlaylistReport>,
    pub library_files: usize,
    pub owned: usize,
    pub uncertain: usize,
    pub missing: usize,
    /// Accept/map overrides whose file is no longer in the library (for
    /// tracks in this run). Those tracks were matched automatically instead.
    pub stale_overrides: Vec<OverrideEntry>,
    /// Library files whose tags could not be read.
    pub unreadable: Vec<UnreadableFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UnreadableFile {
    pub path: std::path::PathBuf,
    pub error: String,
}

pub struct ReportOptions<'a> {
    pub match_config: MatchConfig,
    pub qobuz_locale: &'a str,
    pub overrides: &'a Overrides,
    /// From [`crate::library::ScanResult::unreadable`].
    pub unreadable: &'a [(std::path::PathBuf, String)],
}

fn snippets(t: &DeezerTrack, file: &std::path::Path) -> OverrideSnippets {
    let info = TrackInfo {
        artist: Some(t.artist.clone()),
        title: Some(t.title.clone()),
        note: None,
    };
    OverrideSnippets {
        accept: Overrides::snippet(t.id, Action::Accept, Some(file), &info),
        reject_file: Overrides::snippet(t.id, Action::Reject, Some(file), &info),
        reject_track: Overrides::snippet(t.id, Action::Reject, None, &info),
    }
}

pub fn build_report(
    playlists: &[Playlist],
    library: &[LocalTrack],
    opts: &ReportOptions,
) -> Report {
    let matcher = Matcher::new(library, opts.match_config);
    // The same track often appears in several playlists: match it once.
    let mut memo: HashMap<i64, (MatchStatus, Option<MatchedFile>, bool)> = HashMap::new();

    let mut out = Report {
        playlists: Vec::with_capacity(playlists.len()),
        library_files: library.len(),
        owned: 0,
        uncertain: 0,
        missing: 0,
        stale_overrides: Vec::new(),
        unreadable: opts
            .unreadable
            .iter()
            .map(|(path, error)| UnreadableFile {
                path: path.clone(),
                error: error.clone(),
            })
            .collect(),
    };

    for pl in playlists {
        let rows: Vec<TrackRow> = pl
            .tracks
            .iter()
            .enumerate()
            .map(|(position, t)| {
                let (status, matched, rejected_by_user) = memo
                    .entry(t.id)
                    .or_insert_with(|| {
                        let ov = opts.overrides.for_track(t.id);
                        let r = matcher.match_track_with(t, &ov);
                        let rejected = ov.reject_all && r.candidate.is_none();
                        let matched = r.candidate.map(|c| MatchedFile {
                            file: library[c.local_index].clone(),
                            method: c.method,
                            confidence: c.confidence,
                            scores: c.scores,
                        });
                        (r.status, matched, rejected)
                    })
                    .clone();
                let snippets = match (&status, &matched) {
                    (MatchStatus::Uncertain, Some(m)) => Some(snippets(t, &m.file.path)),
                    _ => None,
                };
                TrackRow {
                    position,
                    links: track_links(&t.artist, &t.title, opts.qobuz_locale),
                    track: t.clone(),
                    status,
                    matched,
                    rejected_by_user,
                    snippets,
                }
            })
            .collect();

        let count = |s| rows.iter().filter(|r| r.status == s).count();
        let (owned, uncertain, missing) = (
            count(MatchStatus::Owned),
            count(MatchStatus::Uncertain),
            count(MatchStatus::Missing),
        );
        out.owned += owned;
        out.uncertain += uncertain;
        out.missing += missing;

        let mut albums: Vec<AlbumGroup> = Vec::new();
        let mut album_index: HashMap<String, usize> = HashMap::new();
        for row in rows.iter().filter(|r| r.status == MatchStatus::Missing) {
            let key = match row.track.album_id {
                Some(id) if id > 0 => id.to_string(),
                _ => format!("{}\u{1}{}", row.track.artist, row.track.album),
            };
            let idx = *album_index.entry(key).or_insert_with(|| {
                albums.push(AlbumGroup {
                    album: row.track.album.clone(),
                    artist: row.track.artist.clone(),
                    links: album_links(&row.track.artist, &row.track.album, opts.qobuz_locale),
                    tracks: Vec::new(),
                });
                albums.len() - 1
            });
            albums[idx].tracks.push(row.clone());
        }

        out.playlists.push(PlaylistReport {
            id: pl.id,
            title: pl.title.clone(),
            source: pl.source,
            link: pl.link.clone(),
            rows,
            owned,
            uncertain,
            missing,
            missing_albums: albums,
        });
    }

    let seen: HashSet<i64> = memo.into_keys().collect();
    out.stale_overrides = opts
        .overrides
        .entries()
        .iter()
        .filter(|e| matches!(e.action, Action::Accept | Action::Map) && seen.contains(&e.deezer_id))
        .filter(|e| {
            e.file
                .as_deref()
                .is_some_and(|f| matcher.index_of(f).is_none())
        })
        .cloned()
        .collect();
    out
}
