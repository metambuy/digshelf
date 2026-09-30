//! Writers for report.html, missing.csv and per-playlist .m3u8 files.
//! Everything is written inside the output directory only.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use minijinja::Environment;
use serde::Serialize;

use crate::error::{Error, Result};
use crate::model::MatchStatus;
use crate::report::{PlaylistReport, Report};

const STYLE_TEMPLATE: &str = include_str!("../templates/_style.html");
const INDEX_TEMPLATE: &str = include_str!("../templates/index.html");
const PLAYLIST_TEMPLATE: &str = include_str!("../templates/playlist.html");

/// Folder (inside the output dir) holding one HTML page per playlist.
pub const PAGES_DIR: &str = "playlists";

/// Paths of the files written by [`write_all`].
#[derive(Debug, Default)]
pub struct Written {
    pub index: PathBuf,
    /// One HTML page per playlist, in report order.
    pub pages: Vec<PathBuf>,
    pub csv: PathBuf,
    /// One `.m3u8` per playlist, in report order.
    pub playlists: Vec<PathBuf>,
}

/// Write `index.html`, `playlists/<name>.html`, `missing.csv` and
/// `<name>.m3u8` into `out_dir`.
pub fn write_all(report: &Report, out_dir: &Path) -> Result<Written> {
    let pages_dir = out_dir.join(PAGES_DIR);
    std::fs::create_dir_all(&pages_dir).map_err(|e| Error::io(&pages_dir, e))?;
    let env = environment()?;

    let mut used = HashSet::new();
    let stems: Vec<String> = report
        .playlists
        .iter()
        .map(|pl| unique_stem(pl, &mut used))
        .collect();

    let mut written = Written {
        index: out_dir.join("index.html"),
        csv: out_dir.join("missing.csv"),
        ..Written::default()
    };
    write_file(&written.index, &render_index(&env, report, &stems)?)?;
    write_file(&written.csv, &render_csv(report)?)?;
    for (pl, stem) in report.playlists.iter().zip(&stems) {
        let page = pages_dir.join(format!("{stem}.html"));
        write_file(&page, &render_playlist(&env, pl)?)?;
        written.pages.push(page);
        let m3u = out_dir.join(format!("{stem}.m3u8"));
        write_file(&m3u, &render_m3u8(pl))?;
        written.playlists.push(m3u);
    }
    Ok(written)
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents).map_err(|e| Error::io(path, e))
}

fn environment() -> Result<Environment<'static>> {
    let mut env = Environment::new();
    // `.html` names enable HTML auto-escaping.
    env.add_template("_style.html", STYLE_TEMPLATE)?;
    env.add_template("index.html", INDEX_TEMPLATE)?;
    env.add_template("playlist.html", PLAYLIST_TEMPLATE)?;
    env.add_filter("mmss", |secs: Option<u32>| match secs {
        Some(s) => format!("{}:{:02}", s / 60, s % 60),
        None => "–".to_string(),
    });
    env.add_filter("ms_mmss", |ms: Option<u64>| match ms {
        Some(ms) => {
            let s = (ms + 500) / 1000;
            format!("{}:{:02}", s / 60, s % 60)
        }
        None => "–".to_string(),
    });
    env.add_filter("pct", |x: f64| format!("{:.0}%", x * 100.0));
    Ok(env)
}

#[derive(Serialize)]
struct PageLink<'a> {
    title: &'a str,
    href: String,
    owned: usize,
    uncertain: usize,
    missing: usize,
    total: usize,
}

fn render_index(env: &Environment, report: &Report, stems: &[String]) -> Result<String> {
    let pages: Vec<PageLink> = report
        .playlists
        .iter()
        .zip(stems)
        .map(|(pl, stem)| PageLink {
            title: &pl.title,
            href: format!("{PAGES_DIR}/{}.html", encode_segment(stem)),
            owned: pl.owned,
            uncertain: pl.uncertain,
            missing: pl.missing,
            total: pl.rows.len(),
        })
        .collect();
    let ctx = minijinja::context! {
        pages,
        library_files => report.library_files,
        owned => report.owned,
        uncertain => report.uncertain,
        missing => report.missing,
        stale_overrides => &report.stale_overrides,
        unreadable => &report.unreadable,
    };
    Ok(env.get_template("index.html")?.render(ctx)?)
}

fn render_playlist(env: &Environment, pl: &PlaylistReport) -> Result<String> {
    Ok(env
        .get_template("playlist.html")?
        .render(minijinja::context! { pl })?)
}

/// Percent-encode a file name for use in a relative URL.
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(b as char);
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

/// One row per missing or uncertain (playlist, track).
pub fn render_csv(report: &Report) -> Result<String> {
    let mut w = csv::Writer::from_writer(Vec::new());
    w.write_record([
        "playlist",
        "status",
        "artist",
        "title",
        "album",
        "duration_s",
        "isrc",
        "deezer_url",
        "candidate_file",
        "confidence",
        "bandcamp",
        "qobuz",
        "beatport",
    ])?;
    for pl in &report.playlists {
        for row in pl.rows.iter().filter(|r| r.status != MatchStatus::Owned) {
            let t = &row.track;
            w.write_record([
                pl.title.as_str(),
                if row.status == MatchStatus::Missing {
                    "missing"
                } else {
                    "uncertain"
                },
                &t.artist,
                &t.title,
                &t.album,
                &t.duration.map(|d| d.to_string()).unwrap_or_default(),
                t.isrc.as_deref().unwrap_or(""),
                &t.link,
                &row.matched
                    .as_ref()
                    .map(|m| m.file.path.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                &row.matched
                    .as_ref()
                    .map(|m| format!("{:.2}", m.confidence))
                    .unwrap_or_default(),
                &row.links.bandcamp,
                &row.links.qobuz,
                &row.links.beatport,
            ])?;
        }
    }
    let bytes = w
        .into_inner()
        .map_err(|e| Error::io("missing.csv", e.into_error()))?;
    Ok(String::from_utf8(bytes).expect("CSV input was UTF-8"))
}

fn one_line(s: &str) -> String {
    s.split(['\r', '\n'])
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

/// Extended M3U, UTF-8, LF, absolute paths, Owned tracks only in playlist
/// order. Imports into MediaMonkey and Mixxx.
pub fn render_m3u8(pl: &PlaylistReport) -> String {
    let mut s = String::from("#EXTM3U\n");
    for row in pl.rows_with(MatchStatus::Owned) {
        let Some(m) = &row.matched else { continue };
        let f = &m.file;
        let secs = f
            .duration_secs()
            .or(row.track.duration)
            .map_or(-1, i64::from);
        let artist = f.artist.as_deref().unwrap_or(&row.track.artist);
        let title = f.title.as_deref().unwrap_or(&row.track.title);
        let _ = writeln!(
            s,
            "#EXTINF:{secs},{} - {}",
            one_line(artist),
            one_line(title)
        );
        let _ = writeln!(s, "{}", f.path.to_string_lossy());
    }
    s
}

/// A safe file-name stem for a playlist, unique (case-insensitively) within
/// one run. Shared by the `.m3u8` and the HTML page.
fn unique_stem(pl: &PlaylistReport, used: &mut HashSet<String>) -> String {
    let cleaned: String = pl
        .title
        .chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let mut stem: String = cleaned
        .trim()
        .trim_start_matches('.')
        .chars()
        .take(120)
        .collect();
    if stem.is_empty() {
        stem = format!("playlist-{}", pl.id);
    }
    if !used.insert(stem.to_lowercase()) {
        stem = format!("{stem} ({})", pl.id);
        used.insert(stem.to_lowercase());
    }
    stem
}
