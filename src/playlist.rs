//! Picking items out of a playlist.
//!
//! Supports the yt-dlp syntax:
//!
//! * `--playlist-start` / `--playlist-end` — 1 based, inclusive
//! * `--playlist-items` — a list of indices and ranges, e.g. `1:3,7,-2:`
//! * `--playlist-reverse` — play the list backwards
//! * `--playlist-random` — shuffle it

/// Apply the selection to a list of items.
///
/// `start` and `end` are 1 based and inclusive, `items` is the raw
/// `--playlist-items` value. Returns the indices of the selected items, in
/// the order they should be downloaded.
pub fn select(
    len: usize,
    start: Option<usize>,
    end: Option<usize>,
    items: Option<&str>,
    reverse: bool,
    random: bool,
) -> Vec<usize> {
    if len == 0 {
        return Vec::new();
    }

    let mut indices: Vec<usize> = match items {
        Some(spec) => parse_items(spec, len),
        None => {
            let first = start.unwrap_or(1).max(1);
            let last = end.unwrap_or(len).min(len);
            if first > last {
                Vec::new()
            } else {
                (first - 1..=last - 1).collect()
            }
        }
    };

    if reverse {
        indices.reverse();
    }
    if random {
        // A simple Fisher-Yates shuffle; there is no seed to make it
        // reproducible, matching yt-dlp's behaviour.
        let mut rng = simple_rng();
        for i in (1..indices.len()).rev() {
            let j = (rng() % (i + 1) as u64) as usize;
            indices.swap(i, j);
        }
    }
    indices
}

/// Parse a `--playlist-items` specification into indices.
///
/// `1:3` is a range, `5` is a single item, `-2` counts from the end and
/// `-2:` means "from the second to last item onwards".
pub fn parse_items(spec: &str, len: usize) -> Vec<usize> {
    let mut out = Vec::new();

    for chunk in spec.split(',') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }

        if let Some((from, to)) = chunk.split_once(':') {
            let from = from.trim();
            let to = to.trim();
            let start = resolve(from, len, 1);
            let end = resolve(to, len, len);
            if start <= end {
                out.extend(start..=end);
            } else {
                out.extend((end..=start).rev());
            }
        } else {
            let index = resolve(chunk, len, 0);
            if index > 0 {
                out.push(index);
            }
        }
    }

    out.retain(|i| *i > 0 && *i <= len);
    out.dedup();
    out
}

/// Turn a (possibly negative) item number into a 1 based index.
///
/// Negative numbers count from the end: -1 is the last item. `default` is
/// used when the number is missing.
fn resolve(text: &str, len: usize, default: usize) -> usize {
    let text = text.trim();
    if text.is_empty() {
        return default;
    }
    match text.parse::<i64>() {
        Ok(value) if value < 0 => {
            let from_end = len as i64 + value + 1;
            if from_end < 1 {
                1
            } else {
                from_end as usize
            }
        }
        Ok(value) if value > 0 => value as usize,
        _ => default,
    }
}

/// A tiny xorshift generator, so the shuffle does not need a dependency.
fn simple_rng() -> impl FnMut() -> u64 {
    let mut state = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x2545F4914F6CDD1D)
        | 1;
    move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        state
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_a_range() {
        assert_eq!(select(10, Some(2), Some(4), None, false, false), vec![1, 2, 3]);
        assert_eq!(select(10, None, None, None, false, false).len(), 10);
    }

    #[test]
    fn reverses() {
        assert_eq!(
            select(5, Some(1), Some(3), None, true, false),
            vec![2, 1, 0]
        );
    }

    #[test]
    fn parses_item_lists() {
        assert_eq!(parse_items("1:3", 10), vec![1, 2, 3]);
        assert_eq!(parse_items("1,3,5", 10), vec![1, 3, 5]);
        assert_eq!(parse_items("2:", 5), vec![2, 3, 4, 5]);
        assert_eq!(parse_items("-2", 5), vec![4]);
        assert_eq!(parse_items("-2:", 5), vec![4, 5]);
        assert_eq!(parse_items("1:3", 3), vec![1, 2, 3]);
    }

    #[test]
    fn ignores_out_of_range_items() {
        assert_eq!(parse_items("1,99", 5), vec![1]);
    }

    #[test]
    fn random_keeps_every_item() {
        let picked = select(10, None, None, None, false, true);
        assert_eq!(picked.len(), 10);
        let mut sorted = picked.clone();
        sorted.sort();
        assert_eq!(sorted, (0..10).collect::<Vec<_>>());
    }
}
