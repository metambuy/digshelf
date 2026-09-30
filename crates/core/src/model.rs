use std::path::PathBuf;

use serde::Serialize;

/// A track as described by the public Deezer API.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeezerTrack {
    /// Negative for tracks a user uploaded themselves (not purchasable).
    pub id: i64,
    pub title: String,
    pub artist: String,
    pub album_id: Option<u64>,
    pub album: String,
    /// Duration in seconds, as reported by Deezer.
    pub duration: Option<u32>,
    pub isrc: Option<String>,
    pub link: String,
    /// Other credited artists (featured, co-main), from `/track/{id}` when
    /// available. Excludes `artist`.
    pub contributors: Vec<String>,
}

/// Where a playlist came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PlaylistSource {
    Playlist,
    Favourites,
}

/// An ordered list of Deezer tracks (a playlist or a user's favourites).
#[derive(Debug, Clone, Serialize)]
pub struct Playlist {
    pub id: u64,
    pub title: String,
    pub source: PlaylistSource,
    pub link: String,
    pub tracks: Vec<DeezerTrack>,
}

/// An audio file found in the local library. Read-only snapshot of its tags.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalTrack {
    pub path: PathBuf,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub duration_ms: Option<u64>,
    pub isrc: Option<String>,
}

impl LocalTrack {
    pub fn duration_secs(&self) -> Option<u32> {
        self.duration_ms.map(|ms| ((ms + 500) / 1000) as u32)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchStatus {
    Owned,
    Uncertain,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MatchMethod {
    Isrc,
    Fuzzy,
    /// Accepted or mapped by the user in the overrides file.
    Manual,
}

/// Per-component similarity scores, kept so a human can see why a match was
/// (or was not) accepted.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct ScoreBreakdown {
    pub title: f64,
    pub artist: f64,
    pub version: f64,
    pub duration: f64,
    /// Absolute duration difference in seconds, when both sides know it.
    pub duration_delta: Option<u32>,
}

/// The best local candidate for a Deezer track.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Candidate {
    /// Index into the local track slice passed to the matcher.
    pub local_index: usize,
    pub method: MatchMethod,
    pub confidence: f64,
    pub scores: Option<ScoreBreakdown>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MatchResult {
    pub status: MatchStatus,
    pub candidate: Option<Candidate>,
}

/// Progress events for long operations, so front ends can report status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    FetchingPlaylist {
        id: u64,
    },
    FetchingUser {
        id: u64,
    },
    /// A user playlist that could not be fetched (e.g. private); others continue.
    SkippedPlaylist {
        title: String,
        error: String,
    },
    FetchedPlaylist {
        title: String,
        tracks: usize,
    },
    FetchingIsrc {
        done: usize,
        total: usize,
    },
    ScanningLibrary {
        files_seen: usize,
    },
    ScannedLibrary {
        files: usize,
        read: usize,
        cached: usize,
        failed: usize,
    },
}
