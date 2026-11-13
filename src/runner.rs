//! Orchestration: glue InnerTube, format selection, downloading and muxing
//! together into the commands the user actually types.

use std::path::{Path, PathBuf};

use indicatif::MultiProgress;
use serde_json::json;

use crate::cli::Cli;
use crate::download::Downloader;
use crate::error::{Error, Result};
use crate::formats::{self, Selector};
use crate::innertube::browse;
use crate::innertube::client::{ClientContext, InnerTube};
use crate::innertube::models::{CaptionTrack, Format, PlayerResponse, VideoDetails};
use crate::innertube::player;
use crate::mux::{self, Meta};
use crate::output::{self, TemplateVars};
use crate::subtitles;
use crate::url::{self, Target};

/// The TV client, used when the Android client's formats are refused.
///
/// Its URLs carry a throttling challenge, so the player script has to be
/// downloaded and run through the embedded JavaScript engine.
struct TvSource {
    jar: Option<std::sync::Arc<reqwest::cookie::Jar>>,
    watch: Option<crate::innertube::web::WatchPage>,
    player_js: Option<String>,
    /// The player script, reduced to the part that solves challenges. It is
    /// the same for every video of a session, so it is only built once.
    preprocessed: Option<String>,
    /// The session data, cached on disk so the next run does not have to
    /// download the watch page and the player script again.
    cache: Option<crate::cache::SessionCache>,
    /// Set when the cached session turned out to be stale.
    refreshed: bool,
}

impl TvSource {
    fn new(jar: Option<std::sync::Arc<reqwest::cookie::Jar>>) -> Self {
        Self {
            jar,
            watch: None,
            player_js: None,
            preprocessed: None,
            cache: crate::cache::SessionCache::open(),
            refreshed: false,
        }
    }

    /// The formats the TV client offers, with their challenges solved.
    async fn formats(&mut self, video_id: &str) -> Result<(PlayerResponse, Vec<Format>)> {
        let http = reqwest::Client::builder()
            .user_agent(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36",
            )
            .build()?;

        // The session data (visitor id, player script, preprocessed player)
        // is the same for every video, so it is cached on disk and the next
        // run does not have to download or parse any of it again.
        if self.watch.is_none() {
            if let Some(cache) = &self.cache {
                if let Some(session) = cache.load() {
                    self.preprocessed = Some(session.preprocessed.clone());
                    self.player_js = Some(session.player_js.clone());
                    self.watch = Some(crate::innertube::web::WatchPage {
                        visitor_data: session.visitor_data,
                        signature_timestamp: session.signature_timestamp,
                        player_js_url: session.player_js_url,
                        api_key: String::new(),
                        client_version: String::new(),
                    });
                }
            }
        }

        if self.watch.is_none() {
            self.watch = Some(crate::innertube::web::fetch(&http, video_id).await?);
        }
        let watch = self.watch.as_ref().expect("just fetched");

        let mut ctx = ClientContext::tv_samsung();
        ctx.visitor_data = Some(watch.visitor_data.clone());
        ctx.signature_timestamp = Some(watch.signature_timestamp);
        let it = InnerTube::with_cookie_jar(ctx, self.jar.clone())?;

        // Solve the throttling challenges with the player's JavaScript.
        if self.preprocessed.is_none() {
            if self.player_js.is_none() {
                self.player_js = Some(
                    crate::innertube::web::fetch_player_js(&http, &watch.player_js_url).await?,
                );
            }
            let player_js = self.player_js.clone().expect("just fetched");
            let preprocessed = tokio::task::spawn_blocking(move || {
                crate::js::preprocess_player(&player_js).map(|(p, _)| p)
            })
            .await
            .map_err(|e| Error::other(format!("the solver thread died: {e}")))??;
            self.preprocessed = Some(preprocessed);
        }

        let player = match player::fetch_tv(&it, video_id).await {
            Ok(player) => player,
            Err(e) => {
                // A cached visitor id can go stale. Fetch a fresh session and
                // try once more.
                let message = e.to_string().to_lowercase();
                let stale = message.contains("login") || message.contains("reload");
                if !stale || self.refreshed {
                    return Err(e);
                }
                self.refreshed = true;
                if let Some(cache) = &self.cache {
                    cache.clear();
                }
                self.watch = None;
                self.player_js = None;
                self.preprocessed = None;
                return Box::pin(self.formats(video_id)).await;
            }
        };

        let formats =
            player::all_formats(player.streaming_data.as_ref().expect("streaming data"));
        let preprocessed = self.preprocessed.clone().expect("just built");
        let solved = tokio::task::spawn_blocking(move || {
            let mut formats = formats;
            player::solve_n_challenges_preprocessed(&mut formats, &preprocessed)?;
            Ok::<Vec<Format>, Error>(formats)
        })
        .await
        .map_err(|e| Error::other(format!("the solver thread died: {e}")))??;

        // Remember the session for the next run.
        if let Some(cache) = &self.cache {
            if let (Some(player_js), Some(preprocessed)) =
                (self.player_js.clone(), self.preprocessed.clone())
            {
                let mut session = crate::cache::Session {
                    visitor_data: watch.visitor_data.clone(),
                    signature_timestamp: watch.signature_timestamp,
                    player_js_url: watch.player_js_url.clone(),
                    player_js,
                    preprocessed,
                    fetched_at: 0,
                };
                let _ = cache.save(&mut session);
            }
        }

        Ok((player, solved))
    }
}

