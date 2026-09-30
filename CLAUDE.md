# digshelf

## Purpose

Find DRM-free purchase options for the tracks in a user's Deezer playlists and
favourites, compare them against a local music library, and (later) help manage
the purchased files.

The library is managed by **MediaMonkey** (macOS). digshelf is a read-only
companion to it: it reports what is owned, missing or uncertain, and writes
playlists that MediaMonkey and Mixxx can import.

## Hard rules

These are non-negotiable. If a feature request conflicts with one, stop and ask.

- **Never download, stream, decrypt or cache audio** from Deezer or any store.
  Deezer `preview` URLs are ignored and never fetched.
- **Deezer:** only the public, unauthenticated `https://api.deezer.com` endpoints.
  No private/gateway APIs, no ARL tokens, no cookies, no scraping of deezer.com.
- **Stores (Bandcamp, Qobuz, Beatport):** build search URLs only. No scraping,
  no automated checkout, and never handle payment data.
- **Respect rate limits.** Throttle requests, back off on quota/5xx errors and
  cache responses in SQLite.
- **The library is read-only.** Never write tags, rename, move or delete library
  files. Never read or write MediaMonkey's database. Never write to Mixxx's
  database. Output goes only to the `--out` directory and the digshelf cache.
- **This is a public repo.** No personal paths, library contents, user IDs or
  real playlist data in commits. Test fixtures are synthetic (fictional artists,
  made-up IDs) or trimmed public catalogue responses. Output of runs against a
  real library (e.g. the accuracy example) goes to `out/` (gitignored) only.
  Never derive test strings from the user's library.

## Conventions

- Rust stable, edition 2021. `cargo fmt` and `cargo clippy --all-targets -- -D warnings`
  must be clean before committing.
- Errors: `thiserror` in `crates/core`, `anyhow` in `crates/cli`.
- Tests use recorded fixtures under `crates/core/tests/fixtures/`. **No live
  network in tests**: HTTP goes through the `Transport` trait, which tests stub.
- `crates/core` holds all logic and knows nothing about the CLI or GUI (no clap,
  no printing to stdout, no process exit). Progress is reported via callbacks.
- Small, PR-sized commits with clear imperative messages.

## Layout

```
crates/core   digshelf-core: Deezer client, cache, library scan, matcher,
              overrides (user decisions), report model, exporters
crates/cli    digshelf binary (clap); examples/accuracy.rs for local accuracy checks
src-tauri/    (M2, not yet created) GUI that reuses digshelf-core
```

## Roadmap

- **M1 — CLI (current):** read Deezer playlists/favourites, scan the library,
  match, and write `index.html` + `playlists/*.html`, `missing.csv` and one
  `.m3u8` per playlist. User decisions come from `overrides.toml` in the config dir.
- **M2 — Tauri GUI:** `src-tauri/` app on top of `digshelf-core`; review and
  accept/reject uncertain matches.
- **M3 — Post-purchase pipeline:** watch the Downloads folder, unzip and verify
  purchases (format, tags present, readable), then move them into a
  configurable **inbox folder that MediaMonkey monitors**. MediaMonkey does the
  renaming and filing. digshelf then re-scans, re-matches and regenerates the
  playlists. digshelf still never renames or tags files inside the library.
