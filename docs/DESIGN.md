# digshelf design (M1, plus pre-M2 fixes)

## Workspace layout

```
Cargo.toml                 workspace (resolver 2), shared dependency versions
crates/core/               package `digshelf-core`, lib `digshelf_core`
  src/lib.rs
  src/error.rs             Error enum (thiserror)
  src/model.rs             DeezerTrack, Playlist, LocalTrack, MatchStatus, ...
  src/http.rs              Transport trait, reqwest impl, throttle + backoff
  src/cache.rs             SQLite cache (rusqlite, bundled)
  src/deezer.rs            input parsing (URLs/IDs), paginated API client
  src/normalize.rs         text normalisation, feat./version extraction
  src/library.rs           read-only scan (walkdir + lofty), incremental via cache
  src/matcher.rs           overrides, then ISRC, then fuzzy matching; confidence score
  src/overrides.rs         user accept/reject/map decisions (TOML file, GUI-writable)
  src/report.rs            report model (rows, album groups, stale overrides, unreadable files)
  src/stores.rs            Bandcamp / Qobuz / Beatport search URL builders
  src/export.rs            index.html + playlists/*.html (minijinja), missing.csv, .m3u8
  templates/               index.html, playlist.html, _style.html (embedded with include_str!)
  tests/fixtures/          synthetic Deezer JSON
crates/cli/                package `digshelf`, binary `digshelf` (clap, anyhow)
  examples/accuracy.rs     matcher accuracy check against a real library (output: gitignored)
src-tauri/                 M2 only; will depend on digshelf-core
```

`digshelf-core` has no clap/stdout/exit. Long operations take a
`&dyn Fn(Progress)` callback so the CLI prints and the future GUI can drive
a progress bar.

## Core module boundaries