/// Context shared across every video in a playlist download.
#[derive(Debug, Clone, Default)]
struct PlaylistContext {
    id: String,
    title: String,
    index: Option<u64>,
}

pub async fn run(cli: Cli) -> Result<()> {
    let target = url::parse(&cli.url)?;

    let ctx = ClientContext::by_name(&cli.client)
        .ok_or_else(|| Error::other(format!("unknown client `{}`", cli.client)))?;

    // Cookies are shared between the API client and the media downloader:
    // age-restricted videos need them on both.
    let jar = match (&cli.cookies, &cli.cookies_from_browser) {
        (Some(path), _) => {
            let jar = crate::cookies::load_jar(path)?;
            Some(std::sync::Arc::new(jar))
        }
        (None, Some(browser)) => {
            let cookies = crate::browser_cookies::extract(browser)?;
            if !cli.quiet {
                eprintln!(
                    "Read {} cookies from {browser}",
                    cookies.len()
                );
            }
            Some(std::sync::Arc::new(crate::browser_cookies::to_jar(&cookies)))
        }
        (None, None) => None,
    };

    let it = InnerTube::with_cookie_jar(ctx, jar.clone())?;

    // The archive of already downloaded videos.
    let archive = match &cli.download_archive {
        Some(path) => Some(crate::archive::Archive::load(path)?),
        None => None,
    };

    let mut dl = Downloader::with_cookie_jar(it.user_agent(), jar.clone())?;
    dl.retries = cli.retries;
    dl.fragments = cli.fragments.max(1);
    dl.chunk_size = match cli.chunk_size.as_str() {
        "0" | "" => 0,
        v => crate::output::parse_size(v)
            .ok_or_else(|| Error::other(format!("bad --chunk-size value `{v}`")))?,
    };

    match target {
        Target::Video { id, playlist } => {
            let want_playlist = cli.playlist && !cli.no_playlist;
            match (want_playlist, playlist) {
                (true, Some(list)) => {
                    download_playlist(&it, &dl, &cli, &list, jar.clone(), archive.as_ref()).await
                }
                _ => {
                    download_video(
                        &it,
                        &dl,
                        &cli,
                        &id,
                        &PlaylistContext::default(),
                        &mut TvSource::new(jar.clone()),
                        archive.as_ref(),
                    )
                    .await
                }
            }
        }
        Target::Playlist { id } => {
            download_playlist(&it, &dl, &cli, &id, jar.clone(), archive.as_ref()).await
        }
    }
}

async fn download_playlist(
    it: &InnerTube,
    dl: &Downloader,
    cli: &Cli,
    playlist_id: &str,
    jar: Option<std::sync::Arc<reqwest::cookie::Jar>>,
    archive: Option<&crate::archive::Archive>,
) -> Result<()> {
    if !cli.quiet {
        eprintln!("Fetching playlist {playlist_id} ...");
    }
    let items = browse::playlist(it, playlist_id).await?;
    if items.is_empty() {
        return Err(Error::other("playlist is empty"));
    }
    if !cli.quiet {
        eprintln!("Playlist has {} videos", items.len());
    }

    let ctx = PlaylistContext {
        id: playlist_id.to_string(),
        title: String::new(),
        index: None,
    };

    let mut failures = 0;
    let mut tv = TvSource::new(jar);
    for (i, item) in items.iter().enumerate() {
        // Be nice to the server.
        if cli.sleep_requests > 0.0 && i > 0 {
            tokio::time::sleep(std::time::Duration::from_secs_f64(cli.sleep_requests)).await;
        }
        let mut item_ctx = ctx.clone();
        item_ctx.index = Some(i as u64 + 1);
        if let Err(e) =
            download_video(it, dl, cli, &item.video_id, &item_ctx, &mut tv, archive).await
        {
            failures += 1;
            eprintln!("error: {} failed: {e}", item.video_id);
        }
    }

    if failures > 0 {
        return Err(Error::other(format!(
            "{failures} of {} videos failed",
            items.len()
        )));
    }
    Ok(())
}

