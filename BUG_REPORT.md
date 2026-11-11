# Bug report — ytgrab fails with HTTP 403 on high-quality formats

**Date:** 2026-11-11
**Component:** `ytgrab` (src/download.rs)
**Version:** local build from `~/Projects/ytgrab-rs`
**Severity:** high — blocks the most common use case (downloading 1080p)
**Status:** fixed — the high quality format is now downloaded (see *The fix*)

---

## Summary

Downloading any video-only progressive format (`137`, `136`, `248`, `247`, `399`, `398`, `135`, `244`, …) fails
immediately with a misleading "URL expired" message. The muxed low-res format (`18`) works fine.

```
$ ytgrab -f 137 --client android "https://youtu.be/JBoUwElRFVM"
error: googlevideo rejected the request (HTTP 403 Forbidden) — the URL expired, re-run the command

$ ytgrab -f 18  --client android --fragments 1 "https://youtu.be/JBoUwElRFVM"
Saved video/tmp/opencode/t18b.mp4      # works (360p, muxed)
```

The URL is **not** expired. The signed googlevideo URL carries a ~6 h expiry, and the failure reproduces
instantly on a URL generated seconds earlier.

---

## Reproduction

```bash
ytgrab --client android --get-url "https://youtu.be/JBoUwElRFVM" > url.txt

# plain GET (no Range) -> 403
curl -s -o /dev/null -w "%{http_code}\n" "$(head -1 url.txt)"

# bounded Range request -> 206 OK
curl -s -o /dev/null -w "%{http_code}\n" -r 0-1000 "$(head -1 url.txt)"

# open-ended Range -> 403
curl -s -o /dev/null -w "%{http_code}\n" -r 0- "$(head -1 url.txt)"
```

---

## Root cause (confirmed)

The original guess — "the range header is missing on a fresh request" — was **only half right**. The
measurements below show what googlevideo actually does.

### The rule

For a given signed URL, googlevideo will serve a **bounded window of the file starting at offset 0**:

| Request on a fresh URL | Status |
|---|---|
| plain GET (whole file) | 403 |
| `Range: bytes=0-` (open ended) | 403 |
| `Range: bytes=0-0` | 206 |
| `Range: bytes=0-1000` | 206 |
| `Range: bytes=5000000-6000000` (non zero start, inside the window) | 206 |
| `Range: bytes=0-15728641` | 206 |
| `Range: bytes=0-16777215` | **403** |
| `Range: bytes=0-104857599` | **403** |
| `Range: bytes=500000000-50001572864` (mid file) | **403** |

So: the window is the first ~15.8 MiB of the file, and requests for more than that are refused with 403
(no `Content-Range`, empty body, `Server: gvs 1.0`).

### The window size differs per format and per video

| URL | `clen` | `ratebypass` | `fvip` | biggest range served |
|---|---|---|---|---|
| this video, itag 137 | 944 MB | – | 4 | ~15.8 MiB |
| this video, itag 136 | 330 MB | – | 1 | refused at 15 MiB |
| this video, itag 18 | 332 MB | `yes` | 4 | 100 MiB, and the whole file |
| Rick Astley, itag 137 | 81 MB | – | 2 | the whole file |

**`ratebypass=yes` is the discriminator.** The ANDROID client returns it for progressive (muxed) formats
but not for adaptive video-only ones. Formats carrying it are served without the window restriction.

`n` (the throttling parameter) is the second one: yt-dlp's URLs come from the **TVHTML5** client and carry
`n=<computed>`, which lifts the restriction — but that client needs a PO token (`LOGIN_REQUIRED` without one)
and yt-dlp needs a JS runtime to compute `n`.

### Why the error message was wrong

A 403 from googlevideo means "range too large" or "not servable", not "expired". The old message sent the
user down the wrong path (re-running the same command, which fails identically).

---

## The fix

**`src/download.rs`**

1. Every request now carries a **bounded** `Range` header; the range size starts at 15 MiB.
2. On a 403 the chunk size is **halved** (down to 256 KiB) and the request is retried, so small windows
   still work.
3. The total size is resolved by probing with `Range: bytes=0-0` and reading `Content-Range`, so the file
   length is known even when the player response does not say.
4. The segmented (parallel) path chunks the same way, and no longer uses an open ended range.
5. `Error::SignedUrl` now carries the offending format id.

**`src/runner.rs`**

6. When googlevideo refuses a format, ytgrab mints a **fresh signed URL** (new player request) and falls
   back to the **next best format** — up to 6 alternatives — instead of giving up. The partial bytes of the
   refused format are deleted, since they belong to a different stream.
7. The error message names the actual cause and the format id.

**`src/output.rs`**

8. Absolute output templates stay absolute: `-o "/tmp/x.%(ext)s"` used to land in `./video/tmp/x.mp4`,
   because `sanitize()` replaced the empty first path component with the fallback name `video`.

---

## The fix, part two: solving the `n` challenge

Falling back to the progressive format works, but it means 360p instead of 1080p. The real answer is to
use a client whose URLs are not restricted.

The **TVHTML5** client serves the high quality formats, but its URLs carry a throttling challenge:

```
n=lIJCGe0SUkJKYksZFr      <- a puzzle, not a value
```

The answer is computed by a function inside YouTube's player JavaScript. yt-dlp solves it by running that
JavaScript in deno/node/bun — which means the user has to install a runtime.

ytgrab embeds [QuickJS](https://bellard.org/quickjs/) (`rquickjs`) instead and vendors the solver from
[yt-dlp/ejs](https://github.com/yt-dlp/ejs) (Unlicense) in `vendor/ejs/`. The solver works statically on
the player's source — it parses `base.js` with meriyah, finds the function that fills in the `n` and `sig`
query parameters of a `URL` object, rewrites it into a standalone function and evaluates it. No browser
environment is involved, so nothing needs to be installed.

```
$ ytgrab -f 137 "https://youtu.be/JBoUwElRFVM"
googlevideo refused format 137 (HTTP 403); retrying through the TV client
Saved tv137.mp4                        # 944,298,744 bytes, 1920x1080, 3600.0s
```

To make the TV client answer at all, three details were needed, all found by comparing our requests with
yt-dlp's:

1. the **watch page**, for the visitor id (`X-Goog-Visitor-Id` header) and the signature timestamp (`STS`),
2. **no API key** in the query string,
3. `playbackContext.contentPlaybackContext.signatureTimestamp` in the body, and `timeZone` /
   `utcOffsetMinutes` in the client context.

Without the visitor id the TV client answers `LOGIN_REQUIRED`.

---

## Secondary bug found in the same session (fixed)

**`-o` with an absolute path was treated as relative.**

```bash
ytgrab -f 18 --client android -o "/tmp/opencode/t18b.%(ext)s" "https://youtu.be/JBoUwElRFVM"
# -> Saved video/tmp/opencode/t18b.mp4
```

The file landed in `./video/tmp/opencode/t18b.mp4` relative to the CWD. `sanitize()` turns an empty string
into the fallback name `video`, and the empty first component of an absolute path is exactly that.

---

## Environment

- Arch Linux, KDE Plasma 6, Wayland, 1920x1080
- `ytgrab` from `~/Projects/ytgrab-rs`
- Network egress IP 158.174.155.144 (datacenter range) — worth noting in case it changes the behaviour
