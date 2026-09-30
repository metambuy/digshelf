//! Client for the **public, unauthenticated** Deezer API (api.deezer.com).
//!
//! Only JSON metadata endpoints are used. No tokens, cookies, private/gateway
//! APIs or audio. `preview` URLs in responses are ignored.

use std::time::Duration;

use serde::Deserialize;

use crate::cache::Cache;
use crate::error::{Error, Result};
use crate::http::Transport;
use crate::model::{DeezerTrack, Playlist, PlaylistSource, Progress};

pub const API_BASE: &str = "https://api.deezer.com";

/// Deezer's error code for "Quota limit exceeded".
const QUOTA_EXCEEDED: i64 = 4;
/// Safety net against pagination loops.
const MAX_PAGES: usize = 500;
const PAGE_SIZE: u32 = 100;

/// Parse a playlist URL (`https://www.deezer.com/en/playlist/123?utm=...`)
/// or a bare numeric ID.
pub fn parse_playlist_id(input: &str) -> Result<u64> {
    parse_id(input, "playlist")
}

/// Parse a user profile URL (`https://www.deezer.com/en/profile/123`) or a bare numeric ID.
pub fn parse_user_id(input: &str) -> Result<u64> {
    parse_id(input, "profile")
}

fn parse_id(input: &str, segment: &str) -> Result<u64> {
    let s = input.trim();
    let invalid = || Error::InvalidInput {
        input: input.to_string(),
    };
    if let Ok(id) = s.parse() {
        return Ok(id);
    }
    let url = reqwest::Url::parse(s).map_err(|_| invalid())?;
    let host = url.host_str().unwrap_or_default();
    if !(host == "deezer.com" || host.ends_with(".deezer.com")) {
        return Err(invalid());
    }
    let mut parts = url.path_segments().ok_or_else(invalid)?;
    parts
        .by_ref()
        .find(|p| *p == segment || (segment == "profile" && *p == "user"))
        .ok_or_else(invalid)?;
    parts
        .next()
        .and_then(|id| id.parse().ok())
        .ok_or_else(invalid)
}

// ---- API response shapes (only the fields we use) ----

#[derive(Debug, Deserialize)]
struct ApiError {
    #[serde(rename = "type", default)]
    kind: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    code: i64,
}

#[derive(Debug, Deserialize)]
struct ErrorEnvelope {
    error: Option<ApiError>,
}

#[derive(Debug, Deserialize)]
struct Page<T> {
    #[serde(default = "Vec::new")]
    data: Vec<T>,
    next: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ArtistJson {
    #[serde(default)]
    name: String,
}

#[derive(Debug, Deserialize)]
struct AlbumJson {
    id: Option<u64>,
    #[serde(default)]
    title: String,
}

#[derive(Debug, Deserialize)]
struct TrackJson {
    id: i64,
    #[serde(default)]
    title: String,
    isrc: Option<String>,
    duration: Option<u32>,
    link: Option<String>,
    artist: Option<ArtistJson>,
    album: Option<AlbumJson>,
    /// Only present on `/track/{id}`, not on list endpoints.
    #[serde(default)]
    contributors: Vec<ArtistJson>,
}

impl From<TrackJson> for DeezerTrack {
    fn from(t: TrackJson) -> Self {
        let main = t.artist.as_ref().map(|a| a.name.to_lowercase());
        let mut contributors: Vec<String> = Vec::new();
        for c in t.contributors {
            let lower = c.name.to_lowercase();
            if !c.name.is_empty()
                && Some(&lower) != main.as_ref()
                && !contributors.iter().any(|x| x.to_lowercase() == lower)
            {
                contributors.push(c.name);
            }
        }
        DeezerTrack {
            id: t.id,
            contributors,
            link: t
                .link
                .unwrap_or_else(|| format!("https://www.deezer.com/track/{}", t.id)),
            title: t.title,
            artist: t.artist.map(|a| a.name).unwrap_or_default(),
            album_id: t.album.as_ref().and_then(|a| a.id),
            album: t.album.map(|a| a.title).unwrap_or_default(),
            duration: t.duration.filter(|d| *d > 0),
            isrc: t.isrc.filter(|s| !s.trim().is_empty()),
        }
    }
}

#[derive(Debug, Deserialize)]
struct PlaylistJson {
    id: u64,
    #[serde(default)]
    title: String,
    link: Option<String>,
    #[serde(default)]
    is_loved_track: bool,
}

/// A playlist as listed under a user, before its tracks are fetched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaylistSummary {
    pub id: u64,
    pub title: String,
    /// Deezer's built-in "Loved tracks" playlist (same content as favourites).
    pub is_loved_tracks: bool,
}

