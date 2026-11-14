//! Fetching the comments of a video.
//!
//! The comments live in the `next` endpoint, but YouTube keeps changing the
//! shape of the payload, so this walks the response tree and picks out the
//! objects that look like comments rather than modelling one fixed layout.
//!
//! When the server does not hand out the comments at all (which happens
//! for some videos and clients), the caller is told so instead of being
//! given an empty file.

use serde_json::Value;

use crate::error::Result;
use crate::innertube::client::InnerTube;

/// One comment.
#[derive(Debug, Clone)]
pub struct Comment {
    pub id: String,
    pub author: String,
    pub text: String,
    pub likes: Option<String>,
    pub published: Option<String>,
    pub replies: Vec<Comment>,
}

/// Fetch the comments of a video.
pub async fn fetch(it: &InnerTube, video_id: &str) -> Result<Vec<Comment>> {
    let mut body = it.context_object().await;
    body["videoId"] = serde_json::json!(video_id);

    let raw = it.post("next", body).await?;
    Ok(collect(&raw))
}

/// Walk the tree and collect the comment-like objects.
fn collect(value: &Value) -> Vec<Comment> {
    let mut out = Vec::new();
    walk(value, &mut out);
    out
}

fn walk(value: &Value, out: &mut Vec<Comment>) {
    match value {
        Value::Object(map) => {
            if let Some(comment) = as_comment(map) {
                out.push(comment);
                return;
            }
            for v in map.values() {
                walk(v, out);
            }
        }
        Value::Array(items) => {
            for v in items {
                walk(v, out);
            }
        }
        _ => {}
    }
}

/// Does this object look like a comment?
fn as_comment(map: &serde_json::Map<String, Value>) -> Option<Comment> {
    // The modern payload keeps the text in `contentText` or `commentText`.
    let text = map
        .get("contentText")
        .or_else(|| map.get("commentText"))
        .or_else(|| map.get("text"))
        .and_then(text_of)?;

    let author = map
        .get("authorText")
        .or_else(|| map.get("author"))
        .and_then(text_of)
        .unwrap_or_default();

    // A comment without an author and without an id is something else.
    let id = map
        .get("commentId")
        .or_else(|| map.get("id"))
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();

    if author.is_empty() && id.is_empty() {
        return None;
    }
    // The reply thread of another comment is not a comment of its own.
    if map.contains_key("replyCount") && !map.contains_key("commentText") && !map.contains_key("contentText")
    {
        return None;
    }

    let likes = map
        .get("voteCount")
        .or_else(|| map.get("likeCount"))
        .and_then(text_of);

    let published = map
        .get("publishedTimeText")
        .or_else(|| map.get("publishedTime"))
        .and_then(text_of);

    let mut replies = Vec::new();
    if let Some(Value::Array(items)) = map.get("replies") {
        for item in items {
            walk(item, &mut replies);
        }
    }

    Some(Comment {
        id,
        author,
        text,
        likes,
        published,
        replies,
    })
}

/// The text of a field, whether it is a string or an object with runs.
fn text_of(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Object(map) => {
            if let Some(runs) = map.get("runs").and_then(|r| r.as_array()) {
                let text: String = runs
                    .iter()
                    .filter_map(|r| r.get("text").and_then(|t| t.as_str()))
                    .collect();
                if !text.is_empty() {
                    return Some(text);
                }
            }
            map.get("simpleText")
                .and_then(|v| v.as_str())
                .map(str::to_string)
        }
        _ => None,
    }
}

/// The comments as JSON, for writing to a file.
pub fn to_json(comments: &[Comment]) -> String {
    fn to_value(comment: &Comment) -> Value {
        serde_json::json!({
            "id": comment.id,
            "author": comment.author,
            "text": comment.text,
            "likes": comment.likes,
            "published": comment.published,
            "replies": comment.replies.iter().map(to_value).collect::<Vec<_>>(),
        })
    }
    serde_json::to_string_pretty(&comments.iter().map(to_value).collect::<Vec<_>>())
        .unwrap_or_else(|_| "[]".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn finds_comments_in_the_tree() {
        let payload = json!({
            "contents": {
                "commentThreadRenderer": {
                    "comment": {
                        "commentRenderer": {
                            "commentId": "abc",
                            "authorText": {"runs": [{"text": "Someone"}]},
                            "contentText": {"runs": [{"text": "Nice video"}]},
                            "voteCount": {"simpleText": "42"},
                            "publishedTimeText": {"runs": [{"text": "2 days ago"}]},
                        }
                    }
                }
            }
        });
        let comments = collect(&payload);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].author, "Someone");
        assert_eq!(comments[0].text, "Nice video");
        assert_eq!(comments[0].likes.as_deref(), Some("42"));
    }

    #[test]
    fn ignores_other_objects() {
        let payload = json!({"somethingElse": {"title": "Not a comment"}});
        assert!(collect(&payload).is_empty());
    }

    #[test]
    fn collects_replies() {
        let payload = json!({
            "comment": {
                "commentId": "a",
                "authorText": {"simpleText": "A"},
                "contentText": {"simpleText": "parent"},
                "replies": [
                    {"commentId": "b", "authorText": {"simpleText": "B"}, "contentText": {"simpleText": "reply"}}
                ]
            }
        });
        let comments = collect(&payload);
        assert_eq!(comments.len(), 1);
        assert_eq!(comments[0].replies.len(), 1);
        assert_eq!(comments[0].replies[0].text, "reply");
    }
}
