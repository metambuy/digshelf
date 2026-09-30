# digshelf

Find DRM-free purchase options for the tracks in your Deezer playlists and
favourites, and see which ones you already own.

digshelf reads your public Deezer playlists, scans your local music library
**read-only**, matches the two, and writes:

- **`index.html`**: an overview of every playlist, plus a list of library
  files whose tags could not be read.
- **`playlists/<name>.html`**: one page per playlist with **Owned / Missing /
  Uncertain** tabs. Missing tracks are grouped by album, with search links for
  **Bandcamp**, **Qobuz** and **Beatport**. Every page is self-contained.
- **`missing.csv`**: every missing or uncertain track with its store links.
- **One `.m3u8` per playlist**, listing the files you already own, ready to
  import into **MediaMonkey** or **Mixxx**.

(Versions before the split wrote a single `report.html`. You can delete an old
one from your output folder.)

It is designed as a companion to [MediaMonkey](https://www.mediamonkey.com/),
which owns file organisation and tagging.

## What digshelf deliberately does *not* do

- **No downloading, streaming, decrypting or caching of audio**, from Deezer or
  any store. Deezer preview URLs are ignored.
- **No private Deezer APIs**: only the public, unauthenticated
  `api.deezer.com` endpoints. No login, ARL tokens, cookies or scraping. Your
  playlists and favourites must be public.
- **No store scraping or checkout.** Store links are plain search URLs that
  you open in your browser. digshelf never handles payment data.
- **No changes to your library.** It never writes tags or renames, moves or
  deletes files. It never reads or writes MediaMonkey's or Mixxx's database.
  It writes only to the `--out` folder and its own cache.

## Install

Requires a stable [Rust toolchain](https://rustup.rs/).

```sh
git clone https://github.com/metambuy/digshelf.git
cd digshelf
cargo install --path crates/cli
```

## Usage

```sh
digshelf scan \
  --music ~/Music/Library \
  --playlist https://www.deezer.com/en/playlist/1234567890 \
  --playlist 9876543210 \
  --user 12345678 \
  --out ./out
```

| option | meaning |
|---|---|
| `--music DIR` | The folder MediaMonkey manages. Scanned read-only. Default: `music_dir` from the config, else `~/Music`. |
| `--playlist URL\|ID` | Public Deezer playlist. Repeatable, or pass several after one flag. |
| `--user ID\|URL` | Public Deezer user: adds their favourites (`/user/{id}/tracks`) and all their public playlists. |
| `--out DIR` | Output folder (default `./out`). |
| `--refresh` | Re-fetch playlist listings instead of using the cache (6-hour TTL). |
| `--overrides FILE` | Your match decisions (default: `~/Library/Application Support/digshelf/overrides.toml`). See [Overrides](#overrides). |
| `--qobuz-locale` | Qobuz storefront for links, e.g. `gb-en`, `fr-fr` (default `us-en`). |
| `--cache-dir DIR` | Where the SQLite cache lives (default: `~/Library/Caches/digshelf`). |

Your Deezer user ID is the number in your profile URL
(`https://www.deezer.com/profile/<id>`). Favourites and playlists must be set
to public in Deezer's privacy settings.

Clear the cache (Deezer responses and library scan results):

```sh
digshelf cache clear
```

### Config file

To avoid retyping paths, create
`~/Library/Application Support/digshelf/config.toml` (or pass `--config FILE`).
Command-line flags take precedence.

```toml
music_dir    = "~/Music/Library"   # the folder MediaMonkey manages
out_dir      = "~/Desktop/digshelf"
user         = "12345678"
playlists    = ["https://www.deezer.com/en/playlist/1234567890"]
qobuz_locale = "gb-en"
overrides_file = "~/Documents/digshelf-overrides.toml"   # optional
```

## How matching works

1. **ISRC:** if a file's ISRC tag equals the Deezer track's ISRC, it is owned.
   Deezer ISRCs missing from list endpoints are fetched from `/track/{id}` and
   cached. Those responses also list contributing artists, which are used for
   matching. digshelf never makes extra requests just for credits.
2. **Fuzzy:** otherwise titles and artists are normalised (case, accents,
   punctuation, `&`/"and"). `feat.` credits are separated out, and
   remix/edit/version suffixes are compared as a separate field. "Remastered"
   and "Original Mix" are ignored. Duration must be within **±3 s**.
3. Each match gets a **confidence score**. High-confidence matches are
   *Owned*. Plausible but doubtful ones (e.g. a remix vs the original, or a
   duration that differs by more than 3 s) are *Uncertain* and are **not**
   added to the `.m3u8`. Review them in the report, then either fix tags in
   MediaMonkey or record your decision as an [override](#overrides), and re-run.

Library scans are incremental: only new or changed files (by mtime and size)
are read again.

See [`docs/DESIGN.md`](docs/DESIGN.md) for the details.

## Overrides

When the matcher is unsure, or wrong, record your decision in
`~/Library/Application Support/digshelf/overrides.toml`. The file is created
only by you, or later by the GUI. It is never part of the repo. Decisions are
keyed by Deezer track ID and apply in every playlist that contains the track.

Each uncertain row in the report has a **Decide** box with three ready-made
snippets. Paste one into the file and re-run `digshelf scan`:

```toml
version = 1

# Same track: count it as owned (and write it to the .m3u8).
[[track]]
deezer_id = 3135556
action = "accept"
file = "/Users/you/Music/Artist/Album/01 Title.flac"

# Wrong file: never match this file to this track (another may still match).
[[track]]
deezer_id = 3135557
action = "reject"
file = "/Users/you/Music/Other/Title (Live).mp3"

# Not owned: treat the track as missing.
[[track]]
deezer_id = 3135558
action = "reject"
```

Use `action = "map"` with a `file` to point a track at a file the matcher never
suggested. If MediaMonkey later moves a file, the entry becomes *stale*:
digshelf warns you, lists it in `index.html`, and falls back to automatic
matching until you update the path. When digshelf (or the GUI) saves this file,
it rewrites it and drops comments you added by hand.

## Importing the playlists

The `.m3u8` files are extended M3U: UTF-8, `#EXTM3U` header,
`#EXTINF:<seconds>,Artist - Title`, and one **absolute path** per track. Only
*Owned* tracks are listed, in Deezer order. Because paths are absolute, you can
import them from anywhere. Keep the library at the same location, and re-run
digshelf after MediaMonkey moves files.

### MediaMonkey

1. In the left tree, right-click **Playlists** and choose **Import playlist…**
   (or drag the `.m3u8` file onto the **Playlists** node).
2. Choose the `.m3u8` file(s) from the output folder.
3. The tracks are already in your library, so MediaMonkey links them by path.

When you re-run digshelf, delete the previously imported playlist before
importing the new file, to avoid duplicates.

### Mixxx

1. In the library sidebar, right-click **Playlists** and choose
   **Import Playlist** (for a crate instead: right-click **Crates** and choose
   **Import Crate**).
2. Choose the `.m3u8` file(s) from the output folder.
3. If Mixxx cannot open the files, add your music folder under
   **Preferences → Library → Music Directories** (on macOS Mixxx needs
   permission for that folder).

digshelf never writes to Mixxx's database. Importing is always a manual step.

## Development

```sh
cargo fmt
cargo clippy --all-targets -- -D warnings
cargo test
```

Tests never touch the network: HTTP goes through a `Transport` trait that
tests replay from synthetic fixtures in `crates/core/tests/fixtures/`.

To check matcher accuracy against your own library (counts, random samples to
verify by hand, and a histogram of duration differences), run:

```sh
cargo run --release -p digshelf --example accuracy -- \
  --music ~/Music --user <your Deezer user ID> --out out/accuracy
```

The output contains your library data. `out/` is gitignored, so keep it there.
See [`CLAUDE.md`](CLAUDE.md) for project rules and the roadmap.

## Roadmap

- **M1: CLI** (this release)
- **M2: Tauri GUI** on top of `digshelf-core`, including reviewing uncertain matches
- **M3: Post-purchase pipeline.** Watch Downloads, unzip and verify purchases,
  move them into an inbox folder that MediaMonkey monitors (MediaMonkey does the
  renaming and filing), then re-scan and update the playlists.

## License

MIT
