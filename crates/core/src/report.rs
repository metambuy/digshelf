//! Combine playlists, library and match results into a report model that the
//! exporters (and later the GUI) render.

use std::collections::HashMap;

use serde::Serialize;

use crate::matcher::{MatchConfig, Matcher};
use crate::model::{
    DeezerTrack, LocalTrack, MatchMethod, MatchStatus, Playlist, PlaylistSource, ScoreBreakdown,
};
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
}

pub struct ReportOptions<'a> {
    pub match_config: MatchConfig,
    pub qobuz_locale: &'a str,
}

pub fn build_report(
    playlists: &[Playlist],
    library: &[LocalTrack],
    opts: &ReportOptions,
) -> Report {
    let matcher = Matcher::new(library, opts.match_config);
    // The same track often appears in several playlists: match it once.
    let mut memo: HashMap<i64, (MatchStatus, Option<MatchedFile>)> = HashMap::new();

    let mut out = Report {
        playlists: Vec::with_capacity(playlists.len()),
        library_files: library.len(),
        owned: 0,
        uncertain: 0,
        missing: 0,
    };

    for pl in playlists {
        let rows: Vec<TrackRow> = pl
            .tracks
            .iter()
            .enumerate()
            .map(|(position, t)| {
                let (status, matched) = memo
                    .entry(t.id)
                    .or_insert_with(|| {
                        let r = matcher.match_track(t);
                        let matched = r.candidate.map(|c| MatchedFile {
                            file: library[c.local_index].clone(),
                            method: c.method,
                            confidence: c.confidence,
                            scores: c.scores,
                        });
                        (r.status, matched)
                    })
                    .clone();
                TrackRow {
                    position,
                    links: track_links(&t.artist, &t.title, opts.qobuz_locale),
                    track: t.clone(),
                    status,
                    matched,
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
    out
}
