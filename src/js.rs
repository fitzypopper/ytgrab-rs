//! Solving YouTube's `n` (throttling) and `sig` (signature) challenges.
//!
//! Some InnerTube clients hand back URLs whose `n` parameter is a puzzle
//! instead of a value. The answer is computed by a function inside YouTube's
//! player JavaScript, so the only way to solve it is to run that JavaScript.
//!
//! Instead of requiring `deno`, `node` or `bun` on the user's machine (which
//! is what yt-dlp does), we embed [QuickJS](https://bellard.org/quickjs/) via
//! the `rquickjs` crate. The solver itself is the Unlicense code from
//! [yt-dlp/ejs](https://github.com/yt-dlp/ejs), vendored in `vendor/ejs/`.
//!
//! It works statically on the player's *source*: the solver parses
//! `base.js`, finds the function that fills in the `n` and `sig` query
//! parameters of a `URL` object, rewrites it into a standalone function and
//! calls it with the challenge. No browser environment is involved.

use rquickjs::{Context, Function, Runtime};

use crate::error::{Error, Result};

const LIB_JS: &str = include_str!("../vendor/ejs/lib.min.js");
const CORE_JS: &str = include_str!("../vendor/ejs/core.min.js");

/// Solve every `n` challenge in one go.
///
/// `player_js` is the contents of YouTube's `base.js`. The returned vector
/// has one entry per challenge, in the same order.
///
/// This blocks for a few hundred milliseconds (parsing the player script
/// dominates), so call it from a blocking thread.
pub fn solve_challenges(player_js: &str, challenges: &[String]) -> Result<Vec<String>> {
    if challenges.is_empty() {
        return Ok(Vec::new());
    }

    let runtime = Runtime::new()?;
    let context = Context::full(&runtime)?;

    // The library script defines `lib`; the core script uses the `meriyah`
    // and `astring` globals it provides.
    context.with(|ctx| {
        ctx.eval::<(), _>(LIB_JS)?;
        ctx.eval::<(), _>("Object.assign(globalThis, lib);")?;
        ctx.eval::<(), _>(CORE_JS)?;
        Ok::<(), rquickjs::Error>(())
    })?;

    let payload = serde_json::json!({
        "type": "player",
        "player": player_js,
        "requests": [{
            "type": "n",
            "challenges": challenges,
        }],
    });

    let raw = context.with(|ctx| {
        let global = ctx.globals();
        let jsc: Function = global
            .get("jsc")
            .map_err(|e| Error::other(format!("no solver in the JS runtime: {e}")))?;

        let json = serde_json::to_string(&payload)
            .map_err(|e| Error::other(format!("could not encode the solver input: {e}")))?;
        let out: String = jsc
            .call((json,))
            .map_err(|e| Error::other(format!("the solver failed: {e}")))?;

        Ok::<String, Error>(out)
    })?;

    parse_solver_output(&raw, challenges)
}

/// Pull the solved values out of the solver's JSON reply.
fn parse_solver_output(raw: &str, challenges: &[String]) -> Result<Vec<String>> {
    let doc: serde_json::Value =
        serde_json::from_str(raw).map_err(|e| Error::other(format!("bad solver output: {e}")))?;

    let responses = doc
        .get("responses")
        .and_then(|r| r.as_array())
        .ok_or_else(|| Error::other("the solver returned no responses"))?;

    for response in responses {
        if response.get("type").and_then(|t| t.as_str()) == Some("error") {
            let message = response
                .get("error")
                .and_then(|e| e.as_str())
                .unwrap_or("unknown error");
            return Err(Error::other(format!("the solver failed: {message}")));
        }
    }

    let data = responses
        .first()
        .and_then(|r| r.get("data"))
        .and_then(|d| d.as_object())
        .ok_or_else(|| Error::other("the solver returned no solutions"))?;

    let mut out = Vec::with_capacity(challenges.len());
    for challenge in challenges {
        let value = data
            .get(challenge)
            .and_then(|v| v.as_str())
            .ok_or_else(|| Error::other(format!("no solution for challenge {challenge}")))?;
        out.push(value.to_string());
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn solver_loads() {
        // Loading the scripts must not blow up; this also proves the
        // vendored files are intact.
        let runtime = Runtime::new().unwrap();
        let context = Context::full(&runtime).unwrap();
        context
            .with(|ctx| {
                ctx.eval::<(), _>(LIB_JS).unwrap();
                ctx.eval::<(), _>("Object.assign(globalThis, lib);").unwrap();
                ctx.eval::<(), _>(CORE_JS).unwrap();
                Ok::<(), rquickjs::Error>(())
            })
            .unwrap();
    }

    #[test]
    fn parses_solver_output() {
        let raw = r#"{"type":"result","responses":[{"type":"result","data":{"abc":"xyz"}}]}"#;
        let out = parse_solver_output(raw, &["abc".to_string()]).unwrap();
        assert_eq!(out, vec!["xyz".to_string()]);
    }

    #[test]
    fn reports_solver_errors() {
        let raw = r#"{"type":"result","responses":[{"type":"error","error":"boom"}]}"#;
        assert!(parse_solver_output(raw, &["abc".to_string()]).is_err());
    }
}
