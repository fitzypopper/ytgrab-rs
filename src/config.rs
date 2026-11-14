//! A configuration file.
//!
//! The file holds command line options, one per line:
//!
//! ```text
//! # comments start with a hash
//! --format bestvideo[height<=1080]+bestaudio
//! --output "%(title)s.%(ext)s"
//! --download-archive archive.txt
//! ```
//!
//! Options given on the command line win, because the command line is
//! applied after the file.

use std::ffi::OsString;

/// Split the argument list into the program's own arguments and the user's.
///
/// The result is `program`, then the options from the config file, then the
/// options the user typed.
pub fn splice(
    program: OsString,
    args: Vec<OsString>,
    config_path: &std::path::Path,
) -> Vec<OsString> {
    let mut out = vec![program];
    let mut user_args = Vec::new();

    let mut iter = args.into_iter().peekable();
    while let Some(arg) = iter.next() {
        let text = arg.to_string_lossy().to_string();
        if text == "--config" || text == "-C" {
            // The value is the next argument; it has already been used.
            let _ = iter.next();
            continue;
        }
        if text.starts_with("--config=") {
            continue;
        }
        user_args.push(arg);
    }

    if let Ok(content) = std::fs::read_to_string(config_path) {
        out.extend(parse(&content));
    }
    out.extend(user_args);
    out
}

/// Turn the lines of a config file into arguments.
pub fn parse(content: &str) -> Vec<OsString> {
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // A bare word without a dash is treated as a long option, the way
        // yt-dlp does it.
        let line = if line.starts_with("--") {
            line.to_string()
        } else {
            format!("--{line}")
        };

        // Split "key = value" into "--key" "value".
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            let value = value.trim().trim_matches('"').trim_matches('\'');
            out.push(OsString::from(key));
            out.push(OsString::from(value));
        } else {
            // "--key value" on one line.
            let mut parts = line.splitn(2, char::is_whitespace);
            if let Some(key) = parts.next() {
                out.push(OsString::from(key.trim()));
                if let Some(value) = parts.next() {
                    let value = value.trim().trim_matches('"').trim_matches('\'');
                    if !value.is_empty() {
                        out.push(OsString::from(value));
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(args: &[OsString]) -> Vec<String> {
        args.iter().map(|a| a.to_string_lossy().to_string()).collect()
    }

    #[test]
    fn parses_lines() {
        let args = parse("--format best\n--output \"a.%(ext)s\"\n# a comment\n\n--quiet");
        assert_eq!(
            text(&args),
            vec!["--format", "best", "--output", "a.%(ext)s", "--quiet"]
        );
    }

    #[test]
    fn accepts_equals_syntax() {
        assert_eq!(text(&parse("--format=best")), vec!["--format", "best"]);
    }

    #[test]
    fn adds_the_dash_to_bare_words() {
        assert_eq!(text(&parse("quiet")), vec!["--quiet"]);
    }

    #[test]
    fn the_command_line_wins() {
        let dir = std::env::temp_dir().join(format!("ytgrab-config-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.txt");
        std::fs::write(&path, "--format best\n").unwrap();

        let args = [
            OsString::from("ytgrab"),
            OsString::from("--format"),
            OsString::from("worst"),
            OsString::from("--config"),
            path.to_string_lossy().to_string().into(),
        ];
        let spliced = super::splice(
            OsString::from("ytgrab"),
            args[1..].to_vec(),
            &path,
        );
        // The config comes first, so the command line overrides it.
        assert_eq!(
            text(&spliced),
            vec!["ytgrab", "--format", "best", "--format", "worst"]
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
