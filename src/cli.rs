//! Command line interface definition.

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser, Debug)]
#[command(
    name = "ytgrab",
    version,
    about = "A fast YouTube downloader written in Rust",
    long_about = None
)]
pub struct Cli {
    /// Video URL, playlist URL, or bare video id
    pub url: String,

    /// Format selector: best, worst, bestvideo+bestaudio, 1080, bv[height<=720]+ba
    #[arg(short = 'f', long, default_value = "best")]
    pub format: String,

    /// Output template, yt-dlp style
    #[arg(short = 'o', long, default_value = "%(title)s.%(ext)s")]
    pub output: String,

    /// List available formats and exit
    #[arg(long)]
    pub list_formats: bool,

    /// Print the direct URL(s) instead of downloading
    #[arg(long)]
    pub get_url: bool,

    /// Print the full metadata as JSON and exit
    #[arg(long)]
    pub dump_json: bool,

    /// Write the metadata alongside the download
    #[arg(long)]
    pub write_info_json: bool,

    /// Write the thumbnail alongside the download
    #[arg(long)]
    pub write_thumbnail: bool,

    /// Download manually written subtitles
    #[arg(long)]
    pub write_subs: bool,

    /// Download automatically generated subtitles
    #[arg(long)]
    pub write_auto_subs: bool,

    /// Subtitle languages (comma separated)
    #[arg(long, default_value = "en")]
    pub sub_lang: String,

    /// Subtitle format: srt or vtt
    #[arg(long, default_value = "srt")]
    pub sub_format: String,

    /// Extract audio only
    #[arg(short = 'x', long)]
    pub extract_audio: bool,

    /// Audio format when extracting audio
    #[arg(long, default_value = "mp3")]
    pub audio_format: String,

    /// Keep the separate video/audio files after muxing
    #[arg(long)]
    pub keep_files: bool,

    /// Do not mux video and audio (keep separate streams)
    #[arg(long)]
    pub no_mux: bool,

    /// Cookies file in Netscape format
    #[arg(long)]
    pub cookies: Option<PathBuf>,

    /// Read cookies from a browser: chrome, brave, chromium, edge, firefox
    #[arg(long, value_name = "BROWSER")]
    pub cookies_from_browser: Option<String>,

    /// InnerTube client to use: android, ios, mweb, web, tv
    #[arg(long, default_value = "android")]
    pub client: String,

    /// Parallel byte-range segments per stream (0 = auto, 1 = sequential)
    #[arg(long, default_value_t = 0)]
    pub fragments: usize,

    /// Size of a single range request, e.g. 8M (0 = the tuned default)
    #[arg(long, value_name = "SIZE", default_value = "0")]
    pub chunk_size: String,

    /// Retry attempts per request
    #[arg(long, default_value_t = 5)]
    pub retries: u32,

    /// Download every video in a playlist
    #[arg(long)]
    pub playlist: bool,

    /// Never touch playlists (default when the url carries a list= param)
    #[arg(long)]
    pub no_playlist: bool,

    /// Prefer Opus audio over AAC (better quality, fewer players support it)
    #[arg(long)]
    pub prefer_opus: bool,

    /// File that lists the videos already downloaded; they are skipped
    #[arg(long, value_name = "FILE")]
    pub download_archive: Option<PathBuf>,

    /// Print this template for every video (yt-dlp template syntax)
    #[arg(long, value_name = "TEMPLATE")]
    pub print: Option<String>,

    /// Do not download anything, only print
    #[arg(long)]
    pub skip_download: bool,

    /// Print the title of every video
    #[arg(long)]
    pub get_title: bool,

    /// Print the id of every video
    #[arg(long)]
    pub get_id: bool,

    /// Print the duration of every video
    #[arg(long)]
    pub get_duration: bool,

    /// Print the uploader of every video
    #[arg(long)]
    pub get_uploader: bool,

    /// Only download videos whose title matches this regular expression
    /// (prefix with ! to invert it)
    #[arg(long, value_name = "REGEX")]
    pub match_filter: Option<String>,

    /// Skip videos whose file already exists
    #[arg(long)]
    pub no_overwrites: bool,

    /// Overwrite existing files (the default)
    #[arg(long)]
    pub force_overwrites: bool,

    /// Skip formats bigger than this, e.g. 50M or 1G
    #[arg(long, value_name = "SIZE")]
    pub max_filesize: Option<String>,

    /// Skip formats smaller than this, e.g. 50M or 1G
    #[arg(long, value_name = "SIZE")]
    pub min_filesize: Option<String>,

    /// Seconds to wait between videos
    #[arg(long, value_name = "SECONDS", default_value_t = 0.0)]
    pub sleep_requests: f64,

    /// Run this shell command for every downloaded file
    #[arg(long, value_name = "CMD")]
    pub exec: Option<String>,

    /// Run this shell command before every download
    #[arg(long, value_name = "CMD")]
    pub exec_before_download: Option<String>,

    /// Start of the playlist (1 based)
    #[arg(long, value_name = "N")]
    pub playlist_start: Option<usize>,

    /// End of the playlist (1 based, inclusive)
    #[arg(long, value_name = "N")]
    pub playlist_end: Option<usize>,

    /// Which items of the playlist, e.g. 1:3,7,-2
    #[arg(long, value_name = "SPEC")]
    pub playlist_items: Option<String>,

    /// Download the playlist backwards
    #[arg(long)]
    pub playlist_reverse: bool,

    /// Download the playlist in a random order
    #[arg(long)]
    pub playlist_random: bool,

    /// Stop after this many videos
    #[arg(long, value_name = "N")]
    pub max_downloads: Option<usize>,

    /// Route everything through this proxy
    #[arg(long, value_name = "URL")]
    pub proxy: Option<String>,

    /// Socket timeout in seconds
    #[arg(long, value_name = "SECONDS", default_value_t = 30.0)]
    pub socket_timeout: f64,

    /// Download straight to the final file, without a .part file
    #[arg(long)]
    pub no_part: bool,

    /// Stop the whole run when a file already exists
    #[arg(long)]
    pub break_on_existing: bool,

    /// List the available subtitles and exit
    #[arg(long)]
    pub list_subs: bool,

    /// Write the video description to a file
    #[arg(long)]
    pub write_description: bool,

    /// Write the comments to a file
    #[arg(long)]
    pub write_comments: bool,

    /// Cut the file name to this many characters
    #[arg(long, value_name = "N")]
    pub trim_filenames: Option<usize>,

    /// Keep only ASCII characters in the file name
    #[arg(long)]
    pub restrict_filenames: bool,

    /// Replace the characters Windows does not allow
    #[arg(long)]
    pub windows_filenames: bool,

    /// Write the --print output to a file: --print-to-file TEMPLATE FILE
    #[arg(long, num_args = 2, value_names = ["TEMPLATE", "FILE"])]
    pub print_to_file: Vec<String>,

    /// Only videos uploaded after this date (YYYYMMDD or today-2weeks)
    #[arg(long, value_name = "DATE")]
    pub dateafter: Option<String>,

    /// Only videos uploaded before this date
    #[arg(long, value_name = "DATE")]
    pub datebefore: Option<String>,

    /// Read options from this file
    #[arg(long, value_name = "FILE")]
    pub config: Option<PathBuf>,

    /// Do not use the session cache
    #[arg(long)]
    pub no_cache_dir: bool,

    /// Verbose logging
    #[arg(short = 'v', long)]
    pub verbose: bool,

    /// Only print errors
    #[arg(short = 'q', long)]
    pub quiet: bool,
}
