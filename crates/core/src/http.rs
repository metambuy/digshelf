//! Minimal HTTP abstraction so tests can replay recorded fixtures instead of
//! touching the network.

use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

use crate::error::{Error, Result};

/// A raw HTTP response: status code and body text.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub body: String,
}

/// Performs GET requests. Implemented by [`ReqwestTransport`] for real use
/// and by fixture-backed stubs in tests.
pub trait Transport {
    fn get(&self, url: &str) -> impl Future<Output = Result<Response>>;
}

/// Default spacing between requests. Deezer allows 50 requests per 5 seconds;
/// 120 ms keeps us at ~8 req/s, well under the quota.
pub const DEFAULT_MIN_INTERVAL: Duration = Duration::from_millis(120);

/// reqwest-based transport (rustls) with a simple client-side throttle.
pub struct ReqwestTransport {
    client: reqwest::Client,
    min_interval: Duration,
    next_slot: Mutex<Option<Instant>>,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!(
                "digshelf/",
                env!("CARGO_PKG_VERSION"),
                " (+https://github.com/metambuy/digshelf)"
            ))
            .timeout(Duration::from_secs(30))
            .build()
            .map_err(|e| Error::Http {
                url: String::new(),
                message: e.to_string(),
            })?;
        Ok(Self {
            client,
            min_interval: DEFAULT_MIN_INTERVAL,
            next_slot: Mutex::new(None),
        })
    }

    /// Reserve the next request slot and return how long to wait for it.
    fn reserve_slot(&self) -> Duration {
        let now = Instant::now();
        let mut next = self.next_slot.lock().unwrap_or_else(|e| e.into_inner());
        let slot = next.map_or(now, |n| n.max(now));
        *next = Some(slot + self.min_interval);
        slot - now
    }
}

impl Transport for ReqwestTransport {
    async fn get(&self, url: &str) -> Result<Response> {
        let wait = self.reserve_slot();
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
        let http_err = |e: reqwest::Error| Error::Http {
            url: url.to_string(),
            message: e.to_string(),
        };
        let resp = self.client.get(url).send().await.map_err(http_err)?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(http_err)?;
        Ok(Response { status, body })
    }
}
