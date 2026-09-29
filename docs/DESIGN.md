# digshelf design (M1)

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
  src/matcher.rs           ISRC + fuzzy matching, confidence score
  src/stores.rs            Bandcamp / Qobuz / Beatport search URL builders
  src/export/              report.html (minijinja), missing.csv, .m3u8
  templates/report.html    embedded with include_str!
  tests/fixtures/          synthetic Deezer JSON
crates/cli/                package `digshelf`, binary `digshelf` (clap, anyhow)
src-tauri/                 M2 only; will depend on digshelf-core
```

`digshelf-core` has no clap/stdout/exit. Long operations take a
`&dyn Fn(Progress)` callback so the CLI prints and the future GUI can drive
a progress bar.

## Core module boundaries

| module     | depends on                   | responsibility |
|------------|------------------------------|----------------|
| http       | reqwest, tokio::time         | `Transport::get(url) -> (status, body)`; the reqwest impl throttles to ≤ 10 req/s (Deezer's quota is 50 per 5 s) |
| deezer     | http, cache, model           | parse inputs, paginate via `next`, retry with exponential backoff on Deezer error code 4 (quota), HTTP 429 and 5xx, fill missing ISRCs from `/track/{id}` |
| cache      | rusqlite                     | HTTP response cache + library scan cache; `clear()` |
| library    | walkdir, lofty, cache        | find audio files, read tags/duration **read-only**, reuse cached rows when mtime+size are unchanged |
| normalize  | deunicode                    | pure functions |
| matcher    | normalize, strsim            | pure: `(deezer tracks, local tracks) -> Vec<MatchResult>` |
| stores     | reqwest::Url (re-export)     | pure URL builders |
| export     | minijinja, csv               | write files into the out dir only |

## SQLite schema

The cache lives at `~/Library/Caches/digshelf/cache.sqlite` by default
(`--cache-dir` overrides it). Its version is tracked with `PRAGMA user_version`.

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
  scanned_at  INTEGER NOT NULL
);
```

Cache TTL: `/track/{id}` responses never expire because ISRC and duration are
stable. List endpoints (playlists, user tracks and playlists) expire after 6 h;
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
3. **ISRC first**: if both sides have ISRCs (normalised to uppercase with no
   hyphens) and they are equal → confidence 1.0, method `isrc`, status Owned.
4. **Fuzzy**: candidate local tracks come from an inverted index on base-title
   tokens (the two rarest tokens of the Deezer title), which avoids an
   O(playlist × library) scan. Each candidate is scored as follows:
   - `title` = Jaro-Winkler(base titles)
   - `artist` = best Jaro-Winkler across the artist-name sets (1.0 if any
     name matches exactly)
   - `version` = 1.0 if both are empty or similar (≥ 0.85); 0.3 if only one side
     has a version (e.g. remix vs original); otherwise Jaro-Winkler
   - `duration` = 1.0 within ±3 s, falling linearly to 0 at ±15 s; 0.7 if unknown
   - gates: title < 0.80 or artist < 0.60 → discarded
   - `confidence = 0.40·title + 0.25·artist + 0.20·version + 0.15·duration`
5. **Decision**:
   - Owned: ISRC match, or confidence ≥ 0.92 **and** duration within ±3 s
     (or unknown on one side and confidence ≥ 0.95)
   - Uncertain: best candidate confidence ≥ 0.70 → flagged for review and
     **not** written to the `.m3u8`
   - Missing: everything else
   The confidence and per-component scores are stored in the result and shown in the report.

## Outputs

- `report.html`: self-contained (inline CSS + a little JS), with one section per
  playlist and Owned / Missing / Uncertain tabs. Missing tracks are grouped by
  album, with album- and track-level store links.
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
- `toml` (cli): optional config file (`~/Library/Application Support/digshelf/config.toml`)
  so the MediaMonkey-managed folder does not need to be passed every run.
- `tempfile` (dev only): temporary directories in tests.
