# PROJECT NOTES

Scratchpad for `ytgrab` (formerly `ytgrab`). Written for future sessions — everything here was
learned by poking at the live API, so do not "fix" it back.

## Layout

```
src/
  main.rs         binary entry point (clap + tokio runtime)
  cli.rs          clap definitions
  runner.rs       orchestration: fetch -> select -> download -> mux
  formats.rs      format selector parsing + ranking
  download.rs     resumable + segmented downloader
  mux.rs          ffmpeg wrappers
  subtitles.rs    json3 -> srt/vtt
  output.rs       output templates + filename sanitising
  url.rs          URL -> video id / playlist id
  cookies.rs      Netscape cookie files
  innertube/
    client.rs     client contexts (android/ios/mweb/web/tv) + POST helper
    player.rs     /youtubei/v1/player
    browse.rs     /youtubei/v1/browse (playlists)
    models.rs     serde models
```

## InnerTube facts (verified 2026-11-10)

**Endpoint**

```
POST https://www.youtube.com/youtubei/v1/player?key=<API_KEY>&prettyPrint=false
Content-Type: application/json
User-Agent: <client user agent>
X-Youtube-Client-Name: <numeric id>
X-Youtube-Client-Version: <version>
```

Body:

```json
{"context": {"client": {"clientName": "ANDROID", "clientVersion": "20.10.38",
 "androidSdkVersion": 34, "hl": "en", "gl": "US"}},
 "videoId": "...", "contentCheckOk": true, "racyCheckOk": true}
```

**Working clients**

| client | key | notes |
|---|---|---|
| ANDROID | `AIzaSyA8eiZmM1FaDVjRy-df2KTyQ_vz_yYM39w` | **use this one.** Returns unciphered URLs, no PO token, no bot check. 30 formats incl. 2160p. |
| IOS | `AIzaSyB-63vPrdThhKuerbB2N_l7Kwwcxj6yUAc` | plausible fallback, untested end to end |
| MWEB / WEB | `AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8` | returns `UNPLAYABLE / Video unavailable` without a visitor data + PO token |
| TVHTML5 | `AIzaSyDCU8hByM-4DrUqRUYnGn-3llEO78bcxq8` | `ERROR: YouTube is no longer supported in this application or device` |

**Gotchas that cost real time**

1. The body must nest the client under `context`. A flat `{"client": {...}}`
   returns `400 Precondition check failed`.
2. `playbackContext` (with `html5Preference`) is only accepted by WEB/MWEB.
   Sending it with ANDROID gives a 400.
3. Codecs are **not** a separate field in the player response — they live
   inside the mime type: `video/mp4; codecs="avc1.640028"`. A video-only
   format has one codec, an audio-only format also has exactly one (the
   audio codec!). Use the mime prefix (`video/` vs `audio/`) to tell them
   apart, never the codec count.
4. Numbers arrive as both JSON numbers and strings (`contentLength`,
   `approxDurationMs`, `fps`, `bitrate`…). `models::flex` handles both.
5. `googlevideo.com` rejects the download if the User-Agent differs from the
   one that fetched the player response. The downloader therefore reuses the
   InnerTube client UA.
6. `/browse` with `browseId: "VL<playlistId>"` works on the ANDROID client
   (WEB returns 404 for the same call). The response is ~3 MB for a 19-video
   playlist.
7. The recursive walker in `browse.rs` must collect the **inner** value of
   `playlistVideoRenderer`, not its parent. Collecting the parent silently
   yields an empty playlist.

**Signature cipher / PO token**

Not needed with the ANDROID client. If a future client returns
`signatureCipher` instead of `url`, the `s=` and `n=` parameters have to be
computed by running the player JS — that needs a JS engine (`boa_engine` or
`rquickjs`) and is the main reason to switch clients.

## The `n` challenge and the embedded JS engine

Some clients (the TV app) hand out URLs whose `n` parameter is a challenge.
The answer is computed by a function inside YouTube's `base.js`.

yt-dlp solves this by running the player JavaScript in deno/node/bun, which
means the user has to install one. ytgrab embeds QuickJS (`rquickjs`) instead
and vendors the solver from yt-dlp/ejs (Unlicense) in `vendor/ejs/`:

* `lib.min.js` bundles meriyah (ISC) and astring (MIT) — a JS parser and
  code generator.
* `core.min.js` parses `base.js`, finds the function that fills in the `n`
  and `sig` query parameters of a `URL` object, rewrites it into a
  standalone function and evaluates it.

