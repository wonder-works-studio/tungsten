use anyhow::{Context, Result, bail};
use reqwest::{Client, StatusCode, multipart};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::{Mutex, Notify};
use tokio::time::Instant;

use crate::api::sync::roblox::*;
use crate::core::assets::asset::AssetKind;
use crate::log;

pub struct RobloxClient {
    client: Client,
    api_key: String,
    /// The earliest point in time we're allowed to send another request.
    rate_limit_reset: Mutex<Option<Instant>>,
    /// Set after a reply that every later request would get too (a rejected
    /// API key or missing permission), so in-flight tasks bail out instead of
    /// hammering the API further.
    fatally_failed: AtomicBool,
    /// Wakes in-flight sleeps (poll backoff, retry waits) when a fatal
    /// failure is recorded.
    cancel: Notify,
}

/// Everything the uploader needs to know about a single asset.
pub struct UploadParams {
    /// File name used in the multipart form.
    pub file_name: String,
    /// Display name sent in the JSON body.
    pub display_name: String,
    /// Description sent in the JSON body.
    pub description: String,
    /// Raw bytes of the (possibly converted) asset.
    pub data: Vec<u8>,
    /// Asset kind — determines API type string and MIME type.
    pub kind: AssetKind,
    /// Creator to upload under.
    pub creator: Creator,
    /// Optional override for the asset type string sent to the API.
    /// If set, this overrides the value derived from `kind`.
    pub asset_type_override: Option<String>,
}

/// What to do with a reply that wasn't 200 OK.
#[derive(Debug, PartialEq, Eq)]
enum Verdict {
    /// Transient: try the same request again after a backoff.
    Retry,
    /// Every request would get this reply; stop the whole run.
    Fatal,
    /// This request failed; other assets carry on.
    Fail,
}

fn classify(status: StatusCode, content_type: &str, body: &str) -> Verdict {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Verdict::Fatal,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::REQUEST_TIMEOUT => Verdict::Retry,
        s if s.is_server_error() => Verdict::Retry,
        // Roblox gave up reading an upload body that arrived too slowly.
        StatusCode::BAD_REQUEST if body.contains("MinRequestBodyDataRate") => Verdict::Retry,
        // A bare HTML 400 ("Your browser sent an invalid request.") comes from
        // the edge in front of the Assets API when uploads arrive back to
        // back; the request never reached Roblox, so nothing was created.
        StatusCode::BAD_REQUEST if !content_type.contains("json") => Verdict::Retry,
        _ => Verdict::Fail,
    }
}

impl RobloxClient {
    pub fn new(api_key: String) -> Self {
        // HTTP/1.1 gives every concurrent upload its own connection. Over
        // HTTP/2 they share one, and a large body stalled behind the others
        // gets dropped by Roblox (400 MinRequestBodyDataRate).
        let client = Client::builder()
            .http1_only()
            .pool_max_idle_per_host(8)
            .build()
            .expect("failed to build reqwest client");

        RobloxClient {
            client,
            api_key,
            rate_limit_reset: Mutex::new(None),
            fatally_failed: AtomicBool::new(false),
            cancel: Notify::new(),
        }
    }

    /// Upload an asset and return its Roblox asset ID.
    pub async fn upload(&self, params: UploadParams) -> Result<u64> {
        let request_json = serde_json::to_string(&UploadRequest {
            asset_type: params
                .asset_type_override
                .clone()
                .unwrap_or_else(|| params.kind.api_type().to_string()),
            display_name: params.display_name.clone(),
            description: params.description.clone(),
            creation_context: CreationContext {
                creator: params.creator,
            },
        })
        .context("Failed to serialize upload request")?;

        let mime = params.kind.mime();

        log!(
            debug,
            "Uploading {} ({} bytes, {})",
            params.display_name,
            params.data.len(),
            mime
        );

        let response = self
            .send_with_retry(|client| {
                let form = multipart::Form::new()
                    .text("request", request_json.clone())
                    .part(
                        "fileContent",
                        multipart::Part::bytes(params.data.clone())
                            .file_name(params.file_name.clone())
                            .mime_str(mime)
                            .unwrap(),
                    );

                client
                    .post("https://apis.roblox.com/assets/v1/assets")
                    .header("x-api-key", &self.api_key)
                    .multipart(form)
            })
            .await?;

        let operation: Operation = parse_json_response(response, "upload response").await?;

        log!(
            debug,
            "Upload accepted for {}, operation {}",
            params.display_name,
            operation.operation_id
        );

        let asset_id = self.poll_operation(&operation.operation_id).await?;
        log!(
            debug,
            "Uploaded {} -> asset {}",
            params.display_name,
            asset_id
        );
        Ok(asset_id)
    }

