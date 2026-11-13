//! Stream downloading.
//!
//! Two modes:
//!   * single stream with HTTP range resume (restart-friendly)
//!   * segmented parallel download: N byte-ranges fetched concurrently, then
//!     concatenated. This is what actually saturates a fast connection when
//!     YouTube throttles a single connection.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use tokio::io::AsyncWriteExt;

use crate::error::{Error, Result};

/// The size of a single range request.
///
/// Two different things depend on this number:
///
/// * googlevideo answers `403 Forbidden` for ranges that are too large, so
///   the chunk has to stay below that limit (about 15 MiB for the worst
///   case, and much smaller for some formats).
/// * the server shapes the bandwidth of a long request: measured on the same
///   file, 8 MiB chunks ran at 18.8 MB/s while 15 MiB chunks collapsed to
///   3.2 MB/s. Staying under the cliff is worth a lot more than fewer
///   requests.
///
/// 8 MiB sits comfortably below both limits.
const CHUNK_SIZE: u64 = 8 * 1024 * 1024;

/// The smallest range we bother trying.
const MIN_CHUNK_SIZE: u64 = 256 * 1024;

#[derive(Debug, Clone)]
pub struct Downloader {
    http: reqwest::Client,
    user_agent: String,
    pub retries: u32,
    /// Number of parallel byte-range segments (1 = plain sequential).
    pub fragments: usize,
}

impl Downloader {
    pub fn new(user_agent: &str) -> Result<Self> {
        Self::with_cookie_jar(user_agent, None)
    }