/// How long a cached response stays valid.
#[derive(Debug, Clone, Copy)]
enum Freshness {
    /// Track metadata (ISRC, duration) is stable.
    Forever,
    /// Lists change; respect the configured TTL.
    List,
}

pub struct DeezerClient<'c, T> {
    transport: T,
    cache: &'c Cache,
    base: String,
    list_ttl: Option<Duration>,
    max_attempts: u32,
    backoff_base: Duration,
}

impl<'c, T: Transport> DeezerClient<'c, T> {
    pub fn new(transport: T, cache: &'c Cache) -> Self {
        Self {
            transport,
            cache,
            base: API_BASE.to_string(),
            list_ttl: Some(Duration::from_secs(6 * 3600)),
            max_attempts: 6,
            backoff_base: Duration::from_secs(1),
        }
    }

    /// TTL for cached list responses. `Some(Duration::ZERO)` forces a refresh.
    pub fn with_list_ttl(mut self, ttl: Option<Duration>) -> Self {
        self.list_ttl = ttl;
        self
    }

    /// Base delay for exponential backoff (tests use zero).
    pub fn with_backoff(mut self, base: Duration) -> Self {
        self.backoff_base = base;
        self
    }

    async fn get_json(&self, url: &str, freshness: Freshness) -> Result<String> {
        let max_age = match freshness {
            Freshness::Forever => None,
            Freshness::List => self.list_ttl,
        };
        let use_cache = !matches!(max_age, Some(d) if d.is_zero());
        if use_cache {
            if let Some(body) = self.cache.http_get(url, max_age)? {
                return Ok(body);
            }
        }

        for attempt in 0..self.max_attempts {
            if attempt > 0 {
                tokio::time::sleep(self.backoff_base * 2u32.pow(attempt - 1)).await;
            }
            let resp = self.transport.get(url).await?;
            if resp.status == 429 || resp.status >= 500 {
                continue;
            }
            if resp.status != 200 {
                return Err(Error::Http {
                    url: url.to_string(),
                    message: format!("HTTP {}", resp.status),
                });
            }
            let envelope: ErrorEnvelope =
                serde_json::from_str(&resp.body).map_err(|source| Error::Json {
                    url: url.to_string(),
                    source,
                })?;
            match envelope.error {
                Some(e) if e.code == QUOTA_EXCEEDED => continue,
                Some(e) => {
                    return Err(Error::DeezerApi {
                        code: e.code,
                        kind: e.kind,
                        message: e.message,
                    })
                }
                None => {
                    self.cache.http_put(url, &resp.body)?;
                    return Ok(resp.body);
                }
            }
        }
        Err(Error::RateLimited {
            attempts: self.max_attempts,
        })
    }

    async fn get<D: serde::de::DeserializeOwned>(&self, url: &str, f: Freshness) -> Result<D> {
        let body = self.get_json(url, f).await?;
        serde_json::from_str(&body).map_err(|source| Error::Json {
            url: url.to_string(),
            source,
        })
    }

