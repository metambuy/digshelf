//! Matcher accuracy check against a real library. Writes counts, random
//! samples for hand verification and a duration-difference histogram.
//!
//! Output contains personal library data: write it to a gitignored folder.
//!
//!     cargo run --release -p digshelf --example accuracy -- \
//!         --music ~/Music --user <id> --out out/accuracy

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::Parser;

use digshelf_core::cache::Cache;
use digshelf_core::deezer::{parse_playlist_id, parse_user_id, DeezerClient};
use digshelf_core::http::ReqwestTransport;
use digshelf_core::library;
use digshelf_core::matcher::{MatchConfig, W_DURATION};
use digshelf_core::model::{MatchMethod, MatchStatus};
use digshelf_core::report::{build_report, ReportOptions, TrackRow};

#[derive(Parser)]
struct Args {
    #[arg(long)]
    music: PathBuf,
    #[arg(long = "playlist", num_args = 1..)]
    playlists: Vec<String>,
    #[arg(long)]
    user: Option<String>,
    #[arg(long, default_value = "out/accuracy")]
    out: PathBuf,
    #[arg(long)]
    cache_dir: Option<PathBuf>,
    /// Rows per sample file.
    #[arg(long, default_value_t = 50)]
    sample: usize,
    #[arg(long, default_value_t = 42)]
    seed: u64,
}

/// Tiny deterministic PRNG (xorshift64*), to avoid a `rand` dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn sample<T: Clone>(&mut self, items: &[T], n: usize) -> Vec<T> {
        let mut idx: Vec<usize> = (0..items.len()).collect();
        let n = n.min(idx.len());
        for i in 0..n {
            let j = i + (self.next() % (idx.len() - i) as u64) as usize;
            idx.swap(i, j);
        }
        idx[..n].iter().map(|&i| items[i].clone()).collect()
    }
}

fn kind(row: &TrackRow) -> &'static str {
    match (row.status, row.matched.as_ref().map(|m| m.method)) {
        (MatchStatus::Owned, Some(MatchMethod::Isrc)) => "owned_isrc",
        (MatchStatus::Owned, _) => "owned_fuzzy",
        (MatchStatus::Uncertain, _) => "uncertain",
        (MatchStatus::Missing, _) => "missing",
    }
}