    /// Build a downloader that sends the given cookies with every request.
    pub fn with_cookie_jar(
        user_agent: &str,
        jar: Option<std::sync::Arc<reqwest::cookie::Jar>>,
    ) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .user_agent(user_agent.to_string())
            .pool_max_idle_per_host(8);
        if let Some(jar) = jar {
            builder = builder.cookie_provider(jar);
        }
        let http = builder.build()?;
        Ok(Self {
            http,
            user_agent: user_agent.to_string(),
            retries: 5,
            fragments: 1,
        })
    }

    pub fn user_agent(&self) -> &str {
        &self.user_agent
    }

    /// Plain GET of a small resource (thumbnails, subtitle tracks).
    pub async fn get_bytes(&self, url: &str) -> Result<Vec<u8>> {
        let resp = self
            .http
            .get(url)
            .header("Accept-Language", "en-US,en;q=0.9")
            .send()
            .await?
            .error_for_status()?;
        Ok(resp.bytes().await?.to_vec())
    }

    /// Download `url` into `dest`, resuming a partial `.part` file if present.
    ///
    /// Returns the number of bytes written to disk for this call.
    pub async fn download(
        &self,
        url: &str,
        dest: &Path,
        bar: &ProgressBar,
        total_hint: Option<u64>,
        fragments: usize,
        itag: i64,
    ) -> Result<u64> {
        let _lock = LockFile::acquire(dest)?;
        if fragments > 1 {
            if let Some(total) = total_hint {
                if total > 1024 * 1024 {
                    return self
                        .download_segmented(url, dest, bar, total, fragments, itag)
                        .await;
                }
            }
        }
        self.download_single(url, dest, bar, total_hint, itag).await
    }

    async fn download_single(
        &self,
        url: &str,
        dest: &Path,
        bar: &ProgressBar,
        total_hint: Option<u64>,
        itag: i64,
    ) -> Result<u64> {
        let part_path = part_path(dest);
        if let Some(parent) = part_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let mut existing = tokio::fs::metadata(&part_path).await.ok().map(|m| m.len());

        // How big is the file? Trust the player response, otherwise ask the
        // server with a one byte range request.
        let mut total = total_hint.filter(|t| *t > 0);
        if total.is_none() {
            match self.probe_size(url).await {
                Ok(t) => total = Some(t),
                Err(e) => tracing::debug!("could not probe the size: {e}"),
            }
        }

        // Already complete?
        if let (Some(len), Some(t)) = (existing, total) {
            if len >= t {
                finalize(&part_path, dest).await?;
                bar.set_position(t);
                return Ok(0);
            }
        }
        if let Some(t) = total {
            bar.set_length(t);
        }

        let mut attempt = 0u32;
        // Start with the biggest range we are willing to ask for and shrink
        // it if the server refuses. Some formats are only served in small
        // windows, and 403 means "too big", not "expired".
        let mut chunk = CHUNK_SIZE;
        loop {
            let offset = existing.unwrap_or(0);
            if let Some(t) = total {
                if offset >= t {
                    break;
                }
            }

            // A bounded range of at most `chunk` bytes. An open ended range
            // (`bytes=N-`) is rejected with 403, and so is a range that
            // covers more than the server is willing to serve.
            let end = match total {
                Some(t) => (offset + chunk - 1).min(t - 1),
                None => offset + chunk - 1,
            };

            // The Range header is always sent, even on a fresh download:
            // without it googlevideo answers 403.
            let req = self
                .http
                .get(url)
                .header("Accept-Encoding", "identity")
                .header("Range", format!("bytes={offset}-{end}"));

            let resp = match req.send().await {
                Ok(r) => r,
                Err(e) => {
                    attempt += 1;
                    if attempt > self.retries {
                        return Err(Error::Http(e));
                    }
                    tokio::time::sleep(backoff(attempt)).await;
                    continue;
                }
            };

            let status = resp.status();
            if status == 403 || status == 410 {
                // 403 from googlevideo usually means the range was too
                // large. Ask for less and try again before giving up.
                if status == 403 && chunk > MIN_CHUNK_SIZE {
                    chunk = (chunk / 2).max(MIN_CHUNK_SIZE);
                    tracing::debug!("range rejected, retrying with {chunk} byte chunks");
                    continue;
                }
                return Err(Error::SignedUrl {
                    status: status.as_u16(),
                    itag,
                });
            }
            if !status.is_success() {
                attempt += 1;
                if attempt > self.retries {
                    return Err(Error::other(format!("download failed with HTTP {status}")));
                }
                tokio::time::sleep(backoff(attempt)).await;
                continue;
            }

            let partial = status == 206;
            if !partial && offset > 0 {
                // The server ignored our range header: start over.
                existing = Some(0);
                attempt += 1;
                if attempt > self.retries {
                    return Err(Error::other("server ignored the range header"));
                }
                continue;
            }

            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(partial && offset > 0)
                .truncate(!partial || offset == 0)
                .open(&part_path)
                .await?;

            let mut stream = resp.bytes_stream();
            let mut written: u64 = 0;
            let mut broken = false;
            while let Some(chunk) = stream.next().await {
                let chunk = match chunk {
                    Ok(c) => c,
                    Err(e) => {
                        // The connection dropped mid-transfer. Keep what we
                        // have and reconnect with the next range.
                        tracing::warn!("transfer interrupted: {e}; resuming");
                        broken = true;
                        break;
                    }
                };
                file.write_all(&chunk).await?;
                written += chunk.len() as u64;
                bar.inc(chunk.len() as u64);
            }
            file.flush().await?;
            drop(file);

            existing = Some(offset + written);

            if let Some(t) = total {
                if existing.unwrap_or(0) >= t {
                    break;
                }
            } else if written < (end - offset + 1) && !broken {
                // Short read: the server has nothing more to give.
                break;
            }

            if broken {
                attempt += 1;
                if attempt > self.retries {
                    return Err(Error::other("download kept breaking mid-transfer"));
                }
                tokio::time::sleep(backoff(attempt)).await;
            }
        }

        finalize(&part_path, dest).await?;
        check_size(dest, total).await?;
        Ok(existing.unwrap_or(0))
    }

    /// Ask the server how big the file is, using a one byte range request.
    async fn probe_size(&self, url: &str) -> Result<u64> {
        let resp = self
            .http
            .get(url)
            .header("Accept-Encoding", "identity")
            .header("Range", "bytes=0-0")
            .send()
            .await?;
        if !resp.status().is_success() {
            return Err(Error::other(format!(
                "size probe failed with HTTP {}",
                resp.status()
            )));
        }
        let content_range = resp
            .headers()
            .get("content-range")
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_string();
        // "bytes 0-0/944298744"
        let total = content_range
            .rsplit('/')
            .next()
            .and_then(|s| s.trim().parse::<u64>().ok())
            .ok_or_else(|| Error::other("no total in the content-range header"))?;
        Ok(total)
    }

    /// Split the file into `fragments` byte ranges, fetch them concurrently.
    async fn download_segmented(
        &self,
        url: &str,
        dest: &Path,
        bar: &ProgressBar,
        total: u64,
        fragments: usize,
        itag: i64,
    ) -> Result<u64> {
        let n = fragments.max(1);
        let chunk = total.div_ceil(n as u64);
        let part_path = part_path(dest);
        if let Some(parent) = part_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

        let downloaded = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::new();

        for i in 0..n {
            let start = i as u64 * chunk;
            if start >= total {
                break;
            }
            let end = ((i as u64 + 1) * chunk).min(total) - 1;
            let seg_path = seg_path(&part_path, i);
            let url = url.to_string();
            let http = self.http.clone();
            let bar = bar.clone();
            let downloaded = Arc::clone(&downloaded);
            let retries = self.retries;

            handles.push(tokio::spawn(async move {
                download_range(
                    &http, &url, &seg_path, start, end, &bar, &downloaded, retries, itag,
                )
                .await
            }));
        }

        for h in handles {
            match h.await {
                Ok(Ok(_)) => {}
                Ok(Err(e)) => return Err(e),
                Err(e) => return Err(Error::other(format!("segment task panicked: {e}"))),
            }
        }

        // Concatenate the segments into the final file.
        let mut out = File::create(&part_path)?;
        let mut seg_index = 0;
        loop {
            let seg = seg_path(&part_path, seg_index);
            if !seg.exists() {
                break;
            }
            let mut f = File::open(&seg)?;
            let mut buf = vec![0u8; 1 << 20];
            loop {
                let n = f.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                out.write_all(&buf[..n])?;
            }
            seg_index += 1;
        }
        out.flush()?;
        drop(out);

        finalize(&part_path, dest).await?;
        check_size(dest, Some(total)).await?;
        Ok(total)
    }
}