| module     | depends on                   | responsibility |
|------------|------------------------------|----------------|
| http       | reqwest, tokio::time         | `Transport::get(url) -> (status, body)`; the reqwest impl throttles to ≤ 10 req/s (Deezer's quota is 50 per 5 s) |
| deezer     | http, cache, model           | parse inputs, paginate via `next`, retry with exponential backoff on Deezer error code 4 (quota), HTTP 429 and 5xx; `fetch_all` orchestrates playlists, favourites and user playlists; `enrich_tracks` adds ISRC, duration and contributors from `/track/{id}` |
| cache      | rusqlite                     | HTTP response cache + library scan cache; `clear()` |
| library    | walkdir, lofty, cache        | find audio files, read tags/duration **read-only**, reuse cached rows when mtime+size are unchanged |
| normalize  | deunicode                    | pure functions |
| matcher    | normalize, strsim, overrides | pure: `(deezer track, overrides for it) -> MatchResult` against an indexed library |
| overrides  | toml, serde                  | load and atomically save the overrides file; per-track lookup |
| report     | matcher, stores, overrides   | build the report model the exporters and the GUI render |
| stores     | reqwest::Url (re-export)     | pure URL builders |
| export     | minijinja, csv               | write files into the out dir only |

## SQLite schema

The cache lives at `~/Library/Caches/digshelf/cache.sqlite` by default
(`--cache-dir` overrides it). Its version is tracked with `PRAGMA user_version`
(currently **2**). Migrating v1→v2 rebuilds only `library_file` (one full rescan)
and keeps `http_cache`. Any other version mismatch starts over, because it is only a cache.

```sql
CREATE TABLE http_cache (
  url        TEXT PRIMARY KEY,   -- api.deezer.com URL (JSON only, never audio)
  body       TEXT NOT NULL,
  fetched_at INTEGER NOT NULL    -- unix seconds
);
CREATE TABLE library_file (
  path        TEXT PRIMARY KEY,  -- absolute path
  mtime       INTEGER NOT NULL,
  size        INTEGER NOT NULL,
  title       TEXT, artist TEXT, album TEXT,
  duration_ms INTEGER,
  isrc        TEXT,
  read_error  TEXT,              -- v2: why tags could not be read (NULL if fine)
  scanned_at  INTEGER NOT NULL
);
```

Cache TTL: `/track/{id}` responses never expire because ISRC, duration and
contributors are stable. List endpoints (playlists, user tracks and playlists) expire after 6 h;
`--refresh` ignores cached list responses. Library rows are reused when
mtime and size match, and rows for files that no longer exist are deleted after a scan.

## Matching algorithm

1. **Normalise** both sides by lowercasing, transliterating to ASCII (deunicode),
   replacing `&`/`+` with `and`, stripping punctuation and collapsing whitespace.
2. **Split the title** into `base`, `version` and `featured`:
   - `feat. X`, `ft. X`, `featuring X`, `with X` (in brackets or trailing) → `featured`
   - bracketed or ` - ` suffixes containing a version keyword (remix, mix, edit,
     dub, version, vip, rework, bootleg, live, instrumental, acoustic, radio,
     extended, club) → `version`
   - "remaster(ed)", "original mix" and "album version" count as **no version**,
     because they are the same recording for our purposes.
   - Artist strings are split on `,` `&` `and` `x` `vs` `feat.` into a set of names.
3. **Overrides first** (see below): accept/map force a file; reject removes
   a file from consideration or marks the track Missing.
   **Then ISRC**: if both sides have ISRCs (normalised to uppercase with no
   hyphens) and they are equal → confidence 1.0, method `isrc`, status Owned.
4. **Fuzzy**: candidate local tracks come from an inverted index on base-title
   tokens (the two rarest tokens of the Deezer title), which avoids an
   O(playlist × library) scan. Each candidate is scored as follows:
   - `title` = normalised Levenshtein similarity of base titles (Jaro-Winkler
     was tried first; its prefix bonus matched "I Can't Stay" to "I Can't Get No …")
   - `artist` = best similarity across the artist-name sets (1.0 if any
     name matches exactly). The Deezer set includes **contributors** (see below).
   - ties: higher confidence, then smaller duration difference, then the
     earliest path
   - `version` = 1.0 if both are empty or similar (≥ 0.85); 0.3 if only one side
     has a version (e.g. remix vs original); otherwise their similarity
   - `duration` = 1.0 within ±3 s, falling linearly to 0 at ±15 s; 0.7 if unknown
   - gates: title < 0.80 or artist < 0.70 → discarded
   - `confidence = 0.40·title + 0.25·artist + 0.20·version + 0.15·duration`
5. **Decision**:
   - Owned: ISRC match, or confidence ≥ 0.92 **and** duration within ±3 s
     (or unknown on one side and confidence ≥ 0.95)
   - Uncertain: best candidate confidence ≥ 0.70 → flagged for review and
     **not** written to the `.m3u8`
   - Missing: everything else
   The confidence and per-component scores are stored in the result and shown in the report.

## Contributor credits

List endpoints (`/playlist/{id}/tracks`, `/user/{id}/tracks`) give only the main
artist. `/track/{id}` also has `contributors` (featured and co-main artists).
digshelf **never makes extra requests for credits**. Tracks missing an ISRC are
fetched anyway (in practice, favourites). For every other track, a
`/track/{id}` response already in the cache is used if present. Contributor
names (excluding the main artist) join the Deezer artist set, so a duet whose
file is tagged under the other artist can still match.

## Overrides

User decisions live in `overrides.toml` in the config directory
(`~/Library/Application Support/digshelf/overrides.toml`, overridable with
`--overrides` or `overrides_file` in the config). They are never stored in the repo or the cache.

```toml
version = 1

[[track]]
deezer_id = 3135556
action = "accept"            # accept | map | reject
file = "/abs/path/to/file.flac"
note = "optional"
artist = "…"                 # informational only
title = "…"
```

| action | `file` | effect |
|---|---|---|
| accept | required | the proposed candidate is right → Owned (method `manual`), in the `.m3u8` |
| map    | required | a hand-picked file is this track → Owned (method `manual`) |
| reject | given    | never match that file to this track; matching reruns without it |
| reject | omitted  | the track is not in the library → Missing |

- Keyed by Deezer track ID, so a decision applies in every playlist the track appears in.
- Precedence: accept/map, then reject, then ISRC, then fuzzy. Conflicting hand edits resolve the same way.
- **Stale** accept/map entries (the file is no longer in the scan, e.g. after
  MediaMonkey moved it) fall back to automatic matching, print a warning, and are
  listed in `index.html`. Entries for tracks not in this run are ignored.
- GUI API (`digshelf_core::overrides::Overrides`): `load` (a missing file gives
  an empty set), `save` (temp file plus rename, entries sorted by Deezer ID),
  `accept`, `map`, `reject`, `remove`, `for_track`, and `snippet` for single
  entries. Saving rewrites the file, so hand-written comments are dropped (a
  fixed header comment is always written).
- The report shows paste-ready accept / reject-file / not-owned snippets under
  every uncertain row.

## Outputs

- `index.html`: totals, a table of playlists linking to their pages, stale
  overrides, and **Unreadable files** (path and error; these are matched by file
  name only).
- `playlists/<name>.html`: one page per playlist with Owned / Missing / Uncertain
  tabs. Missing tracks are grouped by album, with album- and track-level store
  links. Every page is self-contained: the CSS partial `_style.html` is inlined.
- `missing.csv`: one row per (playlist, missing-or-uncertain track).
- `<playlist>.m3u8`: UTF-8 without a BOM and LF endings, starting with `#EXTM3U`,
  then `#EXTINF:<secs>,<Artist> - <Title>` and the absolute path for each
  Owned track, in Deezer playlist order.

Store search URLs (checked in 2026-09):

| store    | album                                             | track |
|----------|---------------------------------------------------|-------|
| Bandcamp | `https://bandcamp.com/search?q=Q&item_type=a`     | `...&item_type=t` |
| Qobuz    | `https://www.qobuz.com/{locale}/search/?q=Q` (redirects to album results) | `https://www.qobuz.com/{locale}/search/tracks/Q` (path segment; `/` replaced with a space) |
| Beatport | `https://www.beatport.com/search/releases?q=Q`    | `https://www.beatport.com/search/tracks?q=Q` |

## Dependencies beyond the suggested set

- `thiserror`, `anyhow`: required by the error conventions.
- `serde_json`: to parse Deezer responses.
- `csv`: correct RFC 4180 quoting for `missing.csv`.
- `deunicode`: accent folding for matching (`Chloé` = `Chloe`). It is small and has no dependencies.
- `dirs` (cli): platform cache/config directories.
- `toml`: in the cli for the optional config file
  (`~/Library/Application Support/digshelf/config.toml`), and in core for the
  overrides file, so the CLI and the GUI read and write it through one API.
- `tempfile` (dev only): temporary directories in tests.
- `csv` (cli, dev only): sample files written by the accuracy example.

## Accuracy check

`crates/cli/examples/accuracy.rs` runs the full pipeline **without overrides**
and writes to a gitignored folder (default `out/accuracy`):

- `counts.txt`: unique Deezer tracks and playlist rows by owned-ISRC /
  owned-fuzzy / uncertain / missing
- `owned_fuzzy_sample.csv`, `uncertain_sample.csv`: seeded random samples
  (Deezer side vs file path, tags and duration, with every score)
- `duration_only.txt`: uncertain matches that would be Owned if the duration
  matched, with a histogram of the duration differences

```sh
cargo run --release -p digshelf --example accuracy -- \
  --music ~/Music --user <id> --out out/accuracy
```

Its output contains personal library data and must never be committed.