    /// Follow `next` links. Refuses to leave the public API host.
    async fn paginate<D: serde::de::DeserializeOwned>(&self, first: String) -> Result<Vec<D>> {
        let mut out = Vec::new();
        let mut url = Some(first);
        let mut pages = 0;
        while let Some(u) = url.take() {
            if !u.starts_with(&self.base) || pages >= MAX_PAGES {
                break;
            }
            let page: Page<D> = self.get(&u, Freshness::List).await?;
            let empty = page.data.is_empty();
            out.extend(page.data);
            pages += 1;
            if !empty {
                url = page.next;
            }
        }
        Ok(out)
    }

    /// Fetch a public playlist and all its tracks.
    pub async fn playlist(&self, id: u64, progress: &dyn Fn(Progress)) -> Result<Playlist> {
        progress(Progress::FetchingPlaylist { id });
        let meta: PlaylistJson = self
            .get(&format!("{}/playlist/{id}", self.base), Freshness::List)
            .await?;
        let tracks: Vec<TrackJson> = self
            .paginate(format!(
                "{}/playlist/{id}/tracks?index=0&limit={PAGE_SIZE}",
                self.base
            ))
            .await?;
        let playlist = Playlist {
            id: meta.id,
            title: meta.title,
            source: PlaylistSource::Playlist,
            link: meta
                .link
                .unwrap_or_else(|| format!("https://www.deezer.com/playlist/{id}")),
            tracks: tracks.into_iter().map(Into::into).collect(),
        };
        progress(Progress::FetchedPlaylist {
            title: playlist.title.clone(),
            tracks: playlist.tracks.len(),
        });
        Ok(playlist)
    }

    /// A user's favourite tracks (must be public).
    pub async fn user_favourites(
        &self,
        user_id: u64,
        progress: &dyn Fn(Progress),
    ) -> Result<Playlist> {
        let tracks: Vec<TrackJson> = self
            .paginate(format!(
                "{}/user/{user_id}/tracks?index=0&limit={PAGE_SIZE}",
                self.base
            ))
            .await?;
        let playlist = Playlist {
            id: user_id,
            title: "Favourites".to_string(),
            source: PlaylistSource::Favourites,
            link: format!("https://www.deezer.com/profile/{user_id}/loved"),
            tracks: tracks.into_iter().map(Into::into).collect(),
        };
        progress(Progress::FetchedPlaylist {
            title: playlist.title.clone(),
            tracks: playlist.tracks.len(),
        });
        Ok(playlist)
    }

    /// Public playlists listed on a user's profile.
    pub async fn user_playlists(&self, user_id: u64) -> Result<Vec<PlaylistSummary>> {
        let lists: Vec<PlaylistJson> = self
            .paginate(format!(
                "{}/user/{user_id}/playlists?index=0&limit={PAGE_SIZE}",
                self.base
            ))
            .await?;
        Ok(lists
            .into_iter()
            .map(|p| PlaylistSummary {
                id: p.id,
                title: p.title,
                is_loved_tracks: p.is_loved_track,
            })
            .collect())
    }

    /// Fetch explicit playlists, then (for `user_id`) the user's favourites and
    /// public playlists, skipping duplicates and the built-in "Loved tracks"
    /// list. Finally fills in missing ISRCs. A user playlist that fails is
    /// reported as [`Progress::SkippedPlaylist`] rather than aborting.
    pub async fn fetch_all(
        &self,
        playlist_ids: &[u64],
        user_id: Option<u64>,
        progress: &dyn Fn(Progress),
    ) -> Result<Vec<Playlist>> {
        let context = |what: String| {
            move |e: Error| Error::Fetch {
                what,
                source: Box::new(e),
            }
        };
        let mut playlists = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for &id in playlist_ids {
            if seen.insert(id) {
                let pl = self
                    .playlist(id, progress)
                    .await
                    .map_err(context(format!("playlist {id} (is it public?)")))?;
                playlists.push(pl);
            }
        }

        if let Some(uid) = user_id {
            progress(Progress::FetchingUser { id: uid });
            let fav = self
                .user_favourites(uid, progress)
                .await
                .map_err(context(format!(
                    "favourites of user {uid} (are they public?)"
                )))?;
            playlists.push(fav);
            let lists = self
                .user_playlists(uid)
                .await
                .map_err(context(format!("playlists of user {uid}")))?;
            for summary in lists {
                // "Loved tracks" duplicates the favourites fetched above.
                if summary.is_loved_tracks || !seen.insert(summary.id) {
                    continue;
                }
                match self.playlist(summary.id, progress).await {
                    Ok(pl) => playlists.push(pl),
                    Err(e @ Error::RateLimited { .. }) => return Err(e),
                    Err(e) => progress(Progress::SkippedPlaylist {
                        title: summary.title,
                        error: e.to_string(),
                    }),
                }
            }
        }

        self.enrich_tracks(
            playlists.iter_mut().flat_map(|p| p.tracks.iter_mut()),
            progress,
        )
        .await?;
        Ok(playlists)
    }