It works statically on the player's source, so no browser environment is
needed. Solving one challenge takes ~0.5 s.

To get a TV client request to work at all, three things are needed, all
discovered by comparing requests with yt-dlp's:

1. the **watch page** for the visitor id (`X-Goog-Visitor-Id` header) and the
   signature timestamp (`STS`),
2. **no API key** in the query string,
3. `playbackContext.contentPlaybackContext.signatureTimestamp` in the body
   and `timeZone`/`utcOffsetMinutes` in the client context.

Without the visitor id the TV client answers `LOGIN_REQUIRED`.

## What works now, end to end

| video | yt-dlp | ytgrab | client used |
|---|---|---|---|
| Rick Astley, 1080p (77 MB) | 14.7 s | **4.4 s** | ANDROID |
| Rick Astley, 4K (342 MB) | 25.1 s | **15.9 s** | ANDROID |
| a 1 hour ambient video, 1080p (944 MB) | 51.8 s | **37.8 s** | ANDROID refused it, then TV |

The 944 MB case was 252 s before the chunk size was fixed — see below.

The Android client is tried first because it needs no JavaScript. Only when
googlevideo refuses its formats does ytgrab pay for the watch page, the
player script and the challenge solving.

## The chunk size cliff

googlevideo shapes the bandwidth of a long request. Measured on the same
file, one connection at a time:

| chunk size | 64 MB took |
|---|---|
| 2 MiB | 6.6 s |
| 4 MiB | 4.5 s |
| 8 MiB | **3.4 s** |
| 15 MiB | 20.0 s |

So 15 MiB chunks were running at a third of the speed of 8 MiB chunks. This
was the single biggest performance bug in the project: the 944 MB video took
252 s and now takes 37.8 s.

yt-dlp avoids it differently: it downloads the **HLS manifest** (667 small
fragments for that video) instead of the progressive URL.

## The throttling discovery (important)

Measured on the same 720p video (26 MB), same machine, same minute:

| | time |
|---|---|
| yt-dlp (single connection) | 8 s |
| ytgrab, one connection | **213 s** (≈125 kB/s) |
| ytgrab, 8 parallel byte-ranges | **2 s** |
| curl on yt-dlp's own URL | throttled to 247 kB/s |

YouTube throttles a *single* HTTP connection to roughly 250 kB/s, but serves
parallel `Range` requests at full speed. The client you use barely matters:
both the ANDROID and the TVHTML5 URLs were throttled the same way by curl.

So the default is now **auto-fragmentation**: streams of 8 MiB or more are
fetched as 8 parallel byte-ranges, 2-8 MiB as 4, smaller ones sequentially.
`--fragments N` overrides it; `--fragments 1` forces the slow single
connection.

The parallel path was verified to produce **byte-identical** output to the
sequential path (`sha256` match on both the video and the audio stream).

## Subtitle classification

The ANDROID client does not return the `kind` field for caption tracks, so a
track cannot be reliably labelled "manual" or "automatic" the way yt-dlp
does (yt-dlp gets it from the WEB client, which needs a PO token). The
timedtext `type=list` endpoint is dead.

Workaround: a track with no `kind` is accepted by **either** `--write-subs`
or `--write-auto-subs`, and a manual track wins over an automatic one for
the same language. `caps=asr` in the track URL is used as a hint when
present.

## Googlevideo's range policy (important, measured)

A 403 from googlevideo does **not** mean the URL expired. Measured on
`https://youtu.be/JBoUwElRFVM` with the ANDROID client:

| request | result |
|---|---|
| plain GET (whole file) | 403 |
| `Range: bytes=0-` (open ended) | 403 |
| `Range: bytes=0-15728641` | 206 |
| `Range: bytes=0-16777215` | 403 |
| `Range: bytes=500000000-50001572864` | 403 |

So a signed URL serves a **bounded window starting at offset 0**, and the
window size differs per format and per video:

* itag 18 (muxed, `ratebypass=yes`) → the whole 332 MB
* itag 137 (video only, 944 MB) → the first ~15.8 MiB
* Rick Astley itag 137 (81 MB) → the whole file

`ratebypass=yes` in the URL is the discriminator: googlevideo serves those
without the window restriction. The ANDROID client only returns it for
progressive formats.

yt-dlp does not hit this because it uses the TVHTML5 client, whose URLs
carry `n=<computed>` — but that needs a PO token and a JS runtime.

The workaround in ytgrab: on a 403, mint a fresh URL and fall back to the
next best format, preferring the ones whose URL has `ratebypass=yes`.

## Interrupted downloads