    async fn poll_operation(&self, operation_id: &str) -> Result<u64> {
        // Backoff doubles up to a cap so a finished operation (audio
        // moderation often completes in 10-60s) is noticed within
        // MAX_POLL_DELAY instead of waiting out an ever-growing sleep.
        // The overall window stays roughly 17 minutes.
        const MAX_POLLS: u32 = 132;
        const MAX_POLL_DELAY: Duration = Duration::from_secs(8);
        let mut delay = Duration::from_secs(1);

        for attempt in 0..MAX_POLLS {
            log!(
                debug,
                "Polling operation {} (attempt {}/{})",
                operation_id,
                attempt + 1,
                MAX_POLLS
            );
            self.sleep_or_cancel(delay).await;
            self.bail_if_cancelled()?;

            let response = self
                .send_with_retry(|client| {
                    client
                        .get(format!(
                            "https://apis.roblox.com/assets/v1/operations/{}",
                            operation_id
                        ))
                        .header("x-api-key", &self.api_key)
                })
                .await?;

            let operation: Operation = parse_json_response(response, "operation response").await?;

            if operation.done {
                return match operation.response {
                    Some(result) => Ok(result
                        .asset_id
                        .parse()
                        .context("Failed to parse asset ID")?),
                    None => bail!(
                        "Operation completed but no asset ID was returned\n  \
                         Hint: This is likely a Roblox API issue, try again"
                    ),
                };
            }

            delay = (delay * 2).min(MAX_POLL_DELAY);
        }

        bail!(
            "Upload timed out after {} poll attempts\n  \
             Hint: The asset may still be processing, check your Roblox inventory",
            MAX_POLLS
        )
    }

    async fn send_with_retry<F>(&self, make_req: F) -> Result<reqwest::Response>
    where
        F: Fn(&Client) -> reqwest::RequestBuilder,
    {
        const MAX_RETRIES: u8 = 5;
        let mut attempt: u8 = 0;

        loop {
            self.bail_if_cancelled()?;
            self.wait_for_rate_limit().await;
            self.bail_if_cancelled()?;

            let response = match make_req(&self.client).send().await {
                Ok(r) => r,
                Err(e) => {
                    // Timeouts and refused/reset connections are transient.
                    if attempt >= MAX_RETRIES || (!e.is_timeout() && !e.is_connect()) {
                        return Err(e).context("Failed to send request");
                    }
                    let wait = jitter(backoff(attempt));
                    log!(
                        warn,
                        "Network error ({}), retrying in {:.2}s",
                        e,
                        wait.as_secs_f64()
                    );
                    self.sleep_or_cancel(wait).await;
                    attempt += 1;
                    continue;
                }
            };

            let status = response.status();
            log!(debug, "API request returned {}", status);
            if status == StatusCode::OK {
                return Ok(response);
            }

            if status == StatusCode::TOO_MANY_REQUESTS && attempt < MAX_RETRIES {
                let wait = rate_limit_wait(&response, attempt);
                log!(warn, "Rate limited, retrying in {:.2}s", wait.as_secs_f64());
                *self.rate_limit_reset.lock().await = Some(Instant::now() + wait);
                self.sleep_or_cancel(wait).await;
                attempt += 1;
                continue;
            }

            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            let body = response.text().await.unwrap_or_default();
            let shown_body: String = body.chars().take(500).collect();

            match classify(status, &content_type, &body) {
                Verdict::Retry if attempt < MAX_RETRIES => {
                    let wait = jitter(backoff(attempt));
                    log!(
                        warn,
                        "Request failed with status {} ({}), retrying in {:.2}s",
                        status,
                        shown_body.trim(),
                        wait.as_secs_f64()
                    );
                    self.sleep_or_cancel(wait).await;
                    attempt += 1;
                }
                Verdict::Fatal => {
                    self.fatally_failed.store(true, Ordering::Release);
                    self.cancel.notify_waiters();
                    bail!(
                        "Request failed with status {}\n  Response: {}\n  \
                         Hint: Check your API key, its permissions and the creator ID",
                        status,
                        shown_body
                    );
                }
                Verdict::Retry | Verdict::Fail => {
                    bail!(
                        "Request failed with status {}\n  Response: {}",
                        status,
                        shown_body
                    );
                }
            }
        }
    }

