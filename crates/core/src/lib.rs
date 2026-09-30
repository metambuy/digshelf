//! digshelf-core: find DRM-free purchase options for Deezer playlists and
//! compare them against a read-only local library.
//!
//! This crate has no CLI or GUI concerns; see `crates/cli` for the binary.

pub mod cache;
pub mod deezer;
pub mod error;
pub mod export;
pub mod http;
pub mod library;
pub mod matcher;
pub mod model;
pub mod normalize;
pub mod overrides;
pub mod report;
pub mod stores;

pub use error::{Error, Result};
