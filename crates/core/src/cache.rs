//! SQLite cache for Deezer API responses and library scan results.
//!
//! Only JSON metadata from api.deezer.com and tag snapshots of local files are
//! stored here. Never audio.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{Error, Result};
use crate::model::LocalTrack;

const SCHEMA_VERSION: i64 = 1;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS http_cache (
    url        TEXT PRIMARY KEY,
    body       TEXT NOT NULL,
    fetched_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS library_file (
    path        TEXT PRIMARY KEY,
    mtime       INTEGER NOT NULL,
    size        INTEGER NOT NULL,
    title       TEXT,
    artist      TEXT,
    album       TEXT,
    duration_ms INTEGER,
    isrc        TEXT,
    scanned_at  INTEGER NOT NULL
);
";

/// A cached library row plus the file stamp it was read at.
#[derive(Debug, Clone, PartialEq)]
pub struct CachedFile {
    pub track: LocalTrack,
    pub mtime: i64,
    pub size: i64,
}

pub struct Cache {
    conn: Connection,
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

impl Cache {
    /// Open (or create) the cache database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::io(parent, e))?;
        }
        Self::init(Connection::open(path)?)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version != SCHEMA_VERSION {
            // Only a cache: on any schema change, start over.
            conn.execute_batch(
                "DROP TABLE IF EXISTS http_cache; DROP TABLE IF EXISTS library_file;",
            )?;
        }
        conn.execute_batch(SCHEMA)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self { conn })
    }

    /// Cached body for `url`, if present and younger than `max_age`
    /// (`None` = never expires).
    pub fn http_get(&self, url: &str, max_age: Option<Duration>) -> Result<Option<String>> {
        let row: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT body, fetched_at FROM http_cache WHERE url = ?1",
                [url],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        Ok(row.and_then(|(body, fetched_at)| match max_age {
            Some(age) if now_secs() - fetched_at > age.as_secs() as i64 => None,
            _ => Some(body),
        }))
    }

    pub fn http_put(&self, url: &str, body: &str) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO http_cache (url, body, fetched_at) VALUES (?1, ?2, ?3)",
            params![url, body, now_secs()],
        )?;
        Ok(())
    }

    /// All cached library rows, keyed by path.
    pub fn library_files(&self) -> Result<HashMap<PathBuf, CachedFile>> {
        let mut stmt = self.conn.prepare(
            "SELECT path, mtime, size, title, artist, album, duration_ms, isrc FROM library_file",
        )?;
        let rows = stmt.query_map([], |r| {
            let path = PathBuf::from(r.get::<_, String>(0)?);
            Ok(CachedFile {
                mtime: r.get(1)?,
                size: r.get(2)?,
                track: LocalTrack {
                    path,
                    title: r.get(3)?,
                    artist: r.get(4)?,
                    album: r.get(5)?,
                    duration_ms: r.get::<_, Option<i64>>(6)?.map(|v| v as u64),
                    isrc: r.get(7)?,
                },
            })
        })?;
        let mut out = HashMap::new();
        for row in rows {
            let row = row?;
            out.insert(row.track.path.clone(), row);
        }
        Ok(out)
    }

    /// Replace the cached rows under `root` with `files` in one transaction.
    /// Rows under `root` that are not in `files` (deleted files) are removed.
    pub fn replace_library(&mut self, root: &Path, files: &[CachedFile]) -> Result<()> {
        let now = now_secs();
        let tx = self.conn.transaction()?;
        {
            let prefix = format!("{}/", root.to_string_lossy().trim_end_matches('/'));
            tx.execute(
                "DELETE FROM library_file WHERE substr(path, 1, length(?1)) = ?1",
                [&prefix],
            )?;
            let mut stmt = tx.prepare(
                "INSERT OR REPLACE INTO library_file
                 (path, mtime, size, title, artist, album, duration_ms, isrc, scanned_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            for f in files {
                let t = &f.track;
                stmt.execute(params![
                    t.path.to_string_lossy(),
                    f.mtime,
                    f.size,
                    t.title,
                    t.artist,
                    t.album,
                    t.duration_ms.map(|v| v as i64),
                    t.isrc,
                    now
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Remove every cached row.
    pub fn clear(&self) -> Result<()> {
        self.conn
            .execute_batch("DELETE FROM http_cache; DELETE FROM library_file; VACUUM;")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn http_roundtrip_and_ttl() {
        let c = Cache::open_in_memory().unwrap();
        c.http_put("https://api.deezer.com/track/1", "{}").unwrap();
        assert_eq!(
            c.http_get("https://api.deezer.com/track/1", None)
                .unwrap()
                .as_deref(),
            Some("{}")
        );
        c.conn
            .execute("UPDATE http_cache SET fetched_at = fetched_at - 7200", [])
            .unwrap();
        let hour = Some(Duration::from_secs(3600));
        assert_eq!(
            c.http_get("https://api.deezer.com/track/1", hour).unwrap(),
            None
        );
        c.clear().unwrap();
        assert_eq!(
            c.http_get("https://api.deezer.com/track/1", None).unwrap(),
            None
        );
    }

    #[test]
    fn replace_library_only_touches_root() {
        let mut c = Cache::open_in_memory().unwrap();
        let file = |p: &str| CachedFile {
            track: LocalTrack {
                path: p.into(),
                title: Some("T".into()),
                artist: None,
                album: None,
                duration_ms: Some(1000),
                isrc: None,
            },
            mtime: 1,
            size: 2,
        };
        c.replace_library(Path::new("/a"), &[file("/a/1.mp3"), file("/a/2.mp3")])
            .unwrap();
        c.replace_library(Path::new("/ab"), &[file("/ab/3.mp3")])
            .unwrap();
        c.replace_library(Path::new("/a"), &[file("/a/2.mp3")])
            .unwrap();
        let mut paths: Vec<_> = c.library_files().unwrap().into_keys().collect();
        paths.sort();
        assert_eq!(
            paths,
            vec![PathBuf::from("/a/2.mp3"), PathBuf::from("/ab/3.mp3")]
        );
    }
}