The downloader writes to `<file>.part` and renames it when complete, so an
interrupted run never leaves a half written file under the real name. On
restart it sends `Range: bytes=<already downloaded>-` and continues.

If the connection dies mid-transfer the same happens: the bytes so far are
kept and the request is retried with the next range. If the server answers
403 the chunk size is halved (15 MiB -> 8 MiB -> ... -> 256 KiB) and the
request is tried again, because a 403 usually means "that range was too
big" rather than "expired".

Two instances cannot corrupt each other: a lock file (`<file>.lock`) holds
the pid of the process that owns the download, and a lock left behind by a
crashed process is taken over once that pid is gone.

`tests/resume.rs` covers all of this against a local server that drops
connections on purpose.

## Speed work

* **8 MiB range requests.** googlevideo shapes the bandwidth of a long
  request: 8 MiB chunks ran at 18.8 MB/s while 15 MiB chunks collapsed to
  3.2 MB/s. This was the single biggest win (252s -> 37.8s on a 944 MB
  video). The chunk size and the number of connections were measured
  against each other:

  | chunk x connections | 77 MB video |
  |---|---|
  | 8 MiB x 8 | **3.39 s (26.4 MB/s)** |
  | 8 MiB x 16 | 3.56 s (25.1 MB/s) |
  | 4 MiB x 8 | 3.66 s (24.5 MB/s) |
  | 4 MiB x 16 | 4.18 s (21.4 MB/s) |
  | 2 MiB x 16 | 4.42 s (20.3 MB/s) |
  | 1 MiB x 32 | 5.21 s (17.2 MB/s) |

  Smaller chunks mean more requests and more overhead; more connections
  than eight make things worse, not better. `--chunk-size` and
  `--fragments` are exposed for tuning.
* **Parallel connections.** YouTube throttles a single connection to about
  250 kB/s but serves parallel range requests at full speed. The number of
  connections turned out not to matter much once the chunk size was right
  (4, 8, 16 and 24 fragments all landed between 25 and 29 MB/s), so the
  default stayed at 8.
* **The HLS path is not worth it.** yt-dlp downloads hard videos over HLS
  (667 fragments of ~25 kB each, with `ratebypass=yes` in the URLs). The
  fragments are tiny and latency bound: four of them took 0.56 s on one
  connection. The progressive path with 8 MiB chunks is faster and simpler.
* **Video and audio at the same time.** The two streams are independent, so
  they are now fetched concurrently instead of one after the other.
* **A session cache.** The watch page, the player script and the parse of it
  are cached in `~/.cache/ytgrab/`, which saves about 6 seconds and 3.9 MB
  of traffic on every run after the first. A stale visitor id is detected
  and refreshed.
* **Size verification.** A download that stops early is now reported
  instead of being muxed into a broken file.

## Features that came after the first release

* `--download-archive` — plain text file of already downloaded videos, same
  format as yt-dlp. The check happens before any request, so a re-run over a
  playlist costs almost nothing.
* `--print` and the `--get-title` / `--get-id` / `--get-duration` /
  `--get-uploader` shortcuts, plus `--skip-download`.
* `--match-filter` — a regular expression against the title, `!` inverts it.
* `--max-filesize` / `--min-filesize` — formats outside the limits are
  dropped; if nothing is left the video is skipped rather than failing.
* `--no-overwrites` — leave existing files alone.
* `--sleep-requests` — wait between videos.

`%(duration)s` is in seconds and `%(duration_string)s` is formatted, to
match yt-dlp.

## Bugs found and fixed during the build

- Template renderer emitted the conversion character (`%(title)s` -> `title` + `s`).
- `has_video()`/`has_audio()` misclassified audio-only formats as video.
- Multiple bracket filters (`[ext=mp4][height<=360]`) were not parsed.
- Playlist walker returned parent objects -> empty playlist.

## TODO

- [ ] Browser cookie extraction (Chrome/Brave on Linux: `Local State` ->
      base64 key -> PBKDF2-HMAC-SHA1 (`saltysalt`, 1000 iters) -> AES-128-CBC
      with a 16-byte space IV). The cookie *file* path already works.
- [ ] `--cookies-from-browser`
- [ ] Verify IOS client as a fallback when ANDROID gets throttled.
- [ ] Archive support (`--download-archive`).
- [ ] Rate limiting (`--sleep-requests`).
- [ ] `--write-auto-subs` translation (`tlang`).
- [ ] Integration test that hits the network, marked `#[ignore]`.
- [ ] Compare speed against yt-dlp on a large video.