async fn download_video(
    it: &InnerTube,
    dl: &Downloader,
    cli: &Cli,
    video_id: &str,
    pctx: &PlaylistContext,
    tv: &mut TvSource,
    archive: Option<&crate::archive::Archive>,
) -> Result<()> {
    // The archive is checked before anything else: a video that was already
    // downloaded should not cost a single request.
    if let Some(archive) = archive {
        if archive.contains(video_id) {
            if !cli.quiet {
                eprintln!("Skipping {video_id}: already in the archive");
            }
            return Ok(());
        }
    }

    // googlevideo serves some formats only in small windows and refuses
    // larger range requests with a 403. When that happens, mint a fresh URL
    // and fall back to the next best format.
    let mut failed_formats: Vec<i64> = Vec::new();
    // The Android client is tried first because it needs no JavaScript. If
    // googlevideo refuses its formats, the TV client is used instead: its
    // URLs work, but their throttling challenge has to be solved first.
    let mut use_tv = false;

    loop {
        let (player, formats) = if use_tv {
            tv.formats(video_id).await?
        } else {
            let player = player::fetch(it, video_id).await?;
            let streaming = player
                .streaming_data
                .as_ref()
                .ok_or_else(|| Error::Unavailable("no streaming data".into()))?;
            let formats = player::all_formats(streaming);
            (player, formats)
        };

        if cli.list_formats {
            print_formats(&formats);
            return Ok(());
        }

        let details = player.video_details.clone().unwrap_or_default();

        // Only download videos whose title matches the filter.
        if let Some(filter) = &cli.match_filter {
            if !title_matches(filter, &details.title) {
                if !cli.quiet {
                    eprintln!("Skipping {video_id}: the title does not match the filter");
                }
                return Ok(());
            }
        }

        // Print the requested fields, then maybe stop here.
        let printed = print_fields(cli, &details, video_id, pctx);
        if printed || cli.skip_download {
            return Ok(());
        }

        // Drop the formats that are too big or too small. If nothing is
        // left the video is skipped, the way yt-dlp does it.
        let formats = filter_by_size(formats, cli)?;
        if formats.is_empty() {
            if !cli.quiet {
                eprintln!("Skipping {video_id}: no format fits the size limits");
            }
            return Ok(());
        }

        let selector = Selector::parse(&cli.format)?;
        let selection = formats::select_excluding_with(
            &selector,
            &formats,
            cli.prefer_opus,
            &failed_formats,
            !failed_formats.is_empty(),
        )?;

        if cli.get_url {
            for f in selection.videos.iter().chain(selection.audios.iter()) {
                if let Some(url) = &f.url {
                    println!("{url}");
                }
            }
            return Ok(());
        }

        if cli.dump_json {
            println!("{}", build_info_json(&player, &selection, video_id));
            return Ok(());
        }

        // Extracting audio never needs the video stream at all.
        if cli.extract_audio {
            let audio = match selection.audios.first() {
                Some(a) => a.clone(),
                None => formats::best_audio_only(&formats, cli.prefer_opus)?,
            };
            return extract_audio_only(dl, cli, &details, &audio, pctx, video_id).await;
        }

        match download_streams(dl, cli, &player, &details, &selection, pctx, video_id).await {
            Ok(()) => {
                if let Some(archive) = archive {
                    archive.record(video_id, &details.title)?;
                }
                return Ok(());
            }
            Err(Error::SignedUrl { status, itag }) => {
                if !use_tv {
                    use_tv = true;
                    eprintln!(
                        "googlevideo refused format {itag} (HTTP {status}); \
                         retrying through the TV client"
                    );
                    continue;
                }
                failed_formats.push(itag);
                if failed_formats.len() > MAX_FORMAT_FALLBACKS {
                    return Err(Error::other(format!(
                        "googlevideo refused every format it offered (HTTP {status})"
                    )));
                }
                eprintln!(
                    "googlevideo refused format {itag} (HTTP {status}); \
                     minting a new URL and trying another format"
                );
                // The half downloaded bytes belong to the refused format.
                remove_stale_parts(&details, pctx, cli);
                continue;
            }
            Err(e) => return Err(e),
        }
    }
}

