//! Turning a file name into something a file system will accept.
//!
//! yt-dlp has two options for this:
//!
//! * `--trim-filenames` — cut the name to a number of characters
//! * `--restrict-filenames` — keep only ASCII characters

/// Characters that are not allowed in a file name on Windows.
const WINDOWS_RESERVED: &[char] = &[
    '<', '>', ':', '"', '/', '\\', '|', '?', '*', '\u{0}',
];

/// Cut the name to `limit` characters (the extension is not counted).
pub fn trim(name: &str, limit: usize) -> String {
    if name.chars().count() <= limit {
        return name.to_string();
    }
    name.chars().take(limit).collect()
}

/// Replace everything that is not plain ASCII with an underscore.
pub fn restrict_to_ascii(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Replace the characters Windows does not allow, and the ones that are
/// simply inconvenient.
pub fn sanitize_windows(name: &str) -> String {
    name.chars()
        .map(|c| {
            if WINDOWS_RESERVED.contains(&c) {
                '_'
            } else {
                c
            }
        })
        .collect()
}

/// Apply every name rule the user asked for.
pub fn apply(name: &str, trim_to: Option<usize>, restrict: bool, windows: bool) -> String {
    let mut out = name.to_string();
    if restrict {
        out = restrict_to_ascii(&out);
    }
    if windows {
        out = sanitize_windows(&out);
    }
    if let Some(limit) = trim_to {
        // Trim the stem, keep the extension.
        if let Some((stem, ext)) = out.rsplit_once('.') {
            if !stem.is_empty() && !ext.is_empty() {
                out = format!("{}.{}", trim(stem, limit), ext);
            }
        } else {
            out = trim(&out, limit);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trims_the_stem_not_the_extension() {
        assert_eq!(trim("abcdefgh.mp4", 4), "abcd");
        assert_eq!(apply("a-very-long-name.mp4", Some(6), false, false), "a-very.mp4");
    }

    #[test]
    fn keeps_ascii_only() {
        assert_eq!(restrict_to_ascii("München straße"), "M_nchen stra_e");
        assert_eq!(restrict_to_ascii("ok-name_1.mp4"), "ok-name_1.mp4");
    }

    #[test]
    fn replaces_windows_reserved_characters() {
        assert_eq!(sanitize_windows("a:b*c?d"), "a_b_c_d");
    }

    #[test]
    fn combines_the_rules() {
        assert_eq!(apply("München: 2024.mp4", Some(4), true, true), "M_nc.mp4");
    }
}
