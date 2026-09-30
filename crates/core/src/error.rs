use std::path::PathBuf;

/// Errors produced by `digshelf-core`.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("HTTP request to {url} failed: {message}")]
    Http { url: String, message: String },

    #[error("Deezer API error {code} ({kind}): {message}")]
    DeezerApi {
        code: i64,
        kind: String,
        message: String,
    },

    #[error("cannot fetch {what}: {source}")]
    Fetch {
        what: String,
        #[source]
        source: Box<Error>,
    },

    #[error("Deezer rate limit still exceeded after {attempts} attempts")]
    RateLimited { attempts: u32 },

    #[error("could not parse Deezer input {input:?}: expected a playlist URL or numeric ID")]
    InvalidInput { input: String },

    #[error("unexpected response from {url}: {source}")]
    Json {
        url: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("cache database error: {0}")]
    Cache(#[from] rusqlite::Error),

    #[error("I/O error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("template error: {0}")]
    Template(#[from] minijinja::Error),

    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        Error::Io {
            path: path.into(),
            source,
        }
    }
}