    fn bail_if_cancelled(&self) -> Result<()> {
        if self.fatally_failed.load(Ordering::Acquire) {
            bail!("A previous request failed fatally, aborting");
        }
        Ok(())
    }

    /// Sleep until any active rate-limit window has passed.
    async fn wait_for_rate_limit(&self) {
        let reset = *self.rate_limit_reset.lock().await;
        if let Some(reset_at) = reset {
            let now = Instant::now();
            if reset_at > now {
                self.sleep_or_cancel(reset_at - now).await;
            }
        }
    }

    /// Sleep, but wake early when a fatal failure cancels the run. Callers
    /// re-check `fatally_failed` afterwards.
    async fn sleep_or_cancel(&self, duration: Duration) {
        tokio::select! {
            _ = tokio::time::sleep(duration) => {}
            _ = self.cancel.notified() => {}
        }
    }
}

/// Exponential backoff for a retry attempt: 1s, 2s, 4s, 8s, 16s.
fn backoff(attempt: u8) -> Duration {
    Duration::from_secs(1u64 << attempt)
}

/// Wait duration for a 429, honoring the `x-ratelimit-reset` header when
/// present, falling back to exponential backoff.
fn rate_limit_wait(response: &reqwest::Response, attempt: u8) -> Duration {
    let base = response
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok())
        .map(Duration::from_secs)
        .unwrap_or_else(|| backoff(attempt));
    jitter(base)
}

/// Apply ±10% jitter so concurrent tasks don't all retry at the same
/// millisecond. Seeded from the clock — good enough to stagger retries.
fn jitter(d: Duration) -> Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|t| t.subsec_nanos())
        .unwrap_or(0);
    let spread = nanos % 200;
    let factor = 0.9 + (spread as f64) / 1000.0;
    d.mul_f64(factor)
}

/// Parse the response body as JSON, with a clear error when Roblox returned
/// something else (an HTML error page from the edge) instead of failing
/// cryptically inside serde.
async fn parse_json_response<T: serde::de::DeserializeOwned>(
    response: reqwest::Response,
    what: &str,
) -> Result<T> {
    let status = response.status();
    let content_type = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("")
        .to_ascii_lowercase();

    if !content_type.contains("json") {
        let body = response.text().await.unwrap_or_default();
        bail!(
            "Expected JSON from Roblox for {what} but got content-type \"{}\" (status {})\n  Response: {}",
            content_type,
            status,
            body.chars().take(500).collect::<String>()
        );
    }

    response
        .json()
        .await
        .with_context(|| format!("Failed to parse {what}"))
}

// Tests

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_doubles_per_attempt() {
        assert_eq!(backoff(0), Duration::from_secs(1));
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(4), Duration::from_secs(16));
    }

    #[test]
    fn jitter_stays_within_ten_percent() {
        for _ in 0..200 {
            let ms = jitter(Duration::from_secs(10)).as_millis();
            assert!((9000..=11000).contains(&ms), "{}ms", ms);
        }
    }

    #[test]
    fn classify_retries_transient_replies() {
        let json = "application/json";
        assert_eq!(classify(StatusCode::BAD_GATEWAY, json, ""), Verdict::Retry);
        assert_eq!(
            classify(StatusCode::SERVICE_UNAVAILABLE, "text/html", ""),
            Verdict::Retry
        );
        assert_eq!(
            classify(
                StatusCode::BAD_REQUEST,
                "application/problem+json; charset=utf-8",
                "{\"title\":\"Failed to read the request form. Reading the request body timed out due to data arriving too slowly. See MinRequestBodyDataRate.\"}"
            ),
            Verdict::Retry
        );
        assert_eq!(
            classify(
                StatusCode::BAD_REQUEST,
                "text/html",
                "<html><body>Your browser sent an invalid request.</body></html>"
            ),
            Verdict::Retry
        );
    }

    #[test]
    fn classify_stops_the_run_only_for_auth_failures() {
        let json = "application/json";
        assert_eq!(classify(StatusCode::UNAUTHORIZED, json, ""), Verdict::Fatal);
        assert_eq!(classify(StatusCode::FORBIDDEN, json, ""), Verdict::Fatal);
        assert_eq!(
            classify(
                StatusCode::BAD_REQUEST,
                json,
                "{\"message\":\"Invalid asset\"}"
            ),
            Verdict::Fail
        );
        assert_eq!(classify(StatusCode::NOT_FOUND, json, ""), Verdict::Fail);
    }
}
