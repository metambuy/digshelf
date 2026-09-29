//! Writers for report.html, missing.csv and per-playlist .m3u8 files.
//! Everything is written inside the output directory only.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use minijinja::Environment;

use crate::error::{Error, Result};
use crate::model::MatchStatus;
use crate::report::{PlaylistReport, Report};

const REPORT_TEMPLATE: &str = include_str!("../templates/report.html");

/// Paths of the files written by [`write_all`].
#[derive(Debug, Default)]
pub struct Written {
    pub report: PathBuf,
    pub csv: PathBuf,
    pub playlists: Vec<PathBuf>,
}

pub fn write_all(report: &Report, out_dir: &Path) -> Result<Written> {
    std::fs::create_dir_all(out_dir).map_err(|e| Error::io(out_dir, e))?;
    let mut written = Written {
        report: out_dir.join("report.html"),
        csv: out_dir.join("missing.csv"),
        playlists: Vec::new(),
    };
    write_file(&written.report, &render_html(report)?)?;
    write_file(&written.csv, &render_csv(report)?)?;

    let mut used = HashSet::new();
    for pl in &report.playlists {
        let path = out_dir.join(unique_file_name(pl, &mut used));
        write_file(&path, &render_m3u8(pl))?;
        written.playlists.push(path);
    }
    Ok(written)
}

fn write_file(path: &Path, contents: &str) -> Result<()> {
    std::fs::write(path, contents).map_err(|e| Error::io(path, e))
}

pub fn render_html(report: &Report) -> Result<String> {
    let mut env = Environment::new();
    // `.html` name enables HTML auto-escaping.
    env.add_template("report.html", REPORT_TEMPLATE)?;
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
    Ok(env.get_template("report.html")?.render(report)?)
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

/// A safe, unique `.m3u8` file name for a playlist.
fn unique_file_name(pl: &PlaylistReport, used: &mut HashSet<String>) -> String {
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
    let mut name = format!("{stem}.m3u8");
    if !used.insert(name.to_lowercase()) {
        name = format!("{stem} ({}).m3u8", pl.id);
        used.insert(name.to_lowercase());
    }
    name
}
