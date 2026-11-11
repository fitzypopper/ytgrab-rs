//! ytgrab — a YouTube downloader written in Rust.

pub mod browser_cookies;
pub mod cli;
pub mod cookies;
pub mod download;
pub mod error;
pub mod formats;
pub mod innertube;
pub mod js;
pub mod mux;
pub mod output;
pub mod runner;
pub mod subtitles;
pub mod url;

pub use error::{Error, Result};