/// Download just the audio stream and convert it to the requested format.
async fn extract_audio_only(
    dl: &Downloader,
    cli: &Cli,
    details: &VideoDetails,
    audio: &Format,
    pctx: &PlaylistContext,
    video_id: &str,
) -> Result<()> {
    let vars = TemplateVars {
        title: details.title.clone(),
        id: video_id.to_string(),
        ext: cli.audio_format.clone(),
        uploader: details.author.clone().unwrap_or_default(),
        duration_secs: details.duration_secs(),
        resolution: String::new(),
        format_id: audio.itag.to_string(),
        playlist_index: pctx.index,
        playlist_title: pctx.title.clone(),
        playlist_id: pctx.id.clone(),
        epoch: chrono::Utc::now().timestamp() as u64,
    };

    let out_path = PathBuf::from(output::sanitize_path(&output::render(&cli.output, &vars)));
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let stem = out_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "audio".into());
    let parent = out_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let source_ext = audio.ext();
    let tmp = parent.join(format!("{stem}.f{}.{source_ext}", audio.itag));
    let url = audio
        .url
        .clone()
        .ok_or_else(|| Error::other("audio stream has no url"))?;

    let mp = MultiProgress::new();
    let bar = mp.add(mux_progress_bar(audio.size_bytes(), "audio"));
    dl.download(
        &url,
        &tmp,
        &bar,
        audio.size_bytes(),
        fragment_count(cli, audio.size_bytes()),
        audio.itag,
    )
    .await?;
    bar.finish_and_clear();

    // Already in the requested container? Then no conversion is needed.
    if source_ext == cli.audio_format {
        std::fs::rename(&tmp, &out_path)?;
    } else {
        let codec = match cli.audio_format.as_str() {
            "mp3" => "libmp3lame",
            "m4a" | "aac" => "aac",
            "opus" => "libopus",
            "flac" => "flac",
            "wav" => "pcm_s16le",
            other => other,
        };
        mux::extract_audio(&tmp, &out_path, codec, Some("192k")).await?;
        let _ = std::fs::remove_file(&tmp);
    }

    if !cli.quiet {
        eprintln!("Saved {}", out_path.display());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// the actual download
// ---------------------------------------------------------------------------

async fn download_streams(
    dl: &Downloader,
    cli: &Cli,
    player: &PlayerResponse,
    details: &VideoDetails,
    selection: &formats::Selection,
    pctx: &PlaylistContext,
    video_id: &str,
) -> Result<()> {
    let video = selection
        .videos
        .first()
        .ok_or_else(|| Error::other("no video stream selected"))?;

    let audio = selection.audios.first();

    let video_ext = video.ext();
    let audio_ext = audio.map(|a| a.ext()).unwrap_or_default();
    let final_ext = if cli.extract_audio {
        cli.audio_format.clone()
    } else if selection.needs_mux() {
        // Keep the container aligned with the codecs for stream copy. Opus in
        // an MP4 works but many players refuse it, so fall back to mkv.
        let audio_ok = audio_ext == "m4a" || audio_ext == "mp4" || audio_ext.is_empty();
        if video_ext == "mp4" && audio_ok {
            "mp4".into()
        } else if video_ext == "webm" && (audio_ext == "webm" || audio_ext.is_empty()) {
            "webm".into()
        } else {
            "mkv".into()
        }
    } else {
        video_ext.clone()
    };

    let vars = TemplateVars {
        title: details.title.clone(),
        id: video_id.to_string(),
        ext: final_ext.clone(),
        uploader: details.author.clone().unwrap_or_default(),
        duration_secs: details.duration_secs(),
        resolution: resolution_of(video),
        format_id: selection
            .videos
            .iter()
            .chain(selection.audios.iter())
            .map(|f| f.itag.to_string())
            .collect::<Vec<_>>()
            .join("+"),
        playlist_index: pctx.index,
        playlist_title: pctx.title.clone(),
        playlist_id: pctx.id.clone(),
        epoch: chrono::Utc::now().timestamp() as u64,
    };

    let out_path = PathBuf::from(output::sanitize_path(&output::render(&cli.output, &vars)));
    if let Some(parent) = out_path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // Leave existing files alone unless the user says otherwise.
    if cli.no_overwrites && out_path.exists() {
        if !cli.quiet {
            eprintln!("Skipping {}: the file already exists", out_path.display());
        }
        return Ok(());
    }

    let stem = out_path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "video".into());
    let parent = out_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));

    let mp = MultiProgress::new();

    // ---- video and audio, at the same time -------------------------------
    let video_tmp = parent.join(format!("{stem}.f{}.{video_ext}", video.itag));
    let video_url = video
        .url
        .clone()
        .ok_or_else(|| Error::other("video stream has no url (signature cipher unsupported)"))?;

    let audio_plan = match audio {
        Some(a) => {
            let ext = a.ext();
            let tmp = parent.join(format!("{stem}.f{}.{ext}", a.itag));
            let url = a
                .url
                .clone()
                .ok_or_else(|| Error::other("audio stream has no url"))?;
            Some((a, url, tmp))
        }
        None => None,
    };

    let video_bar = mp.add(mux_progress_bar(video.size_bytes(), "video"));
    let audio_bar = audio_plan
        .as_ref()
        .map(|(a, _, _)| mp.add(mux_progress_bar(a.size_bytes(), "audio")));

    // The two streams do not depend on each other, so they are fetched at
    // the same time instead of one after the other.
    let video_task = dl.download(
        &video_url,
        &video_tmp,
        &video_bar,
        video.size_bytes(),
        fragment_count(cli, video.size_bytes()),
        video.itag,
    );
    let audio_task = async {
        match (&audio_plan, &audio_bar) {
            (Some((a, url, tmp)), Some(bar)) => {
                dl.download(
                    url,
                    tmp,
                    bar,
                    a.size_bytes(),
                    fragment_count(cli, a.size_bytes()),
                    a.itag,
                )
                .await
            }
            _ => Ok(0),
        }
    };

    let (video_result, audio_result) = tokio::join!(video_task, audio_task);
    video_result?;
    audio_result?;
    video_bar.finish_and_clear();
    if let Some(bar) = &audio_bar {
        bar.finish_and_clear();
    }

    let mut audio_tmp: Option<PathBuf> = None;
    if let Some((_, _, tmp)) = &audio_plan {
        audio_tmp = Some(tmp.clone());
    }

    // ---- combine ---------------------------------------------------------
    let meta = Meta {
        title: details.title.clone(),
        uploader: details.author.clone(),
        artist: details.author.clone(),
        description: details.short_description.clone(),
        webpage_url: Some(format!("https://www.youtube.com/watch?v={video_id}")),
        duration: details.duration_secs(),
    };

    if let Some(audio_path) = &audio_tmp {
        if cli.no_mux {
            // Keep the two streams separate, like yt-dlp's --no-mux.
            if !cli.quiet {
                eprintln!("Saved {}", video_tmp.display());
                eprintln!("Saved {}", audio_path.display());
            }
        } else {
            mux::merge(&video_tmp, audio_path, &out_path, &meta).await?;
            let _ = std::fs::remove_file(&video_tmp);
            if !cli.keep_files {
                let _ = std::fs::remove_file(audio_path);
            }
            if !cli.quiet {
                eprintln!("Saved {}", out_path.display());
            }
        }
    } else {
        std::fs::rename(&video_tmp, &out_path)?;
        if !cli.quiet {
            eprintln!("Saved {}", out_path.display());
        }
    }

    // ---- sidecar files ---------------------------------------------------
    if cli.write_info_json {
        let path = parent.join(format!("{stem}.info.json"));
        std::fs::write(&path, build_info_json(player, selection, video_id))?;
    }

    if cli.write_thumbnail {
        if let Some(thumbs) = &details.thumbnail {
            if let Some(best) = thumbs.best() {
                if let Ok(bytes) = dl.get_bytes(&best.url).await {
                    let ext = if best.url.contains("webp") {
                        "webp"
                    } else {
                        "jpg"
                    };
                    let path = parent.join(format!("{stem}.{ext}"));
                    let _ = std::fs::write(&path, bytes);
                }
            }
        }
    }

    if cli.write_subs || cli.write_auto_subs {
        write_subtitles(dl, cli, player, &parent, &stem).await?;
    }

    Ok(())
}

