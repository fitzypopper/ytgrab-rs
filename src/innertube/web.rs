//! The watch page.
//!
//! Some clients (the TV app, for instance) only work when the request
//! carries a visitor id and a signature timestamp. Both come from the watch
//! page, which also points at the player JavaScript we need for the `n`
//! challenge.

use reqwest::Client;
use serde_json::Value;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Default)]
pub struct WatchPage {
    /// The visitor id, sent as `X-Goog-Visitor-Id`.
    pub visitor_data: String,
    /// The player signature timestamp (`STS`).
    pub signature_timestamp: i64,
    /// Absolute URL of the player JavaScript.
    pub player_js_url: String,
    /// The InnerTube API key.
    pub api_key: String,
    /// The web client's version.
    pub client_version: String,
}

/// Fetch the watch page and pull the interesting bits out of `ytcfg`.
pub async fn fetch(http: &Client, video_id: &str) -> Result<WatchPage> {
    let url = format!("https://www.youtube.com/watch?v={video_id}&hl=en");
    let html = http
        .get(&url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36",
        )
        .header("Accept-Language", "en-US,en;q=0.9")
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;

    let cfg = extract_ytcfg(&html)?;

    let visitor_data = cfg
        .get("INNERTUBE_CONTEXT")
        .and_then(|c| c.get("client"))
        .and_then(|c| c.get("visitorData"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    let player_js = cfg
        .get("PLAYER_JS_URL")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let player_js_url = if player_js.is_empty() {
        String::new()
    } else if player_js.starts_with("http") {
        player_js.to_string()
    } else {
        format!("https://www.youtube.com{player_js}")
    };

    Ok(WatchPage {
        visitor_data,
        signature_timestamp: cfg.get("STS").and_then(|v| v.as_i64()).unwrap_or(0),
        player_js_url,
        api_key: cfg
            .get("INNERTUBE_API_KEY")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        client_version: cfg
            .get("INNERTUBE_CLIENT_VERSION")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    })
}

/// Find the `ytcfg.set({...})` objects in the page and merge them.
///
/// The page contains several `ytcfg.set(...)` calls; only some of them take
/// an object, and the configuration is spread over more than one of them.
fn extract_ytcfg(html: &str) -> Result<Value> {
    const MARKER: &str = "ytcfg.set(";

    let mut merged = serde_json::Map::new();
    let mut found = false;
    let mut offset = 0usize;

    while let Some(pos) = html[offset..].find(MARKER) {
        let start = offset + pos + MARKER.len();
        offset = start;

        // Only the object form holds configuration.
        let body_start = match html[start..].find('{') {
            Some(relative) if html[start..start + relative].trim().is_empty() => start + relative,
            _ => continue,
        };

        let Some(end) = find_json_end(html, body_start) else {
            continue;
        };

        let Ok(value) = serde_json::from_str::<Value>(&html[body_start..end]) else {
            continue;
        };
        if let Some(map) = value.as_object() {
            for (k, v) in map {
                merged.insert(k.clone(), v.clone());
            }
            found = true;
        }
        offset = end;
    }

    if !found {
        return Err(Error::other(
            "the watch page did not contain any configuration (YouTube may be showing a consent page)",
        ));
    }
    Ok(Value::Object(merged))
}

/// The index just past the JSON value that starts at `start`.
fn find_json_end(html: &str, start: usize) -> Option<usize> {
    let bytes = html.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;

    for (i, b) in bytes[start..].iter().enumerate() {
        let c = *b as char;
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth -= 1;
                if depth == 0 {
                    return Some(start + i + 1);
                }
            }
            _ => {}
        }
    }
    None
}

/// Download the player JavaScript.
pub async fn fetch_player_js(http: &Client, url: &str) -> Result<String> {
    if url.is_empty() {
        return Err(Error::other("no player script URL in the watch page"));
    }
    let js = http
        .get(url)
        .header(
            "User-Agent",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36",
        )
        .send()
        .await?
        .error_for_status()?
        .text()
        .await?;
    Ok(js)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_ytcfg_blob() {
        let html = r#"<html>ytcfg.set({"INNERTUBE_API_KEY":"AIza","STS":20761,"PLAYER_JS_URL":"/s/player/x/base.js","INNERTUBE_CONTEXT":{"client":{"visitorData":"Cgt"}},"INNERTUBE_CLIENT_VERSION":"2.2026"});</html>"#;
        let cfg = extract_ytcfg(html).unwrap();
        assert_eq!(cfg.get("STS").unwrap(), 20761);
        assert_eq!(cfg.get("INNERTUBE_API_KEY").unwrap(), "AIza");
        assert_eq!(
            cfg.get("INNERTUBE_CONTEXT")
                .unwrap()
                .get("client")
                .unwrap()
                .get("visitorData")
                .unwrap(),
            "Cgt"
        );
    }

    #[test]
    fn handles_strings_with_braces() {
        let html = r#"ytcfg.set({"A":"}{","B":2});"#;
        let cfg = extract_ytcfg(html).unwrap();
        assert_eq!(cfg.get("A").unwrap(), "}{");
        assert_eq!(cfg.get("B").unwrap(), 2);
    }

    #[test]
    fn rejects_pages_without_config() {
        assert!(extract_ytcfg("<html>no config here</html>").is_err());
    }

    #[test]
    fn skips_non_object_calls_and_merges_objects() {
        let html = r#"<html>
            ytcfg.set('EMERGENCY_BASE_URL', '\/error_204');
            ytcfg.set({"INNERTUBE_API_KEY":"AIza"});
            ytcfg.set("initialInnerWidth",window.innerWidth);
            ytcfg.set({"STS":20761});
            </html>"#;
        let cfg = extract_ytcfg(html).unwrap();
        assert_eq!(cfg.get("INNERTUBE_API_KEY").unwrap(), "AIza");
        assert_eq!(cfg.get("STS").unwrap(), 20761);
    }
}