fn write_sample(path: &PathBuf, rows: &[TrackRow]) -> Result<()> {
    let mut w = csv::Writer::from_path(path)?;
    w.write_record([
        "deezer_id",
        "deezer_artist",
        "deezer_title",
        "deezer_duration_s",
        "deezer_isrc",
        "file",
        "tag_artist",
        "tag_title",
        "file_duration_s",
        "file_isrc",
        "confidence",
        "title_score",
        "artist_score",
        "version_score",
        "duration_score",
        "delta_s",
    ])?;
    let opt = |v: Option<String>| v.unwrap_or_default();
    for r in rows {
        let t = &r.track;
        let m = r.matched.as_ref();
        let f = m.map(|m| &m.file);
        let s = m.and_then(|m| m.scores);
        let f2 = |x: f64| format!("{x:.3}");
        w.write_record([
            t.id.to_string(),
            t.artist.clone(),
            t.title.clone(),
            opt(t.duration.map(|d| d.to_string())),
            opt(t.isrc.clone()),
            opt(f.map(|f| f.path.to_string_lossy().into_owned())),
            opt(f.and_then(|f| f.artist.clone())),
            opt(f.and_then(|f| f.title.clone())),
            opt(f.and_then(|f| f.duration_secs()).map(|d| d.to_string())),
            opt(f.and_then(|f| f.isrc.clone())),
            opt(m.map(|m| f2(m.confidence))),
            opt(s.map(|s| f2(s.title))),
            opt(s.map(|s| f2(s.artist))),
            opt(s.map(|s| f2(s.version))),
            opt(s.map(|s| f2(s.duration))),
            opt(s.and_then(|s| s.duration_delta).map(|d| d.to_string())),
        ])?;
    }
    w.flush()?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args = Args::parse();
    let playlist_ids = args
        .playlists
        .iter()
        .map(|p| parse_playlist_id(p))
        .collect::<Result<Vec<_>, _>>()?;
    let user_id = args.user.as_deref().map(parse_user_id).transpose()?;
    let cache_dir = match args.cache_dir {
        Some(d) => d,
        None => dirs::cache_dir().context("no cache dir")?.join("digshelf"),
    };
    let mut cache = Cache::open(&cache_dir.join("cache.sqlite"))?;

    let playlists = {
        let client = DeezerClient::new(ReqwestTransport::new()?, &cache);
        client.fetch_all(&playlist_ids, user_id, &|_| {}).await?
    };
    let scan = library::scan(&args.music, &mut cache, &|_| {})?;
    let config = MatchConfig::default();
    let report = build_report(
        &playlists,
        &scan.tracks,
        &ReportOptions {
            match_config: config,
            qobuz_locale: "us-en",
        },
    );

    // One row per unique Deezer track (the same track appears in many playlists).
    let mut unique: BTreeMap<i64, TrackRow> = BTreeMap::new();
    let mut row_counts: HashMap<&str, usize> = HashMap::new();
    for row in report.playlists.iter().flat_map(|p| &p.rows) {
        *row_counts.entry(kind(row)).or_default() += 1;
        unique.entry(row.track.id).or_insert_with(|| row.clone());
    }
    let unique: Vec<TrackRow> = unique.into_values().collect();
    let mut unique_counts: HashMap<&str, usize> = HashMap::new();
    for row in &unique {
        *unique_counts.entry(kind(row)).or_default() += 1;
    }

    std::fs::create_dir_all(&args.out)?;
    let mut counts = String::new();
    let with_credits = unique
        .iter()
        .filter(|r| !r.track.contributors.is_empty())
        .count();
    writeln!(
        counts,
        "playlists: {}\nlibrary files: {} ({} unreadable)\nunique Deezer tracks: {} ({} with contributor credits)\nplaylist rows: {}\n",
        report.playlists.len(),
        scan.tracks.len(),
        scan.unreadable.len(),
        unique.len(),
        with_credits,
        report.playlists.iter().map(|p| p.rows.len()).sum::<usize>()
    )?;
    writeln!(
        counts,
        "{:<14} {:>8} {:>7} {:>8}",
        "method", "unique", "%", "rows"
    )?;
    for k in ["owned_isrc", "owned_fuzzy", "uncertain", "missing"] {
        let u = unique_counts.get(k).copied().unwrap_or(0);
        writeln!(
            counts,
            "{:<14} {:>8} {:>6.1}% {:>8}",
            k,
            u,
            100.0 * u as f64 / unique.len().max(1) as f64,
            row_counts.get(k).copied().unwrap_or(0)
        )?;
    }
    std::fs::write(args.out.join("counts.txt"), &counts)?;

    let mut rng = Rng(args.seed.max(1));
    let owned_fuzzy: Vec<TrackRow> = unique
        .iter()
        .filter(|r| kind(r) == "owned_fuzzy")
        .cloned()
        .collect();
    let uncertain: Vec<TrackRow> = unique
        .iter()
        .filter(|r| r.status == MatchStatus::Uncertain)
        .cloned()
        .collect();
    write_sample(
        &args.out.join("owned_fuzzy_sample.csv"),
        &rng.sample(&owned_fuzzy, args.sample),
    )?;
    write_sample(
        &args.out.join("uncertain_sample.csv"),
        &rng.sample(&uncertain, args.sample),
    )?;

    // Uncertain only because of duration: would be owned with a perfect duration score.
    let mut hist: BTreeMap<u32, usize> = BTreeMap::new();
    let mut duration_only = 0;
    for r in &uncertain {
        let Some(m) = &r.matched else { continue };
        let Some(s) = m.scores else { continue };
        let Some(delta) = s.duration_delta else {
            continue;
        };
        let ideal = m.confidence + W_DURATION * (1.0 - s.duration);
        if delta > config.duration_tolerance_secs && ideal >= config.owned_threshold {
            duration_only += 1;
            *hist.entry(delta.min(16)).or_default() += 1;
        }
    }
    let mut h = format!(
        "uncertain (unique): {}\nfailing only on duration (Δ > {} s, otherwise ≥ {}): {}\n\n",
        uncertain.len(),
        config.duration_tolerance_secs,
        config.owned_threshold,
        duration_only
    );
    writeln!(h, "{:>6} {:>6} {:>7}", "Δ s", "count", "%")?;
    for (d, n) in &hist {
        let label = if *d >= 16 {
            ">15".to_string()
        } else {
            d.to_string()
        };
        let pct = 100.0 * *n as f64 / duration_only.max(1) as f64;
        writeln!(
            h,
            "{label:>6} {n:>6} {pct:>6.1}% {}",
            "#".repeat((pct / 2.0).round() as usize)
        )?;
    }
    std::fs::write(args.out.join("duration_only.txt"), &h)?;

    print!("{counts}\n{h}");
    eprintln!("written to {}", args.out.display());
    Ok(())
}
