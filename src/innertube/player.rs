//! `player` endpoint: everything we know about a single video.

use serde_json::json;

use crate::error::{Error, Result};
use crate::innertube::client::InnerTube;
use crate::innertube::models::{Format, PlayerResponse, StreamingData};

/// Fetch the player payload for a video id.
pub async fn fetch(it: &InnerTube, video_id: &str) -> Result<PlayerResponse> {
    let mut body = it.context_object().await;
    body["videoId"] = json!(video_id);
    body["contentCheckOk"] = json!(true);
    body["racyCheckOk"] = json!(true);
    // `playbackContext` is only accepted by the web-flavoured clients; the
    // ANDROID client rejects the whole request with a 400 when it is present.
    if matches!(it.context().name, "WEB" | "MWEB") {
        body["playbackContext"] = json!({
            "contentPlaybackContext": {
                "html5Preference": "HTML5_PREF_WANTS"
            }
        });
    }

    let raw = it.post("player", body).await?;
    parse(raw)
}

/// Fetch the player payload using the TV client.
///
/// The TV client hands out URLs whose `n` parameter is a challenge rather
/// than a value; those have to be solved with the player's JavaScript before
/// the URL can be used.
pub async fn fetch_tv(it: &InnerTube, video_id: &str) -> Result<PlayerResponse> {
    let mut body = it.context_object().await;
    body["videoId"] = json!(video_id);
    body["contentCheckOk"] = json!(true);
    body["racyCheckOk"] = json!(true);
    body["playbackContext"] = json!({
        "contentPlaybackContext": {
            "html5Preference": "HTML5_PREF_WANTS",
            "signatureTimestamp": it.context().signature_timestamp.unwrap_or(0),
        }
    });

    let raw = it.post("player", body).await?;
    parse(raw)
}

fn parse(raw: serde_json::Value) -> Result<PlayerResponse> {
    let parsed: PlayerResponse = serde_json::from_value(raw)?;

    if !parsed.playability_status.is_ok() {
        return Err(Error::Unavailable(parsed.playability_status.message()));
    }
    if parsed
        .streaming_data
        .as_ref()
        .map(|s| s.formats.is_empty() && s.adaptive_formats.is_empty())
        .unwrap_or(true)
    {
        return Err(Error::Unavailable(
            "no playable streams in the player response".into(),
        ));
    }
    Ok(parsed)
}

/// All formats (progressive + adaptive) flattened into one list.
pub fn all_formats(streaming: &StreamingData) -> Vec<Format> {
    let mut v = streaming.formats.clone();
    v.extend(streaming.adaptive_formats.iter().cloned());
    v
}

// ---------------------------------------------------------------------------
// the `n` challenge
// ---------------------------------------------------------------------------

/// The throttling challenge in a URL, if there is one.
pub fn n_challenge(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    parsed
        .query_pairs()
        .find(|(k, _)| k == "n")
        .map(|(_, v)| v.into_owned())
}

/// Put a solved challenge back into a URL.
pub fn set_n_challenge(url: &str, solved: &str) -> String {
    let Ok(mut parsed) = url::Url::parse(url) else {
        return url.to_string();
    };
    let pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .filter(|(k, _)| k != "n")
        .collect();
    parsed.set_query(None);
    {
        let mut q = parsed.query_pairs_mut();
        for (k, v) in &pairs {
            q.append_pair(k, v);
        }
        q.append_pair("n", solved);
    }
    parsed.to_string()
}

/// Solve the `n` challenge of every format that has one.
///
/// This runs the player's JavaScript, which blocks for a moment, so it is
/// done on a blocking thread.
pub fn solve_n_challenges(formats: &mut [Format], player_js: &str) -> Result<()> {
    let mut challenges = Vec::new();
    for f in formats.iter() {
        if let Some(url) = &f.url {
            if let Some(n) = n_challenge(url) {
                challenges.push(n);
            }
        }
    }
    if challenges.is_empty() {
        return Ok(());
    }

    let solved = crate::js::solve_challenges(player_js, &challenges)?;
    let mut iter = solved.into_iter();
    for f in formats.iter_mut() {
        if let Some(url) = &f.url {
            if n_challenge(url).is_some() {
                if let Some(value) = iter.next() {
                    f.url = Some(set_n_challenge(url, &value));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes_the_challenge() {
        let url = "https://example.com/videoplayback?itag=137&n=abc&sig=xyz";
        assert_eq!(n_challenge(url).as_deref(), Some("abc"));

        let solved = set_n_challenge(url, "solved");
        assert_eq!(n_challenge(&solved).as_deref(), Some("solved"));
        assert!(solved.contains("itag=137"));
        assert!(solved.contains("sig=xyz"));
    }

    #[test]
    fn urls_without_a_challenge_are_left_alone() {
        assert_eq!(n_challenge("https://example.com/x?a=1"), None);
    }
}
