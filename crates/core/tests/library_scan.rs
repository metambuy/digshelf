//! Library scan tests on synthetic WAV files generated in a temp dir.

use std::fs;
use std::path::Path;

use digshelf_core::cache::Cache;
use digshelf_core::library::scan;
use digshelf_core::model::Progress;
use lofty::config::WriteOptions;
use lofty::prelude::*;
use lofty::tag::{Tag, TagType};

/// Write a silent 16-bit mono PCM WAV of `secs` seconds at 8 kHz.
fn write_wav(path: &Path, secs: u32) {
    let rate = 8000u32;
    let data_len = rate * 2 * secs;
    let mut b = Vec::with_capacity(44 + data_len as usize);
    b.extend_from_slice(b"RIFF");
    b.extend_from_slice(&(36 + data_len).to_le_bytes());
    b.extend_from_slice(b"WAVEfmt ");
    b.extend_from_slice(&16u32.to_le_bytes());
    b.extend_from_slice(&1u16.to_le_bytes()); // PCM
    b.extend_from_slice(&1u16.to_le_bytes()); // mono
    b.extend_from_slice(&rate.to_le_bytes());
    b.extend_from_slice(&(rate * 2).to_le_bytes());
    b.extend_from_slice(&2u16.to_le_bytes());
    b.extend_from_slice(&16u16.to_le_bytes());
    b.extend_from_slice(b"data");
    b.extend_from_slice(&data_len.to_le_bytes());
    b.resize(44 + data_len as usize, 0);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b).unwrap();
}

/// Test setup only: tag a temp file (the scanner itself never writes).
fn tag(path: &Path, title: &str, artist: &str, isrc: Option<&str>) {
    let mut t = Tag::new(TagType::Id3v2);
    t.set_title(title.into());
    t.set_artist(artist.into());
    t.set_album("Fixture Album".into());
    if let Some(i) = isrc {
        t.insert_text(ItemKey::Isrc, i.into());
    }
    t.save_to_path(path, WriteOptions::default()).unwrap();
}

#[test]
fn scans_tags_duration_and_uses_cache() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let a = root.join("Vela Nine/Coastal Lights/01 Night Drive.wav");
    let b = root.join("Loose/Mira Sol - Glass Rooms.wav");
    write_wav(&a, 3);
    tag(&a, "Night Drive", "Vela Nine", Some("XXAAA2600001"));
    write_wav(&b, 2);
    fs::write(root.join("cover.jpg"), b"not audio").unwrap();
    fs::create_dir_all(root.join(".hidden")).unwrap();
    write_wav(&root.join(".hidden/skip.wav"), 1);
    fs::write(root.join("broken.mp3"), b"garbage").unwrap();

    let before_a = fs::read(&a).unwrap();
    let mut cache = Cache::open_in_memory().unwrap();
    let events = std::cell::RefCell::new(Vec::new());
    let res = scan(root, &mut cache, &|p| events.borrow_mut().push(p)).unwrap();

    assert_eq!(res.tracks.len(), 3, "{:?}", res.tracks);
    let night = res.tracks.iter().find(|t| t.path == a).unwrap();
    assert_eq!(night.title.as_deref(), Some("Night Drive"));
    assert_eq!(night.artist.as_deref(), Some("Vela Nine"));
    assert_eq!(night.isrc.as_deref(), Some("XXAAA2600001"));
    assert_eq!(night.duration_secs(), Some(3));
    let untagged = res.tracks.iter().find(|t| t.path == b).unwrap();
    assert_eq!(untagged.title, None);
    assert_eq!(untagged.duration_secs(), Some(2));
    assert_eq!(res.unreadable.len(), 1);
    assert!(res.unreadable[0].0.ends_with("broken.mp3"));

    // Read-only: the scanned file is byte-identical afterwards.
    assert_eq!(fs::read(&a).unwrap(), before_a);

    // Second scan reuses every cached row.
    let events = std::cell::RefCell::new(Vec::new());
    let res2 = scan(root, &mut cache, &|p| events.borrow_mut().push(p)).unwrap();
    assert_eq!(res2.tracks, res.tracks);
    assert!(events.borrow().contains(&Progress::ScannedLibrary {
        files: 3,
        read: 0,
        cached: 3,
        failed: 0
    }));

    // Deleted files disappear from results and cache.
    fs::remove_file(&b).unwrap();
    let res3 = scan(root, &mut cache, &|_| {}).unwrap();
    assert_eq!(res3.tracks.len(), 2);
    assert_eq!(cache.library_files().unwrap().len(), 2);
}

#[test]
fn missing_root_is_an_error() {
    let mut cache = Cache::open_in_memory().unwrap();
    assert!(scan(Path::new("/definitely/not/here"), &mut cache, &|_| {}).is_err());
}