    /// Enrich tracks with data from `/track/{id}`: ISRC, duration and
    /// contributor credits. Only tracks **missing an ISRC** are requested
    /// (network or cache); for all others an already-cached `/track/{id}`
    /// response is used if present, at no request cost. Individual lookup
    /// failures are skipped, not fatal.
    pub async fn enrich_tracks<'t>(
        &self,
        tracks: impl IntoIterator<Item = &'t mut DeezerTrack>,
        progress: &dyn Fn(Progress),
    ) -> Result<()> {
        let mut todo: Vec<&mut DeezerTrack> = Vec::new();
        for track in tracks.into_iter().filter(|t| t.id > 0) {
            if track.isrc.is_none() {
                todo.push(track);
            } else if let Some(body) = self.cache.http_get(&self.track_url(track.id), None)? {
                if let Ok(full) = serde_json::from_str::<TrackJson>(&body) {
                    apply_details(track, full.into());
                }
            }
        }
        let total = todo.len();
        for (done, track) in todo.iter_mut().enumerate() {
            progress(Progress::FetchingIsrc { done, total });
            match self
                .get::<TrackJson>(&self.track_url(track.id), Freshness::Forever)
                .await
            {
                Ok(full) => apply_details(track, full.into()),
                Err(Error::RateLimited { .. }) => {
                    return Err(Error::RateLimited {
                        attempts: self.max_attempts,
                    })
                }
                Err(_) => {} // e.g. track removed from the catalogue
            }
        }
        if total > 0 {
            progress(Progress::FetchingIsrc { done: total, total });
        }
        Ok(())
    }

    fn track_url(&self, id: i64) -> String {
        format!("{}/track/{id}", self.base)
    }
}

fn apply_details(track: &mut DeezerTrack, full: DeezerTrack) {
    track.isrc = track.isrc.take().or(full.isrc);
    track.duration = track.duration.or(full.duration);
    if track.contributors.is_empty() {
        track.contributors = full.contributors;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_playlist_inputs() {
        assert_eq!(parse_playlist_id("1234567890").unwrap(), 1234567890);
        assert_eq!(
            parse_playlist_id("https://www.deezer.com/en/playlist/12345?utm_source=x").unwrap(),
            12345
        );
        assert_eq!(
            parse_playlist_id("https://deezer.com/playlist/7").unwrap(),
            7
        );
        assert!(parse_playlist_id("https://example.com/playlist/7").is_err());
        assert!(parse_playlist_id("https://www.deezer.com/en/album/7").is_err());
        assert!(parse_playlist_id("hello").is_err());
    }

    #[test]
    fn parses_user_inputs() {
        assert_eq!(parse_user_id("42").unwrap(), 42);
        assert_eq!(
            parse_user_id("https://www.deezer.com/fr/profile/42").unwrap(),
            42
        );
        assert_eq!(
            parse_user_id("https://www.deezer.com/profile/42/loved").unwrap(),
            42
        );
    }
}