#[allow(clippy::too_many_arguments)]
async fn download_range(
    http: &reqwest::Client,
    url: &str,
    dest: &Path,
    start: u64,
    end: u64,
    bar: &ProgressBar,
    downloaded: &AtomicU64,
    retries: u32,
    itag: i64,
) -> Result<()> {
    let expected = end - start + 1;

    // Resume a partially downloaded segment.
    let mut have = tokio::fs::metadata(dest).await.ok().map(|m| m.len()).unwrap_or(0);
    if have >= expected {
        downloaded.fetch_add(expected, Ordering::Relaxed);
        return Ok(());
    }

    let mut attempt = 0u32;
    while have < expected {
        let from = start + have;
        // Bounded range: googlevideo answers 403 for anything longer.
        let to = (from + CHUNK_SIZE - 1).min(end);
        let resp = http
            .get(url)
            .header("Accept-Encoding", "identity")
            .header("Range", format!("bytes={from}-{to}"))
            .send()
            .await;

        let resp = match resp {
            Ok(r) if r.status().is_success() => r,
            Ok(r) if r.status() == 416 => break,
            Ok(r) if r.status() == 403 || r.status() == 410 => {
                return Err(Error::SignedUrl {
                    status: r.status().as_u16(),
                    itag,
                });
            }
            Ok(r) => {
                attempt += 1;
                if attempt > retries {
                    return Err(Error::other(format!("segment failed: HTTP {}", r.status())));
                }
                tokio::time::sleep(backoff(attempt)).await;
                continue;
            }
            Err(e) => {
                attempt += 1;
                if attempt > retries {
                    return Err(Error::Http(e));
                }
                tokio::time::sleep(backoff(attempt)).await;
                continue;
            }
        };

        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .append(true)
            .open(dest)
            .await?;
        let mut stream = resp.bytes_stream();
        let mut written: u64 = 0;
        let mut broken = false;
        while let Some(chunk) = stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(_) => {
                    broken = true;
                    break;
                }
            };
            file.write_all(&chunk).await?;
            written += chunk.len() as u64;
            downloaded.fetch_add(written, Ordering::Relaxed);
            bar.inc(chunk.len() as u64);
        }
        file.flush().await?;
        drop(file);

        have += written;
        if written == 0 {
            attempt += 1;
            if attempt > retries {
                return Err(Error::other("segment made no progress"));
            }
            tokio::time::sleep(backoff(attempt)).await;
        } else if broken {
            attempt += 1;
            if attempt > retries {
                return Err(Error::other("segment kept breaking mid-transfer"));
            }
        }
    }

    Ok(())
}

fn backoff(attempt: u32) -> Duration {
    Duration::from_millis(500 * 2u64.pow(attempt.min(5)))
}

