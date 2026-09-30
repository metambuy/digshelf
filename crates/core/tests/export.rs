//! End-to-end: playlists + library → report model → files on disk.

use std::path::PathBuf;

use digshelf_core::export::{render_m3u8, write_all};
use digshelf_core::matcher::MatchConfig;
use digshelf_core::model::{DeezerTrack, LocalTrack, MatchStatus, Playlist, PlaylistSource};
use digshelf_core::overrides::{Overrides, TrackInfo};
use digshelf_core::report::{build_report, ReportOptions};

fn dz(
    id: i64,
    title: &str,
    artist: &str,
    album_id: u64,
    album: &str,
    dur: u32,
    isrc: Option<&str>,
) -> DeezerTrack {
    DeezerTrack {
        id,
        title: title.into(),
        artist: artist.into(),
        album_id: Some(album_id),
        album: album.into(),
        duration: Some(dur),
        isrc: isrc.map(Into::into),
        link: format!("https://www.deezer.com/track/{id}"),
        contributors: Vec::new(),
    }
}

fn local(path: &str, title: &str, artist: &str, secs: u64, isrc: Option<&str>) -> LocalTrack {
    LocalTrack {
        path: PathBuf::from(path),
        title: Some(title.into()),
        artist: Some(artist.into()),
        album: None,
        duration_ms: Some(secs * 1000),
        isrc: isrc.map(Into::into),
    }
}

fn fixture() -> (Vec<Playlist>, Vec<LocalTrack>) {
    let playlists = vec![
        Playlist {
            id: 1001,
            title: "Night / Mix <b>".into(),
            source: PlaylistSource::Playlist,
            link: "https://www.deezer.com/playlist/1001".into(),
            tracks: vec![
                dz(
                    501,
                    "Night Drive",
                    "Vela Nine",
                    7001,
                    "Coastal Lights",
                    300,
                    Some("XXAAA2600001"),
                ),
                dz(
                    502,
                    "Low Tide",
                    "Vela Nine",
                    7001,
                    "Coastal Lights",
                    280,
                    None,
                ),
                dz(
                    503,
                    "Harbour",
                    "Vela Nine",
                    7001,
                    "Coastal Lights",
                    250,
                    None,
                ),
                dz(
                    504,
                    "Glass Rooms",
                    "Mira Sol",
                    7004,
                    "Glass Rooms EP",
                    200,
                    None,
                ),
                dz(
                    505,
                    "Café Nuit, Pt. 2",
                    "Élan Vital",
                    7003,
                    "Rive Gauche",
                    245,
                    None,
                ),
            ],
        },
        Playlist {
            id: 77,
            title: "Favourites".into(),
            source: PlaylistSource::Favourites,
            link: "https://www.deezer.com/profile/77/loved".into(),
            tracks: vec![dz(
                505,
                "Café Nuit, Pt. 2",
                "Élan Vital",
                7003,
                "Rive Gauche",
                245,
                None,
            )],
        },
    ];
    let library = vec![
        local(
            "/Music/Vela Nine/01 Night Drive.flac",
            "Whatever",
            "Tags",
            300,
            Some("XXAAA2600001"),
        ),
        local(
            "/Music/Vela Nine/02 Low Tide (Dub).mp3",
            "Low Tide (Dub Mix)",
            "Vela Nine",
            280,
            None,
        ),
        local(
            "/Music/Élan Vital/Café Nuit, Pt. 2.m4a",
            "Café Nuit, Pt. 2",
            "Élan Vital",
            246,
            None,
        ),
    ];
    (playlists, library)
}

static NO_OVERRIDES: std::sync::LazyLock<Overrides> = std::sync::LazyLock::new(Overrides::default);

fn opts() -> ReportOptions<'static> {
    ReportOptions {
        match_config: MatchConfig::default(),
        qobuz_locale: "us-en",
        overrides: &NO_OVERRIDES,
    }
}

#[test]
fn report_classifies_and_groups() {
    let (playlists, library) = fixture();
    let r = build_report(&playlists, &library, &opts());
    let pl = &r.playlists[0];
    let statuses: Vec<_> = pl.rows.iter().map(|r| r.status).collect();
    use MatchStatus::*;
    assert_eq!(statuses, [Owned, Uncertain, Missing, Missing, Owned]);
    assert_eq!((pl.owned, pl.uncertain, pl.missing), (2, 1, 2));
    // Missing tracks grouped by album in first-appearance order.
    let albums: Vec<_> = pl
        .missing_albums
        .iter()
        .map(|a| (a.album.as_str(), a.tracks.len()))
        .collect();
    assert_eq!(albums, [("Coastal Lights", 1), ("Glass Rooms EP", 1)]);
    assert!(pl.missing_albums[0].links.bandcamp.contains("item_type=a"));
    assert_eq!((r.owned, r.uncertain, r.missing), (3, 1, 2));
}

