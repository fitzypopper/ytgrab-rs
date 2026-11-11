//! InnerTube HTTP client.
//!
//! InnerTube is YouTube's internal JSON API that the web player and the
//! mobile apps talk to. We speak it directly so we never have to scrape
//! HTML or run a browser.
//!
//! The most interesting trick: each "client context" (android, ios, web, ...)
//! unlocks a different set of features and rate limits. The ANDROID client in
//! particular hands back *direct* `googlevideo.com` URLs with no signature
//! cipher, which removes the single most painful part of a YouTube downloader
//! (running the player's JS to reverse the `s=` parameter).

use serde_json::{json, Value};

use crate::error::{Error, Result};

/// Numeric client ids used by the `X-Youtube-Client-Name` header.
const CLIENT_ID_ANDROID: u32 = 3;
const CLIENT_ID_IOS: u32 = 5;
const CLIENT_ID_MWEB: u32 = 2;
const CLIENT_ID_WEB: u32 = 1;
const CLIENT_ID_TV: u32 = 7;

#[derive(Debug, Clone)]
pub struct ClientContext {
    pub name: &'static str,
    pub version: &'static str,
    pub user_agent: &'static str,
    pub api_key: &'static str,
    pub numeric_id: u32,
    /// Extra fields merged into the `client` object of the request context.
    pub extra: Value,
    /// Visitor id, sent as the `X-Goog-Visitor-Id` header.
    pub visitor_data: Option<String>,
    /// The TV client does not want the API key in the query string.
    pub use_api_key: bool,
    /// The player signature timestamp (`STS`).
    pub signature_timestamp: Option<i64>,
}

impl ClientContext {
    /// ANDROID: returns unciphered URLs, no PO token, no bot checks in
    /// practice. The workhorse.
    pub fn android() -> Self {
        Self {
            name: "ANDROID",
            version: "20.10.38",
            user_agent: "com.google.android.youtube/20.10.38 (Linux; U; Android 14) gzip",
            api_key: "AIzaSyA8eiZmM1FaDVjRy-df2KTyQ_vz_yYM39w",
            numeric_id: CLIENT_ID_ANDROID,
            visitor_data: None,
            use_api_key: true,
            signature_timestamp: None,
            extra: json!({
                "androidSdkVersion": 34,
                "osName": "Android",
                "osVersion": "14",
            }),
        }
    }

    pub fn ios() -> Self {
        Self {
            name: "IOS",
            version: "20.10.4",
            user_agent: "com.google.ios.youtube/20.10.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)",
            api_key: "AIzaSyB-63vPrdThhKuerbB2N_l7Kwwcxj6yUAc",
            numeric_id: CLIENT_ID_IOS,
            visitor_data: None,
            use_api_key: true,
            signature_timestamp: None,
            extra: json!({
                "deviceMake": "Apple",
                "deviceModel": "iPhone16,2",
                "osName": "iPhone",
                "osVersion": "18.3.2.22D82",
            }),
        }
    }

    pub fn mweb() -> Self {
        Self {
            name: "MWEB",
            version: "2.20250101.00.00",
            user_agent: "Mozilla/5.0 (iPhone; CPU iPhone OS 17_5 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.5 Mobile/15E148 Safari/604.1",
            api_key: "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8",
            numeric_id: CLIENT_ID_MWEB,
            visitor_data: None,
            use_api_key: true,
            signature_timestamp: None,
            extra: json!({}),
        }
    }

    pub fn web() -> Self {
        Self {
            name: "WEB",
            version: "2.20250101.00.00",
            user_agent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/132.0.0.0 Safari/537.36",
            api_key: "AIzaSyAO_FJ2SlqU8Q4STEHLGCilw_Y9_11qcW8",
            numeric_id: CLIENT_ID_WEB,
            visitor_data: None,
            use_api_key: true,
            signature_timestamp: None,
            extra: json!({}),
        }
    }

