use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Args, Parser, Subcommand};
use serde::Deserialize;

use digshelf_core::cache::Cache;
use digshelf_core::deezer::{parse_playlist_id, parse_user_id, DeezerClient};
use digshelf_core::export::write_all;
use digshelf_core::http::ReqwestTransport;
use digshelf_core::library;
use digshelf_core::matcher::MatchConfig;
use digshelf_core::model::Progress;
use digshelf_core::report::{build_report, ReportOptions};
use digshelf_core::stores::DEFAULT_QOBUZ_LOCALE;

/// Find DRM-free purchase options for your Deezer playlists and compare them
/// with your local (MediaMonkey-managed) library. Never downloads audio.
#[derive(Parser)]
#[command(name = "digshelf", version, about)]
struct Cli {
    /// Config file [default: <config dir>/digshelf/config.toml]
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Cache directory [default: <cache dir>/digshelf]
    #[arg(long, global = true, value_name = "DIR")]
    cache_dir: Option<PathBuf>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Fetch playlists, scan the library and write report.html, missing.csv and .m3u8 files.
    Scan(ScanArgs),
    /// Manage the local metadata cache.
    Cache {
        #[command(subcommand)]
        command: CacheCommand,
    },
}

#[derive(Subcommand)]
enum CacheCommand {
    /// Delete all cached Deezer responses and library scan results.
    Clear,
}

#[derive(Args)]
struct ScanArgs {
    /// Music folder managed by MediaMonkey (read-only) [default: config `music_dir`, else ~/Music]
    #[arg(long, value_name = "DIR")]
    music: Option<PathBuf>,

    /// Deezer playlist URLs or IDs (repeatable)
    #[arg(long = "playlist", value_name = "URL|ID", num_args = 1..)]
    playlists: Vec<String>,

    /// Public Deezer user ID or profile URL: adds favourites and the user's playlists
    #[arg(long, value_name = "ID|URL")]
    user: Option<String>,

    /// Output directory [default: config `out_dir`, else ./out]
    #[arg(long, value_name = "DIR")]
    out: Option<PathBuf>,

    /// Ignore cached playlist listings and fetch them again
    #[arg(long)]
    refresh: bool,

    /// Qobuz storefront for links, e.g. gb-en, fr-fr [default: config, else us-en]
    #[arg(long, value_name = "LOCALE")]
    qobuz_locale: Option<String>,
}

/// Optional config file. Every field can be overridden on the command line.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    music_dir: Option<String>,
    out_dir: Option<String>,
    user: Option<String>,
    #[serde(default)]
    playlists: Vec<String>,
    qobuz_locale: Option<String>,
}

fn expand_tilde(p: &str) -> PathBuf {
    match (p.strip_prefix("~/"), dirs::home_dir()) {
        (Some(rest), Some(home)) => home.join(rest),
        _ if p == "~" => dirs::home_dir().unwrap_or_else(|| PathBuf::from(p)),
        _ => PathBuf::from(p),
    }
}

fn load_config(path: Option<&Path>) -> Result<Config> {
    let (path, explicit) = match path {
        Some(p) => (p.to_path_buf(), true),
        None => match dirs::config_dir() {
            Some(d) => (d.join("digshelf").join("config.toml"), false),
            None => return Ok(Config::default()),
        },
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            toml::from_str(&text).with_context(|| format!("invalid config {}", path.display()))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound && !explicit => Ok(Config::default()),
        Err(e) => Err(e).with_context(|| format!("cannot read config {}", path.display())),
    }
}

fn cache_path(cli: &Cli) -> Result<PathBuf> {
    let dir = match &cli.cache_dir {
        Some(d) => d.clone(),
        None => dirs::cache_dir()
            .context("no cache directory on this platform; pass --cache-dir")?
            .join("digshelf"),
    };
    Ok(dir.join("cache.sqlite"))
}

fn print_progress(p: Progress) {
    let mut err = std::io::stderr();
    let _ = match p {
        Progress::FetchingPlaylist { id } => write!(err, "\r\x1b[2KFetching playlist {id}…"),
        Progress::FetchingUser { id } => {
            writeln!(err, "\r\x1b[2KFetching favourites and playlists of user {id}…")
        }
        Progress::SkippedPlaylist { title, error } => {
            writeln!(err, "\r\x1b[2K  skipping {title:?}: {error}")
        }
        Progress::FetchedPlaylist { title, tracks } => {
            writeln!(err, "\r\x1b[2K  {title}: {tracks} tracks")
        }
        Progress::FetchingIsrc { done, total } if done == total => {
            writeln!(err, "\r\x1b[2KLooked up ISRCs for {total} tracks")
        }
        Progress::FetchingIsrc { done, total } => {
            write!(err, "\r\x1b[2KLooking up ISRCs {done}/{total}…")
        }
        Progress::ScanningLibrary { files_seen } => {
            write!(err, "\r\x1b[2KScanning library: {files_seen} files…")
        }
        Progress::ScannedLibrary {
            files,
            read,
            cached,
            failed,
        } => writeln!(
            err,
            "\r\x1b[2KLibrary: {files} audio files ({read} read, {cached} from cache, {failed} unreadable)"
        ),
    };
}

