//! User decisions about matches, stored in a TOML file outside the repo
//! (by default `<config dir>/digshelf/overrides.toml`).
//!
//! The file is meant to be edited by hand *and* by the GUI through this API.
//! Saving rewrites the whole file, so hand-written comments are not kept.
//!
//! ```toml
//! version = 1
//!
//! [[track]]
//! deezer_id = 3135556
//! action = "accept"          # accept | reject | map
//! file = "/abs/path/to/file.flac"
//! note = "optional"
//! artist = "informational"   # ignored by the matcher
//! title = "informational"
//! ```
//!
//! - `accept`: the proposed candidate is correct. `map`: this file is the
//!   track (chosen by hand). Both force the track to Owned.
//! - `reject` with `file`: never match that file to this track.
//!   `reject` without `file`: the track is not in the library (Missing).
//! - Precedence: accept/map > reject > automatic matching.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const FORMAT_VERSION: u32 = 1;

const HEADER: &str = "\
# digshelf overrides: confirm, reject or force matches.
# Edited by hand or by digshelf; saving from digshelf drops other comments.
# actions: accept | map (force Owned, needs `file`), reject (with or without `file`).
";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Accept,
    Map,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverrideEntry {
    pub deezer_id: i64,
    pub action: Action,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// Informational only, to keep the file readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Informational only, to keep the file readable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

/// Human-readable context stored alongside an entry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackInfo {
    pub artist: Option<String>,
    pub title: Option<String>,
    pub note: Option<String>,
}

/// The overrides that apply to one Deezer track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackOverride<'a> {
    /// Forced file and whether it was an accept or a map.
    pub forced: Option<(&'a Path, Action)>,
    /// The track is known not to be in the library.
    pub reject_all: bool,
    /// Files that must never be matched to this track.
    pub rejected_files: Vec<&'a Path>,
}

impl TrackOverride<'_> {
    pub fn is_empty(&self) -> bool {
        self.forced.is_none() && !self.reject_all && self.rejected_files.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Overrides {
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default, rename = "track")]
    entries: Vec<OverrideEntry>,
}

fn default_version() -> u32 {
    FORMAT_VERSION
}

impl Default for Overrides {
    fn default() -> Self {
        Self {
            version: FORMAT_VERSION,
            entries: Vec::new(),
        }
    }
}

impl Overrides {
    /// Load from `path`. A missing file is an empty set.
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|message| Error::Overrides {
                path: path.to_path_buf(),
                message,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(Error::io(path, e)),
        }
    }

    pub fn parse(text: &str) -> std::result::Result<Self, String> {
        let mut o: Overrides = toml::from_str(text).map_err(|e| e.to_string())?;
        if o.version > FORMAT_VERSION {
            return Err(format!(
                "format version {} is newer than supported ({FORMAT_VERSION})",
                o.version
            ));
        }
        for e in &o.entries {
            if matches!(e.action, Action::Accept | Action::Map) && e.file.is_none() {
                return Err(format!(
                    "deezer_id {}: action {:?} needs a `file`",
                    e.deezer_id, e.action
                ));
            }
        }
        o.version = FORMAT_VERSION;
        Ok(o)
    }

    /// Serialise with a short header, entries sorted by Deezer ID.
    pub fn to_toml(&self) -> String {
        let mut sorted = self.clone();
        sorted.entries.sort_by_key(|e| (e.deezer_id, e.action));
        let body = toml::to_string_pretty(&sorted).expect("overrides always serialise");
        format!("{HEADER}\n{body}")
    }

    /// Atomically write to `path` (temp file in the same folder, then rename).
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Error::io(dir, e))?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, self.to_toml()).map_err(|e| Error::io(&tmp, e))?;
        std::fs::rename(&tmp, path).map_err(|e| Error::io(path, e))
    }

    pub fn entries(&self) -> &[OverrideEntry] {
        &self.entries
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Confirm that `file` is this track. Replaces earlier decisions for it.
    pub fn accept(&mut self, deezer_id: i64, file: PathBuf, info: TrackInfo) {
        self.force(deezer_id, Action::Accept, file, info);
    }

    /// Point this track at a file chosen by hand. Replaces earlier decisions.
    pub fn map(&mut self, deezer_id: i64, file: PathBuf, info: TrackInfo) {
        self.force(deezer_id, Action::Map, file, info);
    }

    fn force(&mut self, deezer_id: i64, action: Action, file: PathBuf, info: TrackInfo) {
        self.entries.retain(|e| {
            e.deezer_id != deezer_id
                || (e.action == Action::Reject
                    && e.file.is_some()
                    && e.file.as_ref() != Some(&file))
        });
        self.entries
            .push(entry(deezer_id, action, Some(file), info));
    }

    /// Reject `file` for this track, or with `None` mark it as not owned.
    pub fn reject(&mut self, deezer_id: i64, file: Option<PathBuf>, info: TrackInfo) {
        self.entries.retain(|e| {
            e.deezer_id != deezer_id
                || (e.action == Action::Reject && file.is_some() && e.file != file)
        });
        self.entries
            .push(entry(deezer_id, Action::Reject, file, info));
    }

    /// Forget every decision about this track.
    pub fn remove(&mut self, deezer_id: i64) {
        self.entries.retain(|e| e.deezer_id != deezer_id);
    }

    /// Decisions for one track (hand-edited files may contain conflicting
    /// entries; accept/map wins, then reject).
    pub fn for_track(&self, deezer_id: i64) -> TrackOverride<'_> {
        let mut out = TrackOverride::default();
        for e in self.entries.iter().filter(|e| e.deezer_id == deezer_id) {
            match (e.action, e.file.as_deref()) {
                (Action::Accept | Action::Map, Some(f)) if out.forced.is_none() => {
                    out.forced = Some((f, e.action));
                }
                (Action::Reject, Some(f)) => out.rejected_files.push(f),
                (Action::Reject, None) => out.reject_all = true,
                _ => {}
            }
        }
        out
    }

    /// A ready-to-paste snippet with a single entry (used by the report).
    pub fn snippet(
        deezer_id: i64,
        action: Action,
        file: Option<&Path>,
        info: &TrackInfo,
    ) -> String {
        let single = Overrides {
            version: FORMAT_VERSION,
            entries: vec![entry(
                deezer_id,
                action,
                file.map(Path::to_path_buf),
                info.clone(),
            )],
        };
        let text = toml::to_string_pretty(&single).expect("overrides always serialise");
        text.split_once("[[track]]")
            .map(|(_, rest)| format!("[[track]]{rest}"))
            .unwrap_or(text)
    }
}