    /// The Samsung TV app. Its URLs carry a throttling challenge that has to
    /// be solved with the player's JavaScript, but in exchange it serves the
    /// high quality formats that the Android client refuses.
    pub fn tv_samsung() -> Self {
        Self {
            name: "TVHTML5",
            version: "5.20260707",
            user_agent: "Mozilla/5.0 (SMART-TV; Linux; Tizen 2.4.0) AppleWebKit/538.1 (KHTML, like Gecko) Version/2.4.0 TV Safari/538.1",
            api_key: "",
            numeric_id: CLIENT_ID_TV,
            extra: json!({
                "deviceMake": "Samsung",
                "deviceModel": "UKS9800",
                "osName": "Tizen",
                "osVersion": "2.4.0",
            }),
            visitor_data: None,
            use_api_key: false,
            signature_timestamp: None,
        }
    }

    pub fn tv() -> Self {
        Self {
            name: "TVHTML5_SIMPLY_EMBEDDED_PLAYER",
            version: "2.0",
            user_agent: "Mozilla/5.0 (PlayStation; PlayStation 4/12.00) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/16.0 Safari/605.1.15",
            api_key: "AIzaSyDCU8hByM-4DrUqRUYnGn-3llEO78bcxq8",
            numeric_id: CLIENT_ID_TV,
            visitor_data: None,
            use_api_key: true,
            signature_timestamp: None,
            extra: json!({}),
        }
    }

    pub fn by_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "android" => Some(Self::android()),
            "ios" => Some(Self::ios()),
            "mweb" => Some(Self::mweb()),
            "web" => Some(Self::web()),
            "tv" | "tvhtml5" => Some(Self::tv()),
            _ => None,
        }
    }

    fn client_object(&self) -> Value {
        let mut obj = json!({
            "clientName": self.name,
            "clientVersion": self.version,
            "hl": "en",
            "gl": "US",
            "timeZone": "UTC",
            "utcOffsetMinutes": 0,
        });
        if let (Some(dst), Some(src)) = (obj.as_object_mut(), self.extra.as_object()) {
            for (k, v) in src {
                dst.insert(k.clone(), v.clone());
            }
        }
        obj
    }
}

#[derive(Debug, Clone)]
pub struct InnerTube {
    http: reqwest::Client,
    ctx: ClientContext,
}

impl InnerTube {
    pub fn new(ctx: ClientContext) -> Result<Self> {
        Self::with_cookie_jar(ctx, None)
    }

    /// Build a client that sends the given cookies with every request.
    pub fn with_cookie_jar(
        ctx: ClientContext,
        jar: Option<std::sync::Arc<reqwest::cookie::Jar>>,
    ) -> Result<Self> {
        let mut builder = reqwest::Client::builder()
            .user_agent(ctx.user_agent)
            .timeout(std::time::Duration::from_secs(30));
        if let Some(jar) = jar {
            builder = builder.cookie_provider(jar);
        }
        let http = builder.build()?;
        Ok(Self { http, ctx })
    }

    pub fn context(&self) -> &ClientContext {
        &self.ctx
    }

    pub fn user_agent(&self) -> &str {
        self.ctx.user_agent
    }

    /// POST a request to an InnerTube endpoint, returning the raw JSON.
    pub async fn post(&self, endpoint: &str, body: Value) -> Result<Value> {
        let url = if self.ctx.use_api_key && !self.ctx.api_key.is_empty() {
            format!(
                "https://www.youtube.com/youtubei/v1/{}?key={}&prettyPrint=false",
                endpoint, self.ctx.api_key
            )
        } else {
            format!("https://www.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false")
        };
        if std::env::var("YTDBG").is_ok() {
            eprintln!("DEBUG url={url}");
            eprintln!("DEBUG body={body}");
        }
        let mut req = self
            .http
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Origin", "https://www.youtube.com")
            .header("X-Youtube-Client-Name", self.ctx.numeric_id.to_string())
            .header("X-Youtube-Client-Version", self.ctx.version)
            .header("Accept-Language", "en-US,en;q=0.9");
        if let Some(visitor) = &self.ctx.visitor_data {
            req = req.header("X-Goog-Visitor-Id", visitor);
        }
        let resp = req
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;
        if !status.is_success() {
            return Err(Error::other(format!(
                "innertube {endpoint} returned HTTP {status}: {}",
                text.chars().take(300).collect::<String>()
            )));
        }
        serde_json::from_str(&text).map_err(Error::Json)
    }

    /// The `context` object every InnerTube request body carries.
    pub async fn context_object(&self) -> Value {
        json!({
            "context": {
                "client": self.ctx.client_object(),
            }
        })
    }
}
