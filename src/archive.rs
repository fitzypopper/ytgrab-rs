//! The download archive.
//!
//! A plain text file that lists the videos that were already downloaded, so
//! a second run over the same playlist skips them. The format is the same as
//! yt-dlp's:
//!
//! ```text
//! youtube dQw4w9WgXcQ Never Gonna Give You Up
//! ```

use std::collections::HashSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::Result;

/// The extractor name written in front of every entry.
const EXTRACTOR: &str = "youtube";

#[derive(Debug)]
pub struct Archive {
    path: PathBuf,
    /// Interior mutability so the archive can be shared as `&Archive`.
    entries: std::cell::RefCell<HashSet<String>>,
}

impl Archive {
    /// Read an archive file. A missing file is not an error: it just means
    /// nothing has been downloaded yet.
    pub fn load(path: &Path) -> Result<Self> {
        let entries = match std::fs::read_to_string(path) {
            Ok(content) => content
                .lines()
                .filter_map(|line| {
                    let line = line.trim();
                    if line.is_empty() || line.starts_with('#') {
                        return None;
                    }
                    // "<extractor> <id> <title...>"
                    let mut parts = line.splitn(3, ' ');
                    let extractor = parts.next()?;
                    let id = parts.next()?;
                    if extractor != EXTRACTOR {
                        return None;
                    }
                    Some(id.to_string())
                })
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashSet::new(),
            Err(e) => return Err(e.into()),
        };

        Ok(Self {
            path: path.to_path_buf(),
            entries: std::cell::RefCell::new(entries),
        })
    }

    /// Has this video already been downloaded?
    pub fn contains(&self, video_id: &str) -> bool {
        self.entries.borrow().contains(video_id)
    }

    /// Add a video to the archive.
    ///
    /// The entry is written and flushed straight away, so an interrupted run
    /// does not lose the information.
    pub fn record(&self, video_id: &str, title: &str) -> Result<()> {
        if self.entries.borrow().contains(video_id) {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        writeln!(file, "{EXTRACTOR} {video_id} {title}")?;
        file.flush()?;
        self.entries.borrow_mut().insert(video_id.to_string());
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.borrow().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.borrow().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("ytgrab-archive-{name}-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join("archive.txt")
    }

    #[test]
    fn a_missing_file_is_empty() {
        let path = temp("missing");
        let _ = std::fs::remove_file(&path);
        let archive = Archive::load(&path).unwrap();
        assert!(archive.is_empty());
        assert!(!archive.contains("abc"));
    }

    #[test]
    fn records_and_reads_back() {
        let path = temp("record");
        let _ = std::fs::remove_file(&path);

        let archive = Archive::load(&path).unwrap();
        archive.record("dQw4w9WgXcQ", "Never Gonna Give You Up").unwrap();
        assert!(archive.contains("dQw4w9WgXcQ"));

        // A second run reads the file and sees the entry.
        let reloaded = Archive::load(&path).unwrap();
        assert!(reloaded.contains("dQw4w9WgXcQ"));
        assert_eq!(reloaded.len(), 1);
    }

    #[test]
    fn ignores_other_extractors_and_comments() {
        let path = temp("other");
        std::fs::write(
            &path,
            "# a comment\nyoutube abc Some title\nvimeo xyz Other\n\n",
        )
        .unwrap();
        let archive = Archive::load(&path).unwrap();
        assert!(archive.contains("abc"));
        assert!(!archive.contains("xyz"));
    }
}
