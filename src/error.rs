//! Error types shared across the crate.

use std::fmt;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("malformed response: {0}")]
    Json(#[from] serde_json::Error),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),

    #[error("this video is unavailable: {0}")]
    Unavailable(String),

    #[error("unsupported url: {0}")]
    UnsupportedUrl(String),

    #[error("no matching format for selector `{0}`")]
    NoFormat(String),

    #[error("ffmpeg failed: {0}")]
    Ffmpeg(String),

    #[error("database error: {0}")]
    Db(#[from] rusqlite::Error),

    #[error("javascript error: {0}")]
    Js(#[from] rquickjs::Error),

    /// googlevideo refused the request. Usually the range was too large or
    /// the URL was refused outright; a fresh player request may help.
    #[error("googlevideo rejected the request (HTTP {status}) for format {itag}")]
    SignedUrl { status: u16, itag: i64 },

    #[error("{0}")]
    Other(String),
}

impl Error {
    pub fn other(msg: impl fmt::Display) -> Self {
        Error::Other(msg.to_string())
    }
}