async fn write_subtitles(
    dl: &Downloader,
    cli: &Cli,
    player: &PlayerResponse,
    parent: &Path,
    stem: &str,
) -> Result<()> {
    let Some(tracklist) = player
        .captions
        .as_ref()
        .and_then(|c| c.player_captions_tracklist_renderer.as_ref())
    else {
        if !cli.quiet {
            eprintln!("no subtitles available");
        }
        return Ok(());
    };

    let wanted_langs: Vec<String> = cli
        .sub_lang
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    // One file per language: a manual track wins over an automatic one.
    let mut chosen: Vec<(String, &CaptionTrack)> = Vec::new();
    for track in &tracklist.caption_tracks {
        let lang = track.language_code.clone().unwrap_or_default();
        let kind = track_kind(track);

        let wanted = match kind {
            TrackKind::Manual => cli.write_subs,
            TrackKind::Auto => cli.write_auto_subs,
            // The ANDROID client does not tell us whether a track was
            // uploaded or generated, so accept it for either flag.
            TrackKind::Unknown => cli.write_subs || cli.write_auto_subs,
        };
        if !wanted {
            continue;
        }
        if !wanted_langs.iter().any(|w| w == &lang || *w == "all") {
            continue;
        }

        match chosen.iter_mut().find(|(l, _)| *l == lang) {
            Some((_, existing)) => {
                // Prefer a manual track over an automatic one.
                if track_kind(existing) != TrackKind::Manual && kind == TrackKind::Manual {
                    *existing = track;
                }
            }
            None => chosen.push((lang, track)),
        }
    }

    for (lang, track) in chosen {
        let Some(url) = json3_url(&track.base_url) else {
            continue;
        };
        let body = dl.get_bytes(&url).await?;
        let text = String::from_utf8_lossy(&body);
        let cues = subtitles::parse_json3(&text)?;
        if cues.is_empty() {
            continue;
        }
        let path = parent.join(format!("{stem}.{lang}.{}", cli.sub_format));
        subtitles::write_cues(&cues, &path)?;
        if !cli.quiet {
            eprintln!("Saved {}", path.display());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackKind {
    Manual,
    Auto,
    Unknown,
}

/// Classify a subtitle track as manual, automatic, or unknown.
///
/// The ANDROID client omits the `kind` field, so fall back to looking for
/// the `caps=asr` marker in the track URL.
fn track_kind(track: &CaptionTrack) -> TrackKind {
    if track.kind.as_deref() == Some("asr") || track.base_url.contains("caps=asr") {
        TrackKind::Auto
    } else if track.kind.as_deref() == Some("asr") || track.vss_id.as_deref() == Some("manual") {
        TrackKind::Manual
    } else {
        TrackKind::Unknown
    }
}

/// Swap the timedtext format parameter for `json3`.
fn json3_url(base: &str) -> Option<String> {
    let mut url = ::url::Url::parse(base).ok()?;
    // Rebuild the query string, replacing any existing `fmt` with json3.
    let pairs: Vec<(String, String)> = url
        .query_pairs()
        .map(|(k, v)| (k.into_owned(), v.into_owned()))
        .filter(|(k, _)| k != "fmt")
        .collect();
    url.set_query(None);
    {
        let mut q = url.query_pairs_mut();
        for (k, v) in &pairs {
            q.append_pair(k, v);
        }
        q.append_pair("fmt", "json3");
    }
    Some(url.to_string())
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

/// Does the title match the filter?
///
/// The filter is a regular expression. A leading `!` inverts it.
fn title_matches(filter: &str, title: &str) -> bool {
    let (invert, pattern) = match filter.strip_prefix('!') {
        Some(rest) => (true, rest),
        None => (false, filter),
    };

    let matched = match regex::Regex::new(pattern) {
        Ok(re) => re.is_match(title),
        Err(e) => {
            eprintln!("warning: bad match filter `{filter}`: {e}");
            false
        }
    };
    if invert {
        !matched
    } else {
        matched
    }
}

/// Drop the formats that are outside the size limits.
fn filter_by_size(formats: Vec<Format>, cli: &Cli) -> Result<Vec<Format>> {
    let max = match &cli.max_filesize {
        Some(v) => Some(
            output::parse_size(v)
                .ok_or_else(|| Error::other(format!("bad --max-filesize value `{v}`")))?,
        ),
        None => None,
    };
    let min = match &cli.min_filesize {
        Some(v) => Some(
            output::parse_size(v)
                .ok_or_else(|| Error::other(format!("bad --min-filesize value `{v}`")))?,
        ),
        None => None,
    };
    if max.is_none() && min.is_none() {
        return Ok(formats);
    }

    let kept: Vec<Format> = formats
        .into_iter()
        .filter(|f| match f.size_bytes() {
            Some(size) => {
                max.map(|m| size <= m).unwrap_or(true) && min.map(|m| size >= m).unwrap_or(true)
            }
            // Formats without a known size are kept: the limit is about the
            // ones we can measure.
            None => true,
        })
        .collect();

    Ok(kept)
}

/// Print the requested fields. Returns true when something was printed.
fn print_fields(
    cli: &Cli,
    details: &VideoDetails,
    video_id: &str,
    pctx: &PlaylistContext,
) -> bool {
    let vars = TemplateVars {
        title: details.title.clone(),
        id: video_id.to_string(),
        ext: String::new(),
        uploader: details.author.clone().unwrap_or_default(),
        duration_secs: details.duration_secs(),
        resolution: String::new(),
        format_id: String::new(),
        playlist_index: pctx.index,
        playlist_title: pctx.title.clone(),
        playlist_id: pctx.id.clone(),
        epoch: chrono::Utc::now().timestamp() as u64,
    };

    let mut printed = false;
    if cli.get_title {
        println!("{}", vars.title);
        printed = true;
    }
    if cli.get_id {
        println!("{video_id}");
        printed = true;
    }
    if cli.get_duration {
        println!("{}", details.duration_secs().unwrap_or(0));
        printed = true;
    }
    if cli.get_uploader {
        println!("{}", vars.uploader);
        printed = true;
    }
    if let Some(template) = &cli.print {
        println!("{}", output::render(template, &vars));
        printed = true;
    }
    printed
}

/// How many alternative formats to try when googlevideo refuses one.
const MAX_FORMAT_FALLBACKS: usize = 6;

/// Delete the partial files of formats that could not be downloaded.
///
/// They hold the bytes of a different format, so resuming them would
/// produce a corrupt file.
fn remove_stale_parts(details: &VideoDetails, pctx: &PlaylistContext, cli: &Cli) {
    let vars = TemplateVars {
        title: details.title.clone(),
        id: String::new(),
        ext: "mp4".to_string(),
        uploader: details.author.clone().unwrap_or_default(),
        duration_secs: details.duration_secs(),
        resolution: String::new(),
        format_id: String::new(),
        playlist_index: pctx.index,
        playlist_title: pctx.title.clone(),
        playlist_id: pctx.id.clone(),
        epoch: 0,
    };
    let out_path = PathBuf::from(output::sanitize_path(&output::render(&cli.output, &vars)));
    let Some(stem) = out_path.file_stem().map(|s| s.to_string_lossy().to_string()) else {
        return;
    };
    let parent = match out_path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p.to_path_buf(),
        _ => PathBuf::from("."),
    };

    let Ok(entries) = std::fs::read_dir(&parent) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(&stem) && (name.contains(".f") || name.ends_with(".part")) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// How many byte-ranges to fetch in parallel for a stream of this size.
///
/// YouTube throttles a *single* connection to roughly 250 kB/s, but serves
/// parallel range requests at full speed. Splitting a large file into eight
/// ranges took 2 s where one connection took 213 s for the same bytes.
fn fragment_count(cli: &Cli, size: Option<u64>) -> usize {
    if cli.fragments > 0 {
        return cli.fragments;
    }
    // Auto: only bother with parallel ranges when there is enough to gain.
    match size {
        Some(s) if s >= 8 * 1024 * 1024 => 8,
        Some(s) if s >= 2 * 1024 * 1024 => 4,
        _ => 1,
    }
}

fn resolution_of(format: &Format) -> String {
    match (format.width, format.height) {
        (Some(w), Some(h)) => format!("{w}x{h}"),
        (_, Some(h)) => format!("{h}p"),
        _ => String::new(),
    }
}

fn mux_progress_bar(total: Option<u64>, label: &str) -> indicatif::ProgressBar {
    let bar = indicatif::ProgressBar::new(total.unwrap_or(0));
    bar.set_style(crate::download::progress_style());
    bar.set_message(label.to_string());
    bar
}

fn print_formats(formats: &[Format]) {
    println!(
        "{:<6} {:<5} {:<12} {:>4}  {:>10}  {:>10}  {:<14} {:<12}",
        "ID", "EXT", "RESOLUTION", "FPS", "SIZE", "TBR", "VCODEC", "ACODEC"
    );
    let mut sorted: Vec<&Format> = formats.iter().collect();
    sorted.sort_by(|a, b| {
        b.height()
            .cmp(&a.height())
            .then_with(|| b.tbr().cmp(&a.tbr()))
            .then_with(|| a.itag.cmp(&b.itag))
    });
    for f in sorted {
        let vcodec = f.vcodec().unwrap_or("-");
        let acodec = if f.has_video() && !f.has_audio() {
            "video only".to_string()
        } else if !f.has_video() {
            "audio only".to_string()
        } else {
            f.acodec().unwrap_or("-").to_string()
        };
        println!(
            "{:<6} {:<5} {:<12} {:>4}  {:>10}  {:>10}  {:<14} {:<12}",
            f.itag,
            f.ext(),
            if f.height() > 0 {
                resolution_of(f)
            } else {
                "audio only".to_string()
            },
            if f.fps() > 0 { f.fps() } else { 0 },
            f.size_str(),
            format!("{}kbps", f.tbr()),
            vcodec,
            acodec
        );
    }
}

fn build_info_json(
    player: &PlayerResponse,
    selection: &formats::Selection,
    video_id: &str,
) -> String {
    let details = player.video_details.clone().unwrap_or_default();
    let formats_json: Vec<serde_json::Value> = selection
        .videos
        .iter()
        .chain(selection.audios.iter())
        .map(|f| {
            json!({
                "format_id": f.itag.to_string(),
                "ext": f.ext(),
                "height": f.height(),
                "fps": f.fps(),
                "vcodec": f.vcodec(),
                "acodec": f.acodec(),
                "tbr": f.tbr(),
                "filesize": f.size_bytes(),
            })
        })
        .collect();

    let doc = json!({
        "id": video_id,
        "title": details.title,
        "uploader": details.author,
        "channel_id": details.channel_id,
        "description": details.short_description,
        "duration": details.duration_secs(),
        "view_count": details.view_count.as_ref().and_then(|v| v.parse::<u64>().ok()),
        "webpage_url": format!("https://www.youtube.com/watch?v={video_id}"),
        "extractor": "innertube:android",
        "selected_formats": formats_json,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_else(|_| "{}".into())
}

/// Helper for the caption track language list (used by --dump-json).
#[allow(dead_code)]
pub fn caption_languages(player: &PlayerResponse) -> Vec<String> {
    player
        .captions
        .as_ref()
        .and_then(|c| c.player_captions_tracklist_renderer.as_ref())
        .map(|t| {
            t.caption_tracks
                .iter()
                .map(|c: &CaptionTrack| c.language_code.clone().unwrap_or_default())
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn cli(args: &[&str]) -> Cli {
        let mut full = vec!["ytgrab", "https://example.com/watch?v=abc"];
        full.extend_from_slice(args);
        Cli::parse_from(full)
    }

    #[test]
    fn the_filter_matches_the_title() {
        assert!(title_matches("Rick", "Rick Astley - Live"));
        assert!(!title_matches("Minecraft", "Rick Astley - Live"));
    }

    #[test]
    fn a_bang_inverts_the_filter() {
        assert!(title_matches("!Minecraft", "Rick Astley - Live"));
        assert!(!title_matches("!Rick", "Rick Astley - Live"));
    }

    #[test]
    fn a_broken_filter_matches_nothing() {
        assert!(!title_matches("[unclosed", "Never Gonna Give You Up"));
    }

    #[test]
    fn the_size_limits_drop_formats() {
        // The size comes from `content_length`.
        let format = Format {
            itag: 137,
            content_length: Some(80 * 1024 * 1024),
            ..Default::default()
        };

        let kept = filter_by_size(vec![format.clone()], &cli(&["--max-filesize", "50M"])).unwrap();
        assert!(kept.is_empty(), "80MB does not fit into 50MB");

        let kept = filter_by_size(vec![format.clone()], &cli(&["--min-filesize", "50M"])).unwrap();
        assert_eq!(kept.len(), 1);

        let kept = filter_by_size(vec![format], &cli(&[])).unwrap();
        assert_eq!(kept.len(), 1, "without limits nothing is dropped");
    }

    #[test]
    fn a_bad_size_is_an_error() {
        let result = filter_by_size(vec![], &cli(&["--max-filesize", "banana"]));
        assert!(result.is_err());
    }
}
