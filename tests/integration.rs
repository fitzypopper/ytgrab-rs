//! Integration tests.
//!
//! The pure-logic tests run offline. The network tests are `#[ignore]`d so
//! `cargo test` stays fast and deterministic:
//!
//! ```sh
//! cargo test -- --ignored --nocapture
//! ```

use ytgrab::formats::{select, Selector};
use ytgrab::innertube::client::{ClientContext, InnerTube};
use ytgrab::innertube::player;
use ytgrab::output::{render, sanitize, TemplateVars};
use ytgrab::subtitles::{parse_json3, to_srt, to_vtt};
use ytgrab::url::{parse, Target};

// ---------------------------------------------------------------------------
// offline
// ---------------------------------------------------------------------------

#[test]
fn parses_every_url_shape() {
    let cases = [
        ("https://www.youtube.com/watch?v=dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("https://youtu.be/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("https://www.youtube.com/shorts/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("https://www.youtube.com/embed/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("https://www.youtube.com/live/dQw4w9WgXcQ", "dQw4w9WgXcQ"),
        ("https://m.youtube.com/watch?v=dQw4w9WgXcQ&t=30", "dQw4w9WgXcQ"),
        ("dQw4w9WgXcQ", "dQw4w9WgXcQ"),
    ];
    for (input, expected) in cases {
        match parse(input).unwrap() {
            Target::Video { id, .. } => assert_eq!(id, expected, "for {input}"),
            other => panic!("expected a video target for {input}, got {other:?}"),
        }
    }
}

#[test]
fn playlist_urls_are_recognised() {
    assert_eq!(
        parse("https://www.youtube.com/playlist?list=PLabc").unwrap(),
        Target::Playlist { id: "PLabc".into() }
    );
}

#[test]
fn selector_grammar() {
    // Well formed.
    for s in [
        "best",
        "worst",
        "bv",
        "ba",
        "1080",
        "bestvideo[height<=720]",
        "bv[ext=mp4][height<=1080]+ba",
        "best[vcodec^=avc1]",
    ] {
        assert!(Selector::parse(s).is_ok(), "should parse: {s}");
    }
    // Broken.
    for s in ["", "notaselector", "bestvideo[height<=720"] {
        assert!(Selector::parse(s).is_err(), "should fail: {s}");
    }
}

#[test]
fn templates_render_like_yt_dlp() {
    let vars = TemplateVars {
        title: "Video Title".into(),
        id: "abc12345678".into(),
        ext: "mp4".into(),
        uploader: "Someone".into(),
        duration_secs: Some(61),
        resolution: "1920x1080".into(),
        format_id: "137+140".into(),
        playlist_index: Some(2),
        playlist_title: "My Playlist".into(),
        playlist_id: "PLabc".into(),
        filepath: String::new(),
        filename: String::new(),
        epoch: 1_700_000_000,
    };
    assert_eq!(render("%(title)s.%(ext)s", &vars), "Video Title.mp4");
    assert_eq!(
        render("%(playlist_index)s - %(title)s.%(ext)s", &vars),
        "2 - Video Title.mp4"
    );
    assert_eq!(render("%(uploader)s/%(id)s.%(ext)s", &vars), "Someone/abc12345678.mp4");
    assert_eq!(render("%(duration)s", &vars), "61", "duration is in seconds");
    assert_eq!(render("%(duration_string)s", &vars), "1:01");
    assert_eq!(render("%(epoch)s", &vars), "1700000000");
}

#[test]
fn filenames_are_sanitised() {
    assert_eq!(sanitize("a/b\\c:d*e?f"), "a_b_c_d_e_f");
    assert_eq!(sanitize("  spaced  "), "spaced");
    assert_eq!(sanitize("..."), "video");
}

#[test]
fn subtitles_round_trip() {
    let body = r#"{"events":[
        {"tStartMs":0,"dDurationMs":1000,"segs":[{"utf8":"one"}]},
        {"tStartMs":1000,"dDurationMs":1000,"segs":[{"utf8":"two"}]}
    ]}"#;
    let cues = parse_json3(body).unwrap();
    assert_eq!(cues.len(), 2);
    assert!(to_srt(&cues).contains("00:00:00,000 --> 00:00:01,000"));
    assert!(to_vtt(&cues).starts_with("WEBVTT"));
    assert!(to_vtt(&cues).contains("00:00:01.000 --> 00:00:02.000"));
}

// ---------------------------------------------------------------------------
// network (ignored by default)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "hits the network"]
async fn fetches_a_real_video() {
    let it = InnerTube::new(ClientContext::android()).unwrap();
    let player = player::fetch(&it, "dQw4w9WgXcQ").await.unwrap();

    assert_eq!(player.playability_status.status, "OK");
    let formats = player::all_formats(player.streaming_data.as_ref().unwrap());
    assert!(formats.len() > 5);

    // Every format must carry a usable URL: that is the whole point of
    // using the ANDROID client.
    for f in &formats {
        assert!(f.url.is_some(), "format {} has no url", f.itag);
    }

    let selection = select(&Selector::parse("best").unwrap(), &formats).unwrap();
    assert_eq!(selection.videos.len(), 1);
    assert_eq!(selection.audios.len(), 1);
    assert!(selection.videos[0].height() >= 720);
}

#[tokio::test]
#[ignore = "hits the network"]
async fn lists_a_playlist() {
    use ytgrab::innertube::browse;
    let it = InnerTube::new(ClientContext::android()).unwrap();
    let items = browse::playlist(&it, "PLbpi6ZahtOH6Blw3RGYpWkSByi_T7Rygb")
        .await
        .unwrap();
    assert!(!items.is_empty());
    assert!(items.iter().all(|i| i.video_id.len() == 11));
}