fn entry(deezer_id: i64, action: Action, file: Option<PathBuf>, info: TrackInfo) -> OverrideEntry {
    OverrideEntry {
        deezer_id,
        action,
        file,
        note: info.note,
        artist: info.artist,
        title: info.title,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(title: &str) -> TrackInfo {
        TrackInfo {
            artist: Some("Vela Nine".into()),
            title: Some(title.into()),
            note: None,
        }
    }

    #[test]
    fn roundtrip_through_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub/overrides.toml");
        assert!(
            Overrides::load(&path).unwrap().is_empty(),
            "missing file = empty"
        );

        let mut o = Overrides::default();
        o.reject(20, None, info("Low Tide"));
        o.accept(10, "/m/Night Drive.flac".into(), info("Night Drive"));
        o.reject(10, Some("/m/other.mp3".into()), TrackInfo::default());
        o.save(&path).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.starts_with("# digshelf overrides"));
        let loaded = Overrides::load(&path).unwrap();
        let ids: Vec<_> = loaded
            .entries()
            .iter()
            .map(|e| (e.deezer_id, e.action))
            .collect();
        // reject(10, file) replaced the accept for 10 (a reject overrides it).
        assert_eq!(ids, [(10, Action::Reject), (20, Action::Reject)]);
        assert!(!dir.path().join("sub/overrides.toml.tmp").exists());
    }

    #[test]
    fn accept_replaces_earlier_decisions() {
        let mut o = Overrides::default();
        o.reject(1, None, TrackInfo::default());
        o.reject(1, Some("/m/a.mp3".into()), TrackInfo::default());
        o.reject(1, Some("/m/b.mp3".into()), TrackInfo::default());
        o.accept(1, "/m/a.mp3".into(), TrackInfo::default());
        let t = o.for_track(1);
        assert_eq!(t.forced, Some((Path::new("/m/a.mp3"), Action::Accept)));
        assert!(!t.reject_all);
        // Rejecting a different file is kept; rejecting the accepted one is not.
        assert_eq!(t.rejected_files, [Path::new("/m/b.mp3")]);
        assert!(o.for_track(2).is_empty());
        o.remove(1);
        assert!(o.is_empty());
    }

    #[test]
    fn hand_edited_conflicts_prefer_accept() {
        let o = Overrides::parse(
            r#"
            [[track]]
            deezer_id = 5
            action = "reject"

            [[track]]
            deezer_id = 5
            action = "map"
            file = "/m/x.flac"
            "#,
        )
        .unwrap();
        let t = o.for_track(5);
        assert_eq!(t.forced, Some((Path::new("/m/x.flac"), Action::Map)));
        assert!(t.reject_all, "kept, but forced wins in the matcher");
    }

    #[test]
    fn validation() {
        let err = Overrides::parse("[[track]]\ndeezer_id = 1\naction = \"accept\"").unwrap_err();
        assert!(err.contains("needs a `file`"), "{err}");
        assert!(Overrides::parse("version = 99").is_err());
        assert!(Overrides::parse("[[track]]\ndeezer_id = 1\naction = \"keep\"").is_err());
        assert!(
            Overrides::parse("[[track]]\ndeezer_id = 1\naction = \"reject\"\ntypo = 1").is_err()
        );
    }

    #[test]
    fn snippet_is_a_single_pasteable_entry() {
        let s = Overrides::snippet(
            7,
            Action::Accept,
            Some(Path::new("/m/a \"b\".mp3")),
            &info("X"),
        );
        assert!(s.starts_with("[[track]]\n"), "{s}");
        let parsed = Overrides::parse(&s).unwrap();
        assert_eq!(
            parsed.entries()[0].file.as_deref(),
            Some(Path::new("/m/a \"b\".mp3"))
        );
    }
}
