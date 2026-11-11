//! Resume and unstable-connection tests.
//!
//! These run against a local server that misbehaves on purpose: it drops
//! connections mid-transfer and refuses the first request without a range
//! header, the way googlevideo does.

use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use ytgrab::download::Downloader;

/// The bytes the fake server serves.
fn payload() -> Vec<u8> {
    (0..512 * 1024u32).map(|i| (i % 251) as u8).collect()
}

/// A server that drops connections and demands range requests.
///
/// `drop_every` makes every n-th connection die after a few bytes, and the
/// first request without a `Range` header is answered with `403`, exactly
/// like googlevideo does.
struct FlakyServer {
    addr: SocketAddr,
    requests: Arc<AtomicUsize>,
}

async fn start_flaky_server(drop_every: usize) -> FlakyServer {
    let data = payload();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let requests = Arc::new(AtomicUsize::new(0));

    let counter = Arc::clone(&requests);
    tokio::spawn(async move {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                continue;
            };
            let data = data.clone();
            let counter = Arc::clone(&counter);
            tokio::spawn(async move {
                let n = counter.fetch_add(1, Ordering::SeqCst);
                handle(&mut socket, &data, n, drop_every).await;
            });
        }
    });

    FlakyServer { addr, requests }
}

async fn handle(
    socket: &mut tokio::net::TcpStream,
    data: &[u8],
    request_number: usize,
    drop_every: usize,
) {
    // Read the request head.
    let mut head = Vec::new();
    let mut byte = [0u8; 1];
    while socket.read(&mut byte).await.unwrap_or(0) > 0 {
        head.push(byte[0]);
        if head.ends_with(b"\r\n\r\n") {
            break;
        }
        if head.len() > 16 * 1024 {
            return;
        }
    }
    let head = String::from_utf8_lossy(&head).to_string();

    let range = head
        .lines()
        .find_map(|l| {
            let lower = l.to_ascii_lowercase();
            lower.strip_prefix("range:").map(|v| v.trim().to_string())
        })
        .and_then(|v| {
            v.strip_prefix("bytes=")
                .map(|s| s.split('-').next().unwrap_or("0").trim().parse::<usize>().ok())
                .flatten()
        });

    let Some(start) = range else {
        // No range header: googlevideo answers 403.
        let _ = socket
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await;
        let _ = socket.flush().await;
        return;
    };

    let total = data.len();
    let end = total - 1;
    let header = format!(
        "HTTP/1.1 206 Partial Content\r\nContent-Length: {}\r\nContent-Range: bytes {}-{}/{}\r\nAccept-Ranges: bytes\r\n\r\n",
        end - start + 1,
        start,
        end,
        total
    );
    let _ = socket.write_all(header.as_bytes()).await;

    // Every n-th connection dies after a short burst, starting with the
    // first one so the client has to reconnect.
    let drop_after = if drop_every > 0 && request_number % drop_every == 0 {
        64 * 1024
    } else {
        usize::MAX
    };

    let mut sent = 0;
    let mut body = &data[start..];
    while !body.is_empty() {
        let take = body.len().min(16 * 1024).min(drop_after.saturating_sub(sent));
        if take == 0 {
            break;
        }
        if socket.write_all(&body[..take]).await.is_err() {
            return;
        }
        body = &body[take..];
        sent += take;
    }
    let _ = socket.flush().await;
    // Drop the connection without a clean close.
    let _ = socket.shutdown().await;
}

fn temp_path(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("ytgrab-test-{name}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&dir);
    dir.join("video.mp4")
}

#[tokio::test]
async fn resumes_when_the_server_keeps_dropping_the_connection() {
    let server = start_flaky_server(3).await;
    let dest = temp_path("flaky");
    let _ = std::fs::remove_file(&dest);
    let _ = std::fs::remove_file(dest.with_extension("mp4.part"));

    let downloader = Downloader::new("ytgrab-test").unwrap();
    let bar = indicatif::ProgressBar::hidden();
    let url = format!("http://{}/videoplayback", server.addr);

    let written = downloader
        .download(&url, &dest, &bar, Some(payload().len() as u64), 1, 137)
        .await
        .expect("the download should survive dropped connections");

    let got = std::fs::read(&dest).unwrap();
    assert_eq!(got.len(), payload().len(), "the file should be complete");
    assert_eq!(got, payload(), "the bytes should be in order");
    assert!(written > 0);
    assert!(
        server.requests.load(Ordering::SeqCst) > 1,
        "the client should have reconnected"
    );
}

#[tokio::test]
async fn always_sends_a_range_header() {
    // The server answers 403 to any request without a range header, the way
    // googlevideo does. The download has to work anyway, which means the
    // range header must always be there - even for the very first request.
    let server = start_flaky_server(0).await;
    let dest = temp_path("norange");
    let _ = std::fs::remove_file(&dest);

    let downloader = Downloader::new("ytgrab-test").unwrap();
    let bar = indicatif::ProgressBar::hidden();
    let url = format!("http://{}/videoplayback", server.addr);

    downloader
        .download(&url, &dest, &bar, Some(payload().len() as u64), 1, 137)
        .await
        .expect("the download should work");

    assert_eq!(std::fs::read(&dest).unwrap(), payload());
}

#[tokio::test]
async fn picks_up_where_a_previous_run_stopped() {
    let server = start_flaky_server(0).await;
    let dest = temp_path("resume");
    let _ = std::fs::remove_file(&dest);

    // Pretend a previous run got half way.
    let part = dest.with_extension("mp4.part");
    let data = payload();
    let half = data.len() / 2;
    std::fs::write(&part, &data[..half]).unwrap();

    let downloader = Downloader::new("ytgrab-test").unwrap();
    let bar = indicatif::ProgressBar::hidden();
    let url = format!("http://{}/videoplayback", server.addr);

    downloader
        .download(&url, &dest, &bar, Some(data.len() as u64), 1, 137)
        .await
        .expect("the download should resume");

    let got = std::fs::read(&dest).unwrap();
    assert_eq!(got, data, "the resumed file should match byte for byte");
    assert!(!part.exists(), "the partial file should be cleaned up");
}

#[tokio::test]
async fn parallel_segments_produce_the_same_bytes() {
    let server = start_flaky_server(0).await;
    let sequential = temp_path("seq");
    let parallel = temp_path("par");
    let _ = std::fs::remove_file(&sequential);
    let _ = std::fs::remove_file(&parallel);

    let downloader = Downloader::new("ytgrab-test").unwrap();
    let bar = indicatif::ProgressBar::hidden();
    let url = format!("http://{}/videoplayback", server.addr);

    downloader
        .download(&url, &sequential, &bar, Some(payload().len() as u64), 1, 137)
        .await
        .unwrap();
    downloader
        .download(&url, &parallel, &bar, Some(payload().len() as u64), 8, 137)
        .await
        .unwrap();

    assert_eq!(
        std::fs::read(&sequential).unwrap(),
        std::fs::read(&parallel).unwrap(),
        "parallel and sequential downloads must be identical"
    );
}