/// Guards a destination file against two downloaders writing at once.
///
/// Without this, two instances appending to the same `.part` file produce a
/// file that is longer than the real stream and silently corrupt it.
struct LockFile {
    path: PathBuf,
}

impl LockFile {
    fn acquire(dest: &Path) -> Result<Self> {
        let mut path = dest.as_os_str().to_os_string();
        path.push(".lock");
        let path = PathBuf::from(path);
        match std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&path)
        {
            Ok(mut f) => {
                let _ = writeln!(f, "{}", std::process::id());
                Ok(LockFile { path })
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                // A crashed process leaves the lock behind. If the pid that
                // owned it is gone, take the lock over.
                if let Some(pid) = lock_owner(&path) {
                    if !process_alive(pid) {
                        let _ = std::fs::remove_file(&path);
                        return LockFile::acquire(dest);
                    }
                    return Err(Error::other(format!(
                        "another download (pid {pid}) is already writing to {}",
                        dest.display()
                    )));
                }
                Err(Error::other(format!(
                    "another download is already writing to {}",
                    dest.display()
                )))
            }
            Err(e) => Err(Error::Io(e)),
        }
    }
}

/// The pid recorded inside a lock file.
fn lock_owner(path: &Path) -> Option<u32> {
    let content = std::fs::read_to_string(path).ok()?;
    content.trim().parse::<u32>().ok()
}

/// Is this process still running? Linux only, but that is where we run.
fn process_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
}

impl Drop for LockFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Make sure the file on disk is as long as the server said it would be.
///
/// A download that stops early looks fine until it is muxed, so it is
/// checked here and reported instead.
async fn check_size(dest: &Path, total: Option<u64>) -> Result<()> {
    let Some(expected) = total.filter(|t| *t > 0) else {
        return Ok(());
    };
    let actual = tokio::fs::metadata(dest).await.map(|m| m.len()).unwrap_or(0);
    if actual != expected {
        return Err(Error::other(format!(
            "the download is incomplete: {} bytes of {} ({:.1}%)",
            actual,
            expected,
            actual as f64 / expected as f64 * 100.0
        )));
    }
    Ok(())
}

fn part_path(dest: &Path) -> PathBuf {
    let mut s = dest.as_os_str().to_os_string();
    s.push(".part");
    PathBuf::from(s)
}

fn seg_path(part: &Path, index: usize) -> PathBuf {
    let mut s = part.as_os_str().to_os_string();
    s.push(format!(".seg{index}"));
    PathBuf::from(s)
}

async fn finalize(part: &Path, dest: &Path) -> Result<()> {
    tokio::fs::rename(part, dest).await?;
    // Clean up any leftover segment files.
    let mut idx = 0;
    loop {
        let seg = seg_path(part, idx);
        if !seg.exists() {
            break;
        }
        let _ = tokio::fs::remove_file(&seg).await;
        idx += 1;
    }
    Ok(())
}

/// Build a yt-dlp-ish progress bar style.
pub fn progress_style() -> ProgressStyle {
    ProgressStyle::with_template(
        "{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {per_sec}",
    )
    .unwrap_or_else(|_| ProgressStyle::default_bar())
    .progress_chars("=> ")
}

/// Append the second half of a file, used when stitching separate files.
pub fn concat_files(inputs: &[PathBuf], out: &Path) -> Result<()> {
    let mut out_file = File::create(out)?;
    for input in inputs {
        let mut f = File::open(input)?;
        let mut buf = vec![0u8; 1 << 20];
        loop {
            let n = f.read(&mut buf)?;
            if n == 0 {
                break;
            }
            out_file.write_all(&buf[..n])?;
        }
    }
    out_file.flush()?;
    Ok(())
}

/// Copy the first `n` bytes of `src` into `dst` (used for tagging).
pub fn copy_prefix(src: &Path, dst: &Path, n: u64) -> Result<()> {
    let mut s = File::open(src)?;
    let mut d = File::create(dst)?;
    let mut buf = vec![0u8; 1 << 20];
    let mut remaining = n;
    while remaining > 0 {
        let want = std::cmp::min(remaining, buf.len() as u64) as usize;
        let read = s.read(&mut buf[..want])?;
        if read == 0 {
            break;
        }
        d.write_all(&buf[..read])?;
        remaining -= read as u64;
    }
    d.flush()?;
    Ok(())
}

/// Seek helper used when patching files in place.
pub fn seek_start(file: &mut File) -> Result<()> {
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}
