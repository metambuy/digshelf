//! Deezer client tests against recorded (synthetic) fixtures. No network.

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use digshelf_core::cache::Cache;
use digshelf_core::deezer::DeezerClient;
use digshelf_core::http::{Response, Transport};
use digshelf_core::model::PlaylistSource;
use digshelf_core::Error;

fn fixture(name: &str) -> String {
    let path = format!(
        "{}/tests/fixtures/deezer/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// Replays queued responses per URL; the last one repeats. Records calls.
#[derive(Default)]
struct Stub {
    routes: RefCell<HashMap<String, VecDeque<Response>>>,
    calls: RefCell<Vec<String>>,
}

impl Stub {
    fn route(self, url: &str, fixture_name: &str) -> Self {
        self.route_status(url, 200, fixture_name)
    }

    fn route_status(self, url: &str, status: u16, fixture_name: &str) -> Self {
        self.routes
            .borrow_mut()
            .entry(url.to_string())
            .or_default()
            .push_back(Response {
                status,
                body: fixture(fixture_name),
            });
        self
    }

    fn calls_to(&self, url: &str) -> usize {
        self.calls.borrow().iter().filter(|u| *u == url).count()
    }
}

impl Transport for &Stub {
    async fn get(&self, url: &str) -> digshelf_core::Result<Response> {
        self.calls.borrow_mut().push(url.to_string());
        let mut routes = self.routes.borrow_mut();
        let queue = routes
            .get_mut(url)
            .unwrap_or_else(|| panic!("unexpected request: {url}"));
        Ok(if queue.len() > 1 {
            queue.pop_front().unwrap()
        } else {
            queue.front().unwrap().clone()
        })
    }
}

const PL: &str = "https://api.deezer.com/playlist/1001";
const PL_TRACKS_0: &str = "https://api.deezer.com/playlist/1001/tracks?index=0&limit=100";
const PL_TRACKS_2: &str = "https://api.deezer.com/playlist/1001/tracks?index=2&limit=100";

fn playlist_stub() -> Stub {
    Stub::default()
        .route(PL, "playlist_1001.json")
        .route(PL_TRACKS_0, "playlist_1001_tracks_0.json")
        .route(PL_TRACKS_2, "playlist_1001_tracks_2.json")
}

#[tokio::test]
async fn fetches_paginated_playlist() {
    let stub = playlist_stub();
    let cache = Cache::open_in_memory().unwrap();
    let client = DeezerClient::new(&stub, &cache).with_backoff(Duration::ZERO);
    let pl = client.playlist(1001, &|_| {}).await.unwrap();

    assert_eq!(pl.title, "Synthetic Night Mix");
    assert_eq!(pl.source, PlaylistSource::Playlist);
    let titles: Vec<_> = pl.tracks.iter().map(|t| t.title.as_str()).collect();
    assert_eq!(
        titles,
        [
            "Night Drive",
            "Low Tide (Kora Blue Remix)",
            "Café Nuit (feat. Ana Luz)"
        ]
    );
    assert_eq!(pl.tracks[0].isrc.as_deref(), Some("XXAAA2600001"));
    assert_eq!(pl.tracks[2].artist, "Élan Vital");
    assert_eq!(pl.tracks[1].album, "Low Tide Remixes");
    // Preview URLs are never requested.
    assert!(stub
        .calls
        .borrow()
        .iter()
        .all(|u| u.starts_with("https://api.deezer.com/")));
}

#[tokio::test]
async fn second_fetch_is_served_from_cache() {
    let stub = playlist_stub();
    let cache = Cache::open_in_memory().unwrap();
    let client = DeezerClient::new(&stub, &cache);
    client.playlist(1001, &|_| {}).await.unwrap();
    client.playlist(1001, &|_| {}).await.unwrap();
    assert_eq!(stub.calls_to(PL_TRACKS_0), 1);

    // A zero TTL (--refresh) bypasses cached lists.
    let client = DeezerClient::new(&stub, &cache).with_list_ttl(Some(Duration::ZERO));
    client.playlist(1001, &|_| {}).await.unwrap();
    assert_eq!(stub.calls_to(PL_TRACKS_0), 2);
}

#[tokio::test]
async fn retries_on_quota_and_server_errors() {
    let stub = Stub::default()
        .route(PL, "error_quota.json")
        .route_status(PL, 503, "error_quota.json")
        .route(PL, "playlist_1001.json")
        .route(PL_TRACKS_0, "playlist_1001_tracks_0.json")
        .route(PL_TRACKS_2, "playlist_1001_tracks_2.json");
    let cache = Cache::open_in_memory().unwrap();
    let client = DeezerClient::new(&stub, &cache).with_backoff(Duration::ZERO);
    let pl = client.playlist(1001, &|_| {}).await.unwrap();
    assert_eq!(pl.tracks.len(), 3);
    assert_eq!(stub.calls_to(PL), 3);
}

#[tokio::test]
async fn persistent_quota_error_gives_up() {
    let stub = Stub::default().route(PL, "error_quota.json");
    let cache = Cache::open_in_memory().unwrap();
    let client = DeezerClient::new(&stub, &cache).with_backoff(Duration::ZERO);
    let err = client.playlist(1001, &|_| {}).await.unwrap_err();
    assert!(matches!(err, Error::RateLimited { .. }), "{err}");
}

#[tokio::test]
async fn missing_playlist_is_an_api_error_and_not_cached() {
    let stub = Stub::default().route(PL, "error_no_data.json");
    let cache = Cache::open_in_memory().unwrap();
    let client = DeezerClient::new(&stub, &cache);
    let err = client.playlist(1001, &|_| {}).await.unwrap_err();
    assert!(matches!(err, Error::DeezerApi { code: 800, .. }), "{err}");
    assert_eq!(cache.http_get(PL, None).unwrap(), None);
}

#[tokio::test]
async fn favourites_fill_missing_isrc_and_skip_uploads() {
    let stub = Stub::default()
        .route(
            "https://api.deezer.com/user/77/tracks?index=0&limit=100",
            "user_77_tracks_0.json",
        )
        .route("https://api.deezer.com/track/504", "track_504.json");
    let cache = Cache::open_in_memory().unwrap();
    let client = DeezerClient::new(&stub, &cache);
    let mut fav = client.user_favourites(77, &|_| {}).await.unwrap();
    assert_eq!(fav.source, PlaylistSource::Favourites);
    assert_eq!(fav.tracks.len(), 2);
    assert!(fav.tracks.iter().all(|t| t.isrc.is_none()));

    client
        .fill_missing_isrc(fav.tracks.iter_mut(), &|_| {})
        .await
        .unwrap();
    assert_eq!(fav.tracks[0].isrc.as_deref(), Some("XXCCC2600004"));
    // Negative IDs are user uploads: never looked up.
    assert_eq!(fav.tracks[1].isrc, None);
    assert_eq!(stub.calls.borrow().len(), 2);
}

#[tokio::test]
async fn lists_user_playlists() {
    let stub = Stub::default().route(
        "https://api.deezer.com/user/77/playlists?index=0&limit=100",
        "user_77_playlists_0.json",
    );
    let cache = Cache::open_in_memory().unwrap();
    let lists = DeezerClient::new(&stub, &cache)
        .user_playlists(77)
        .await
        .unwrap();
    assert_eq!(lists.len(), 2);
    assert!(lists[0].is_loved_tracks);
    assert_eq!(lists[1].id, 1001);
}
