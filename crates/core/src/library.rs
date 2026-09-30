//! Read-only scan of the local music library.
//!
//! The library is owned by MediaMonkey: this module only opens files for
//! reading (lofty's `Probe::open` uses `File::open`), never writes tags and
//! never renames, moves or deletes anything.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use lofty::config::{ParseOptions, ParsingMode};
use lofty::file::TaggedFile;
use lofty::prelude::*;
use lofty::probe::Probe;
use walkdir::WalkDir;

use crate::cache::{Cache, CachedFile};
use crate::error::{Error, Result};
use crate::model::{LocalTrack, Progress};

/// File extensions treated as audio (lowercase).
pub const AUDIO_EXTENSIONS: &[&str] = &[
    "flac", "mp3", "m4a", "aac", "mp4", "aiff", "aif", "wav", "ogg", "oga",
];

#[derive(Debug, Default)]
pub struct ScanResult {
    pub tracks: Vec<LocalTrack>,
    /// Files that were found but whose tags could not be read. They are still
    /// included in `tracks` (matched by file name only).
    pub unreadable: Vec<(PathBuf, String)>,
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn is_hidden(entry: &walkdir::DirEntry) -> bool {
    entry.depth() > 0 && entry.file_name().to_string_lossy().starts_with('.')
}

fn non_empty(s: Option<std::borrow::Cow<'_, str>>) -> Option<String> {
    s.map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

fn probe(path: &Path, read_properties: bool) -> std::result::Result<TaggedFile, String> {
    Probe::open(path)
        .map_err(|e| e.to_string())?
        .options(
            ParseOptions::new()
                .read_cover_art(false)
                .read_properties(read_properties)
                .parsing_mode(ParsingMode::Relaxed),
        )
        .read()
        .map_err(|e| e.to_string())
}

/// Read tags and duration from one file, without modifying it. If the audio
/// stream cannot be parsed, fall back to tags only (no duration).
pub fn read_file(path: &Path) -> std::result::Result<LocalTrack, String> {
    let tagged = probe(path, true).or_else(|e| probe(path, false).map_err(|_| e))?;
    let duration = tagged.properties().duration();
    let tag = tagged.primary_tag().or_else(|| tagged.first_tag());
    Ok(LocalTrack {
        path: path.to_path_buf(),
        title: tag.and_then(|t| non_empty(t.title())),
        artist: tag.and_then(|t| non_empty(t.artist())),
        album: tag.and_then(|t| non_empty(t.album())),
        duration_ms: (!duration.is_zero()).then_some(duration.as_millis() as u64),
        isrc: tag
            .and_then(|t| t.get_string(ItemKey::Isrc))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty()),
    })
}

struct Found {
    path: PathBuf,
    mtime: i64,
    size: i64,
}

/// Scan `root` recursively. Files whose mtime and size match the cache are not
/// re-read. The cache is updated with the result.
pub fn scan(root: &Path, cache: &mut Cache, progress: &dyn Fn(Progress)) -> Result<ScanResult> {
    let root = std::path::absolute(root).map_err(|e| Error::io(root, e))?;
    if !root.is_dir() {
        return Err(Error::io(
            &root,
            std::io::Error::new(std::io::ErrorKind::NotFound, "music folder not found"),
        ));
    }

    let mut found = Vec::new();
    for entry in WalkDir::new(&root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|e| !is_hidden(e))
    {
        let Ok(entry) = entry else { continue }; // permission errors etc.
        if !entry.file_type().is_file() || !is_audio(entry.path()) {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |d| d.as_secs() as i64);
        found.push(Found {
            path: entry.into_path(),
            mtime,
            size: meta.len() as i64,
        });
        if found.len() % 500 == 0 {
            progress(Progress::ScanningLibrary {
                files_seen: found.len(),
            });
        }
    }

    let mut cached: HashMap<PathBuf, CachedFile> = cache.library_files()?;
    let mut rows: Vec<CachedFile> = Vec::with_capacity(found.len());
    let mut todo: Vec<Found> = Vec::new();
    for f in found {
        match cached.remove(&f.path) {
            Some(c) if c.mtime == f.mtime && c.size == f.size => rows.push(c),
            _ => todo.push(f),
        }
    }
    let mut result = ScanResult::default();
    for c in &rows {
        if let Some(e) = &c.read_error {
            result.unreadable.push((c.track.path.clone(), e.clone()));
        }
    }
    let cached_count = rows.len();

    let read = read_parallel(&todo);
    for (f, outcome) in todo.into_iter().zip(read) {
        let (track, read_error) = match outcome {
            Ok(t) => (t, None),
            Err(e) => {
                result.unreadable.push((f.path.clone(), e.clone()));
                let t = LocalTrack {
                    path: f.path.clone(),
                    title: None,
                    artist: None,
                    album: None,
                    duration_ms: None,
                    isrc: None,
                };
                (t, Some(e))
            }
        };
        rows.push(CachedFile {
            track,
            mtime: f.mtime,
            size: f.size,
            read_error,
        });
    }

    cache.replace_library(&root, &rows)?;
    progress(Progress::ScannedLibrary {
        files: rows.len(),
        read: rows.len() - cached_count,
        cached: cached_count,
        failed: result.unreadable.len(),
    });

    rows.sort_by(|a, b| a.track.path.cmp(&b.track.path));
    result.unreadable.sort();
    result.tracks = rows.into_iter().map(|r| r.track).collect();
    Ok(result)
}

/// A malformed file must not abort the whole scan.
fn read_guarded(path: &Path) -> std::result::Result<LocalTrack, String> {
    std::panic::catch_unwind(|| read_file(path))
        .unwrap_or_else(|_| Err("tag reader panicked".to_string()))
}

fn read_parallel(files: &[Found]) -> Vec<std::result::Result<LocalTrack, String>> {
    if files.is_empty() {
        return Vec::new();
    }
    let workers = std::thread::available_parallelism()
        .map_or(4, |n| n.get())
        .min(files.len());
    let chunk = files.len().div_ceil(workers);
    std::thread::scope(|s| {
        let handles: Vec<_> = files
            .chunks(chunk)
            .map(|c| s.spawn(move || c.iter().map(|f| read_guarded(&f.path)).collect::<Vec<_>>()))
            .collect();
        handles
            .into_iter()
            .flat_map(|h| h.join().expect("read_guarded does not panic"))
            .collect()
    })
}
