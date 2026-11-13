//! A cache of the session data the TV client needs.
//!
//! Getting a usable TV client request costs a watch page (1.3 MB), the
//! player script (2.6 MB) and a parse of that script (~3 s). None of it
//! changes between runs, so it is cached on disk.
//!
//! The visitor id can go stale, so a cached session that makes the server
//! answer `LOGIN_REQUIRED` is discarded and fetched again.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::error::Result;

/// How long a cached session is considered fresh.
const MAX_AGE_SECONDS: u64 = 24 * 60 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub visitor_data: String,
    pub signature_timestamp: i64,
    pub player_js_url: String,
    /// The player script itself, so it does not have to be downloaded again.
    pub player_js: String,
    /// The player script, reduced to the part that solves challenges.
    pub preprocessed: String,
    #[serde(default)]
    pub fetched_at: u64,
}

impl Session {
    pub fn is_fresh(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        now.saturating_sub(self.fetched_at) < MAX_AGE_SECONDS
    }
}

/// The on disk cache.
pub struct SessionCache {
    dir: PathBuf,
}

impl SessionCache {
    /// The cache directory, or `None` when there is no cache to speak of.
    pub fn open() -> Option<Self> {
        let dir = dirs::cache_dir()?.join("ytgrab");
        std::fs::create_dir_all(&dir).ok()?;
        Some(Self { dir })
    }

    fn path(&self) -> PathBuf {
        self.dir.join("session.json")
    }

    /// Read the cached session, if there is a fresh one.
    pub fn load(&self) -> Option<Session> {
        let content = std::fs::read_to_string(self.path()).ok()?;
        let session: Session = serde_json::from_str(&content).ok()?;
        if session.is_fresh() {
            Some(session)
        } else {
            None
        }
    }

    /// Store a session.
    pub fn save(&self, session: &mut Session) -> Result<()> {
        session.fetched_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let content = serde_json::to_string(session)
            .map_err(|e| crate::error::Error::other(format!("{e}")))?;
        // Write to a temporary file first so a crash cannot leave a broken
        // cache behind.
        let tmp = self.dir.join("session.json.tmp");
        std::fs::write(&tmp, content)?;
        std::fs::rename(&tmp, self.path())?;
        Ok(())
    }

    /// Throw the cached session away.
    pub fn clear(&self) {
        let _ = std::fs::remove_file(self.path());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        Session {
            visitor_data: "Cgt".into(),
            signature_timestamp: 20761,
            player_js_url: "https://example.com/base.js".into(),
            player_js: "var a = 1;".into(),
            preprocessed: "var b = 2;".into(),
            fetched_at: 0,
        }
    }

    #[test]
    fn a_fresh_session_survives_a_round_trip() {
        let dir = std::env::temp_dir().join(format!("ytgrab-cache-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let cache = SessionCache { dir: dir.clone() };
        let _ = std::fs::remove_file(cache.path());

        assert!(cache.load().is_none(), "nothing cached yet");

        let mut s = session();
        cache.save(&mut s).unwrap();
        assert!(s.fetched_at > 0);

        let loaded = cache.load().expect("the session should be cached");
        assert_eq!(loaded.visitor_data, "Cgt");
        assert_eq!(loaded.preprocessed, "var b = 2;");
        assert!(loaded.is_fresh());

        cache.clear();
        assert!(cache.load().is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_old_session_is_not_fresh() {
        let mut s = session();
        s.fetched_at = 0;
        assert!(!s.is_fresh());
    }
}