async fn scan(cli: &Cli, args: &ScanArgs) -> Result<()> {
    let config = load_config(cli.config.as_deref())?;
    let music = args
        .music
        .clone()
        .or_else(|| config.music_dir.as_deref().map(expand_tilde))
        .or_else(|| dirs::home_dir().map(|h| h.join("Music")))
        .context("no music folder; pass --music")?;
    let out = args
        .out
        .clone()
        .or_else(|| config.out_dir.as_deref().map(expand_tilde))
        .unwrap_or_else(|| PathBuf::from("out"));
    let qobuz_locale = args
        .qobuz_locale
        .clone()
        .or(config.qobuz_locale)
        .unwrap_or_else(|| DEFAULT_QOBUZ_LOCALE.to_string());
    let playlist_inputs = if args.playlists.is_empty() {
        config.playlists
    } else {
        args.playlists.clone()
    };
    let user_input = args.user.clone().or(config.user);
    if playlist_inputs.is_empty() && user_input.is_none() {
        bail!("nothing to do: pass --playlist <URL|ID> and/or --user <ID> (or set them in the config file)");
    }

    // Validate every input before touching the network.
    let mut playlist_ids = Vec::new();
    for input in &playlist_inputs {
        playlist_ids.push(parse_playlist_id(input)?);
    }
    let user_id = user_input.as_deref().map(parse_user_id).transpose()?;

    let mut cache = Cache::open(&cache_path(cli)?)?;
    let playlists = {
        let transport = ReqwestTransport::new()?;
        let mut client = DeezerClient::new(transport, &cache);
        if args.refresh {
            client = client.with_list_ttl(Some(Duration::ZERO));
        }
        client
            .fetch_all(&playlist_ids, user_id, &print_progress)
            .await?
    };

    eprintln!("Scanning {} (read-only)…", music.display());
    let scan = library::scan(&music, &mut cache, &print_progress)
        .with_context(|| format!("cannot scan music folder {}", music.display()))?;

    let report = build_report(
        &playlists,
        &scan.tracks,
        &ReportOptions {
            match_config: MatchConfig::default(),
            qobuz_locale: &qobuz_locale,
        },
    );
    let written = write_all(&report, &out)?;

    eprintln!();
    for pl in &report.playlists {
        eprintln!(
            "  {:<40} {:>4} owned {:>4} uncertain {:>4} missing",
            truncate(&pl.title, 40),
            pl.owned,
            pl.uncertain,
            pl.missing
        );
    }
    eprintln!(
        "\nTotal: {} owned, {} uncertain, {} missing",
        report.owned, report.uncertain, report.missing
    );
    println!("{}", written.report.display());
    println!("{}", written.csv.display());
    for p in &written.playlists {
        println!("{}", p.display());
    }
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max - 1).collect();
        t.push('…');
        t
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    match &cli.command {
        Command::Scan(args) => scan(&cli, args).await,
        Command::Cache {
            command: CacheCommand::Clear,
        } => {
            let path = cache_path(&cli)?;
            Cache::open(&path)?.clear()?;
            eprintln!("Cleared cache at {}", path.display());
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cli_parses_scan() {
        let cli = Cli::try_parse_from([
            "digshelf",
            "scan",
            "--music",
            "/lib",
            "--playlist",
            "1",
            "--playlist",
            "https://www.deezer.com/playlist/2",
            "--user",
            "3",
            "--out",
            "o",
        ])
        .unwrap();
        let Command::Scan(a) = cli.command else {
            panic!()
        };
        assert_eq!(a.playlists.len(), 2);
        assert_eq!(a.music.as_deref(), Some(Path::new("/lib")));
    }

    #[test]
    fn cli_parses_cache_clear() {
        assert!(Cli::try_parse_from(["digshelf", "cache", "clear"]).is_ok());
    }

    #[test]
    fn config_parses() {
        let c: Config = toml::from_str(
            "music_dir = \"~/Music/Library\"\nuser = \"3\"\nplaylists = [\"1\"]\nqobuz_locale = \"gb-en\"",
        )
        .unwrap();
        assert_eq!(c.playlists, ["1"]);
        assert!(toml::from_str::<Config>("unknown = 1").is_err());
    }
}
