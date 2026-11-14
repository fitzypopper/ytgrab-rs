//! Date filters.
//!
//! yt-dlp's date syntax is understood here:
//!
//! * `YYYYMMDD` — that day
//! * `today`, `yesterday`, `now`
//! * `now-2weeks`, `today-3months` — relative dates
//!
//! The dates are compared in UTC.

use chrono::{Datelike, Duration, NaiveDate, Utc};

/// Parse a date specification into seconds since the epoch.
pub fn parse(spec: &str) -> Option<i64> {
    let spec = spec.trim().to_ascii_lowercase();
    if spec.is_empty() {
        return None;
    }

    // A plain date: YYYYMMDD or YYYY-MM-DD.
    if spec.len() == 10 && spec.as_bytes().get(4) == Some(&b'-') {
        if let Ok(date) = NaiveDate::parse_from_str(&spec, "%Y-%m-%d") {
            return date.and_hms_opt(0, 0, 0).map(|d| d.and_utc().timestamp());
        }
    }
    if spec.len() == 8 && spec.chars().all(|c| c.is_ascii_digit()) {
        let year: i32 = spec[0..4].parse().ok()?;
        let month: u32 = spec[4..6].parse().ok()?;
        let day: u32 = spec[6..8].parse().ok()?;
        let date = NaiveDate::from_ymd_opt(year, month, day)?;
        return Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp());
    }

    let (base, offset) = match spec.split_once('-') {
        // "today-2weeks" — but not "2024-01-01", which is not a valid spec.
        Some((head, tail)) if !head.is_empty() && !tail.is_empty() => {
            let count: i64 = tail
                .trim_end_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .ok()?;
            (head, Some(count))
        }
        _ => (spec.as_str(), None),
    };

    let today = Utc::now().date_naive();
    let base_date = match base {
        "today" => today,
        "yesterday" => today - Duration::days(1),
        "now" => today,
        other => {
            // "now-2weeks" was split above; a bare "now" with no offset.
            if other != "now" {
                return None;
            }
            today
        }
    };

    let mut date = base_date;
    if let Some(count) = offset {
        // The unit is whatever follows the number.
        let unit = tail_unit(&spec);
        date = match unit {
            "day" | "days" => date - Duration::days(count),
            "week" | "weeks" => date - Duration::weeks(count),
            "month" | "months" => date - Duration::days(count * 30),
            "year" | "years" => date - Duration::days(count * 365),
            _ => return None,
        };
    }

    Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp())
}

/// The unit of a relative date, e.g. `weeks` in `now-2weeks`.
fn tail_unit(spec: &str) -> &str {
    match spec.rsplit_once('-') {
        Some((_, tail)) => {
            let start = tail.find(|c: char| !c.is_ascii_digit()).unwrap_or(0);
            &tail[start..]
        }
        None => "",
    }
}

/// Does the upload date fall inside the window?
///
/// `after` and `before` are epoch seconds; the upload date is an ISO 8601
/// string like `2009-10-24T23:57:33-07:00`.
pub fn matches(upload: &str, after: Option<i64>, before: Option<i64>) -> bool {
    if after.is_none() && before.is_none() {
        return true;
    }
    let Some(timestamp) = parse_iso8601(upload) else {
        // Without a date there is nothing to compare, so the video is kept.
        return true;
    };
    if let Some(after) = after {
        if timestamp < after {
            return false;
        }
    }
    if let Some(before) = before {
        if timestamp > before {
            return false;
        }
    }
    true
}

/// Parse an ISO 8601 timestamp into epoch seconds.
pub fn parse_iso8601(text: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(text) {
        return Some(dt.timestamp());
    }
    // "2009-10-24" on its own.
    let date = NaiveDate::parse_from_str(text, "%Y-%m-%d").ok()?;
    Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp())
}

/// Today, in UTC.
pub fn today() -> i64 {
    Utc::now()
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight is a valid time")
        .and_utc()
        .timestamp()
}

/// The year, for the "this year" style shortcuts.
pub fn year() -> i32 {
    Utc::now().year()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_dates() {
        let d = parse("20240115").unwrap();
        assert_eq!(parse_iso8601("2024-01-15").unwrap(), d);
    }

    #[test]
    fn rejects_nonsense() {
        assert!(parse("").is_none());
        assert!(parse("notadate").is_none());
        assert!(parse("2024").is_none());
    }

    #[test]
    fn parses_relative_dates() {
        let parsed = parse("today").unwrap();
        assert_eq!(parsed, today());
        assert!(parse("now-2weeks").unwrap() < parsed);
        assert!(parse("today-1day").unwrap() < parsed);
    }

    #[test]
    fn compares_against_the_window() {
        let upload = "2009-10-24T23:57:33-07:00";
        let after = parse("2009-01-01").unwrap();
        let before = parse("2010-01-01").unwrap();
        assert!(matches(upload, Some(after), Some(before)));
        assert!(!matches(upload, parse("2010-01-01"), None));
        assert!(!matches(upload, None, parse("2009-01-01")));
    }
}