#[test]
fn m3u8_is_extended_utf8_with_absolute_paths() {
    let (playlists, library) = fixture();
    let r = build_report(&playlists, &library, &opts());
    assert_eq!(
        render_m3u8(&r.playlists[0]),
        "#EXTM3U\n\
         #EXTINF:300,Tags - Whatever\n\
         /Music/Vela Nine/01 Night Drive.flac\n\
         #EXTINF:246,Élan Vital - Café Nuit, Pt. 2\n\
         /Music/Élan Vital/Café Nuit, Pt. 2.m4a\n"
    );
}

#[test]
fn writes_all_outputs() {
    let (playlists, library) = fixture();
    let r = build_report(&playlists, &library, &opts());
    let dir = tempfile::tempdir().unwrap();
    let w = write_all(&r, dir.path()).unwrap();

    let names: Vec<_> = w
        .playlists
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["Night _ Mix _b_.m3u8", "Favourites.m3u8"]);
    for p in &w.playlists {
        let bytes = std::fs::read(p).unwrap();
        assert!(bytes.starts_with(b"#EXTM3U\n"), "no BOM, header first");
        assert!(!bytes.contains(&b'\r'));
    }

    let csv = std::fs::read_to_string(&w.csv).unwrap();
    let lines: Vec<_> = csv.lines().collect();
    assert_eq!(lines.len(), 4, "header + 1 uncertain + 2 missing:\n{csv}");
    assert!(lines[0].starts_with("playlist,status,artist,title"));
    assert!(lines[1].starts_with("Night / Mix <b>,uncertain,Vela Nine,Low Tide,"));
    assert!(lines[2].contains(",missing,Vela Nine,Harbour,"));

    let html = std::fs::read_to_string(&w.report).unwrap();
    assert!(
        html.contains("Night &#x2f; Mix &lt;b&gt;"),
        "titles are escaped"
    );
    // Undo attribute escaping to compare URLs and paths.
    let html = html.replace("&#x2f;", "/").replace("&amp;", "&");
    assert!(!html.contains("Mix <b>"));
    assert!(html.contains("data-panel=\"uncertain\""));
    assert!(html.contains("https://www.beatport.com/search/releases?q=Vela+Nine+Coastal+Lights"));
    assert!(html.contains("/Music/Vela Nine/02 Low Tide (Dub).mp3"));
    assert!(html.contains("version 30% · Δ0s"));
    assert!(!html.contains("<script src"), "self-contained");
}

#[test]
fn overrides_flow_into_report_and_m3u8() {
    let (playlists, library) = fixture();
    let mut ov = Overrides::default();
    // Accept the uncertain "Low Tide" candidate (a dub mix), reject the ISRC match,
    // and point an override at a file MediaMonkey has since moved.
    ov.accept(
        502,
        "/Music/Vela Nine/02 Low Tide (Dub).mp3".into(),
        TrackInfo::default(),
    );
    ov.reject(501, None, TrackInfo::default());
    ov.map(
        504,
        "/Music/moved/Glass Rooms.mp3".into(),
        TrackInfo::default(),
    );
    let o = ReportOptions {
        overrides: &ov,
        ..opts()
    };
    let r = build_report(&playlists, &library, &o);
    let pl = &r.playlists[0];
    use MatchStatus::*;
    let statuses: Vec<_> = pl.rows.iter().map(|r| r.status).collect();
    assert_eq!(statuses, [Missing, Owned, Missing, Missing, Owned]);
    assert!(pl.rows[0].rejected_by_user);
    assert_eq!(
        pl.rows[1].matched.as_ref().unwrap().method,
        digshelf_core::model::MatchMethod::Manual
    );
    assert_eq!(r.stale_overrides.len(), 1);
    assert_eq!(r.stale_overrides[0].deezer_id, 504);
    let m3u = render_m3u8(pl);
    assert!(m3u.contains("/Music/Vela Nine/02 Low Tide (Dub).mp3\n"));
    assert!(!m3u.contains("Night Drive.flac"));
}

#[test]
fn uncertain_rows_carry_pasteable_snippets() {
    let (playlists, library) = fixture();
    let r = build_report(&playlists, &library, &opts());
    let row = &r.playlists[0].rows[1];
    assert_eq!(row.status, MatchStatus::Uncertain);
    let s = row.snippets.as_ref().unwrap();
    for text in [&s.accept, &s.reject_file, &s.reject_track] {
        let parsed = Overrides::parse(text).unwrap();
        assert_eq!(parsed.entries()[0].deezer_id, 502);
    }
    assert!(
        r.playlists[0].rows[0].snippets.is_none(),
        "owned rows need none"
    );
}
