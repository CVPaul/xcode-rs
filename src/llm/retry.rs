/// Retry configuration and exponential-backoff helper for LLM HTTP calls.
///
/// # Design goals
///
/// - Comprehensive error coverage: 429, 500/502/503, timeouts, network drops
/// - Exponential back-off with ±25 % jitter so a burst of clients doesn't
///   thunderherd the same retry window
/// - Parse `Retry-After` header (seconds or HTTP-date) and respect it
/// - Fail fast on 400/422 (bad request / unprocessable) — retrying won't help
/// - Report each retry attempt to the user via `AgentIO::show_status()`
///
/// # Usage
///
/// ```rust,ignore
/// let config = RetryConfig::default();
/// let response = retry_with_backoff(&config, io.as_ref(), || async {
///     // one attempt at calling the API
///     make_one_request().await
/// }).await?;
/// ```
use crate::io::AgentIO;
use anyhow::{anyhow, Result};
use rand::Rng;
use serde::{Deserialize, Serialize};
use std::time::Duration;

// ─── RetryConfig ─────────────────────────────────────────────────────────────

/// Per-provider retry parameters, stored in `AgentConfig` and therefore in
/// the user's `~/.config/xcode/config.json` under `"agent"`.
///
/// All durations are **milliseconds** so the JSON is human-readable numbers.
///
/// Default values mirror sensible production settings:
/// ```json
/// {
///   "max_retries": 5,
///   "initial_delay_ms": 1000,
///   "max_delay_ms": 60000,
///   "backoff_multiplier": 2.0
/// }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetryConfig {
    /// Maximum number of retry attempts before giving up.
    /// Each attempt is a fresh HTTP request.
    #[serde(default = "RetryConfig::default_max_retries")]
    pub max_retries: u32,

    /// Delay before the *first* retry, in milliseconds.
    /// Subsequent delays are multiplied by `backoff_multiplier`.
    #[serde(default = "RetryConfig::default_initial_delay_ms")]
    pub initial_delay_ms: u64,

    /// Hard ceiling on the delay between retries, in milliseconds.
    /// Exponential growth is capped here regardless of how many retries happen.
    #[serde(default = "RetryConfig::default_max_delay_ms")]
    pub max_delay_ms: u64,

    /// Multiplier applied to the current delay after each retry.
    /// 2.0 = classic exponential back-off.
    #[serde(default = "RetryConfig::default_backoff_multiplier")]
    pub backoff_multiplier: f64,
}

impl RetryConfig {
    // Serde `default` helpers — each function returns the field's default
    // value.  Serde requires these to be free functions (or associated fns
    // that take no args), so we use associated functions here.
    fn default_max_retries() -> u32 {
        5
    }
    fn default_initial_delay_ms() -> u64 {
        1_000
    }
    fn default_max_delay_ms() -> u64 {
        60_000
    }
    fn default_backoff_multiplier() -> f64 {
        2.0
    }
}

impl Default for RetryConfig {
    fn default() -> Self {
        // Allow env vars to override defaults — useful in tests and CI.
        // XCODEAI_RETRY_MAX overrides max_retries (e.g. set to 0 to disable retries).
        // XCODEAI_RETRY_INITIAL_MS overrides the initial delay.
        let max_retries = std::env::var("XCODEAI_RETRY_MAX")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(Self::default_max_retries);
        let initial_delay_ms = std::env::var("XCODEAI_RETRY_INITIAL_MS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or_else(Self::default_initial_delay_ms);
        RetryConfig {
            max_retries,
            initial_delay_ms,
            max_delay_ms: Self::default_max_delay_ms(),
            backoff_multiplier: Self::default_backoff_multiplier(),
        }
    }
}

// ─── RetryDecision ────────────────────────────────────────────────────────────

/// What the retry logic should do when it encounters an error.
#[derive(Debug, PartialEq)]
pub enum RetryDecision {
    /// Retry after the given duration (may come from `Retry-After` header).
    RetryAfter(Duration),
    /// Do not retry — the error is permanent (e.g. 400 Bad Request).
    Fail,
}

// ─── classify_http_status ─────────────────────────────────────────────────────

/// Classify an HTTP status code into a retry decision.
///
/// | Status  | Decision            | Reason                                  |
/// |---------|---------------------|-----------------------------------------|
/// | 429     | RetryAfter(delay)   | Rate limited — we should back off       |
/// | 500     | RetryAfter(delay)   | Server error — transient, worth retrying|
/// | 502     | RetryAfter(delay)   | Bad gateway — upstream glitch           |
/// | 503     | RetryAfter(delay)   | Service unavailable — transient         |
/// | 504     | RetryAfter(delay)   | Gateway timeout                         |
/// | 400/422 | Fail                | Bad request — retrying won't help       |
/// | other   | Fail                | Unexpected — surface to the user        |
pub fn classify_http_status(status: u16, fallback_delay: Duration) -> RetryDecision {
    match status {
        // Transient errors that are worth retrying
        429 | 500 | 502 | 503 | 504 => RetryDecision::RetryAfter(fallback_delay),
        // Permanent client-side errors
        400 | 422 => RetryDecision::Fail,
        // Unknown — treat as permanent to avoid retrying unexpected situations
        _ => RetryDecision::Fail,
    }
}

// ─── parse_retry_after ────────────────────────────────────────────────────────

/// Parse an optional `Retry-After` header value into a `Duration`.
///
/// This function is a public utility for callers that want to extract the
/// server-provided retry delay from an HTTP response header. It is also
/// exercised by unit tests in this module.
///
/// Parse an optional `Retry-After` header value into a `Duration`.
///
/// The header can be:
/// - A plain integer: `"30"` → wait 30 seconds
/// - An HTTP-date: `"Wed, 01 Mar 2026 12:30:00 GMT"` → compute remaining seconds
///   (we only handle the integer form for now; HTTP-date falls back to `default`)
///
/// Returns `default` if the header is absent, unparseable, or is an HTTP-date
/// that we don't handle yet.
#[allow(dead_code)]
pub fn parse_retry_after(header: Option<&str>, default: Duration) -> Duration {
    match header {
        Some(value) => {
            // Try parsing as a plain integer (seconds)
            if let Ok(secs) = value.trim().parse::<u64>() {
                return Duration::from_secs(secs);
            }
            // HTTP-date format — not implemented yet, fall through to default
            default
        }
        None => default,
    }
}

// ─── jitter ───────────────────────────────────────────────────────────────────

/// Add ±25 % random jitter to a duration.
///
/// Jitter prevents a "thundering herd" of retrying clients all hitting the
/// server at exactly the same moment after a rate limit window expires.
///
/// Formula: delay * (0.75 + random(0..0.50))
/// ≡ delay * uniform(0.75, 1.25)
fn add_jitter(duration: Duration) -> Duration {
    let mut rng = rand::thread_rng();
    // Pick a multiplier in [0.75, 1.25]
    let multiplier = 0.75 + rng.gen::<f64>() * 0.50;
    let jittered_ms = (duration.as_millis() as f64 * multiplier) as u64;
    Duration::from_millis(jittered_ms)
}

// ─── next_delay ──────────────────────────────────────────────────────────────

/// Compute the next exponential back-off delay, clamped to `max_delay`.
///
/// `current_delay` is the delay used for the most recent retry.
/// Returns the new delay for the *next* retry.
pub fn next_delay(config: &RetryConfig, current_delay: Duration) -> Duration {
    let next_ms = (current_delay.as_millis() as f64 * config.backoff_multiplier) as u64;
    let capped_ms = next_ms.min(config.max_delay_ms);
    Duration::from_millis(capped_ms)
}

// ─── retry_with_backoff ───────────────────────────────────────────────────────

/// Run an async operation with exponential back-off retry on transient errors.
///
/// # Type parameters
///
/// - `T` — the success value type
/// - `F` — a `Fn() -> Fut` factory; called once per attempt (closures that
///   return a `Future` can only be awaited once, so we need a factory)
/// - `Fut` — the `Future` produced by `F`
///
/// # Arguments
///
/// - `config` — retry parameters (max attempts, delays, multiplier)
/// - `io` — I/O handle used to report retry status messages to the user
/// - `f` — factory function producing one `Future` per attempt
///
/// # Retry logic
///
/// 1. Call `f()` to get a `Future`, then `.await` it.
/// 2. If it succeeds → return `Ok(value)`.
/// 3. If it fails, inspect the error:
///    - Does it downcast to `RetryableError`?
///      - `RetryableError::Http { status, retry_after }` → use `classify_http_status`
///      - `RetryableError::Timeout` → always retry
///      - `RetryableError::Network(_)` → always retry
///    - If not a `RetryableError` → propagate immediately (unknown errors)
/// 4. If retries exhausted → return the last error.
///
/// # Status reporting
///
/// Each retry calls `io.show_status("⏳ Retrying in 5s (attempt 2/5) — 429 rate limit")`
/// so the user knows the agent hasn't frozen.
pub async fn retry_with_backoff<T, F, Fut>(
    config: &RetryConfig,
    io: &dyn AgentIO,
    f: F,
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    // Start with `initial_delay_ms`; grows by `backoff_multiplier` each retry.
    let mut current_delay = Duration::from_millis(config.initial_delay_ms);
    let mut last_err: anyhow::Error = anyhow!("No attempts made");

    for attempt in 0..=config.max_retries {
        match f().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                // Try to downcast to our retryable error type.
                // If the error is NOT a RetryableError, propagate immediately —
                // we don't know how to handle it.
                let retryable = err.downcast_ref::<RetryableError>();

                let retry_decision = match retryable {
                    Some(RetryableError::Http {
                        status,
                        retry_after,
                    }) => {
                        // Use the Retry-After header delay if the server gave us one,
                        // otherwise fall back to our computed exponential delay.
                        let server_delay = retry_after
                            .map(Duration::from_secs)
                            .unwrap_or(current_delay);
                        classify_http_status(*status, server_delay)
                    }
                    Some(RetryableError::Timeout) => {
                        // Timeouts are always worth retrying
                        RetryDecision::RetryAfter(current_delay)
                    }
                    Some(RetryableError::Network(_)) => {
                        // Network drops are always worth retrying
                        RetryDecision::RetryAfter(current_delay)
                    }
                    None => {
                        // Unknown error type — propagate immediately
                        return Err(err);
                    }
                };

                // If this is the last attempt, or the error is permanent, bail out.
                if attempt >= config.max_retries {
                    last_err = err;
                    break;
                }

                match retry_decision {
                    RetryDecision::Fail => {
                        // Permanent error — don't retry
                        return Err(err);
                    }
                    RetryDecision::RetryAfter(wait) => {
                        // Apply jitter so multiple clients don't retry in lockstep
                        let jittered = add_jitter(wait);

                        // Build a human-readable reason from the error
                        let reason = match retryable {
                            Some(RetryableError::Http { status, .. }) => match status {
                                429 => "rate limit",
                                500 => "server error",
                                502 => "bad gateway",
                                503 => "service unavailable",
                                504 => "gateway timeout",
                                _ => "HTTP error",
                            },
                            Some(RetryableError::Timeout) => "timeout",
                            Some(RetryableError::Network(_)) => "network error",
                            None => "error",
                        };

                        // Inform the user so they know we haven't frozen
                        io.show_status(&format!(
                            "⏳ Retrying in {:.1}s (attempt {}/{}) — {reason}",
                            jittered.as_secs_f64(),
                            attempt + 1,
                            config.max_retries,
                        ))
                        .await
                        .ok(); // .ok() — retry status is best-effort, ignore errors

                        tokio::time::sleep(jittered).await;

                        // Advance the exponential back-off for the next attempt
                        current_delay = next_delay(config, current_delay);
                        last_err = err;
                    }
                }
            }
        }
    }

    Err(last_err)
}

// ─── RetryableError ──────────────────────────────────────────────────────────

/// Marker error type that wraps transient failures so `retry_with_backoff`
/// can distinguish them from permanent errors (like `ParseError`).
///
/// # Usage in openai.rs
///
/// Inside the SSE event loop, when we encounter a retryable condition,
/// we return `Err(RetryableError::Http { ... }.into())`.  `anyhow::Error`
/// carries the concrete type, and `retry_with_backoff` downcasts to check.
#[derive(thiserror::Error, Debug)]
pub enum RetryableError {
    /// An HTTP error with an optional `Retry-After` hint (seconds).
    #[error("HTTP {status} error")]
    Http {
        status: u16,
        /// Parsed from `Retry-After` header, if present.
        retry_after: Option<u64>,
    },
    /// The request timed out before receiving a complete response.
    #[error("Request timed out")]
    Timeout,
    /// A network-level error (connection refused, DNS failure, etc.)
    #[error("Network error: {0}")]
    Network(String),
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::NullIO;
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    // ── classify_http_status ────────────────────────────────────────────────

    #[test]
    fn test_classify_retryable_codes() {
        let delay = Duration::from_secs(1);
        // All these should trigger a retry
        for code in [429u16, 500, 502, 503, 504] {
            assert_eq!(
                classify_http_status(code, delay),
                RetryDecision::RetryAfter(delay),
                "expected RetryAfter for {code}"
            );
        }
    }

    #[test]
    fn test_classify_permanent_codes() {
        let delay = Duration::from_secs(1);
        // These should fail immediately
        for code in [400u16, 422, 401, 403, 404] {
            assert_eq!(
                classify_http_status(code, delay),
                RetryDecision::Fail,
                "expected Fail for {code}"
            );
        }
    }

    // ── parse_retry_after ───────────────────────────────────────────────────

    #[test]
    fn test_parse_retry_after_integer() {
        let result = parse_retry_after(Some("30"), Duration::from_secs(5));
        assert_eq!(result, Duration::from_secs(30));
    }

    #[test]
    fn test_parse_retry_after_with_whitespace() {
        let result = parse_retry_after(Some("  15  "), Duration::from_secs(5));
        assert_eq!(result, Duration::from_secs(15));
    }

    #[test]
    fn test_parse_retry_after_none_returns_default() {
        let default = Duration::from_secs(5);
        let result = parse_retry_after(None, default);
        assert_eq!(result, default);
    }

    #[test]
    fn test_parse_retry_after_invalid_falls_back_to_default() {
        // HTTP-date format — not implemented, should fall back
        let default = Duration::from_secs(5);
        let result = parse_retry_after(Some("Wed, 01 Mar 2026 12:30:00 GMT"), default);
        assert_eq!(result, default);
    }

    // ── next_delay ──────────────────────────────────────────────────────────

    #[test]
    fn test_next_delay_doubles_by_default() {
        let config = RetryConfig::default();
        let d = Duration::from_millis(1000);
        let next = next_delay(&config, d);
        assert_eq!(next, Duration::from_millis(2000));
    }

    #[test]
    fn test_next_delay_caps_at_max() {
        let config = RetryConfig {
            max_delay_ms: 3000,
            ..RetryConfig::default()
        };
        let d = Duration::from_millis(2000);
        let next = next_delay(&config, d);
        // 2000 * 2.0 = 4000, capped at 3000
        assert_eq!(next, Duration::from_millis(3000));
    }

    // ── retry_with_backoff ──────────────────────────────────────────────────

    /// Succeeds on the first attempt — no retries needed.
    #[tokio::test]
    async fn test_no_retry_on_success() {
        let config = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 1, // tiny delay so the test is fast
            ..RetryConfig::default()
        };
        let io = NullIO;
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry_with_backoff(&config, &io, || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Ok::<&str, anyhow::Error>("ok")
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    /// Fails with a 429 twice, then succeeds on the third attempt.
    #[tokio::test]
    async fn test_retry_on_429_then_success() {
        let config = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 1,
            max_delay_ms: 10,
            backoff_multiplier: 2.0,
        };
        let io = NullIO;
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry_with_backoff(&config, &io, || {
            let cc = cc.clone();
            async move {
                let n = cc.fetch_add(1, Ordering::SeqCst);
                if n < 2 {
                    // First two calls return 429
                    Err(anyhow::Error::new(RetryableError::Http {
                        status: 429,
                        retry_after: None,
                    }))
                } else {
                    // Third call succeeds
                    Ok("done")
                }
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(result.unwrap(), "done");
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    /// Fails permanently with 400 — should not retry.
    #[tokio::test]
    async fn test_no_retry_on_400() {
        let config = RetryConfig {
            max_retries: 3,
            initial_delay_ms: 1,
            ..RetryConfig::default()
        };
        let io = NullIO;
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry_with_backoff(&config, &io, || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Err::<&str, _>(anyhow::Error::new(RetryableError::Http {
                    status: 400,
                    retry_after: None,
                }))
            }
        })
        .await;

        assert!(result.is_err());
        // Should have been called exactly once (no retries on 400)
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    /// Exhausts all retries — should return the last error.
    #[tokio::test]
    async fn test_exhausts_retries() {
        let config = RetryConfig {
            max_retries: 2,
            initial_delay_ms: 1,
            max_delay_ms: 10,
            backoff_multiplier: 2.0,
        };
        let io = NullIO;
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry_with_backoff(&config, &io, || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                Err::<&str, _>(anyhow::Error::new(RetryableError::Http {
                    status: 503,
                    retry_after: None,
                }))
            }
        })
        .await;

        assert!(result.is_err());
        // initial attempt + 2 retries = 3 total calls
        assert_eq!(call_count.load(Ordering::SeqCst), 3);
    }

    /// Non-retryable error (not a RetryableError) propagates immediately.
    #[tokio::test]
    async fn test_non_retryable_error_propagates_immediately() {
        let config = RetryConfig {
            max_retries: 5,
            initial_delay_ms: 1,
            ..RetryConfig::default()
        };
        let io = NullIO;
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry_with_backoff(&config, &io, || {
            let cc = cc.clone();
            async move {
                cc.fetch_add(1, Ordering::SeqCst);
                // Return a plain anyhow error (not RetryableError)
                Err::<&str, _>(anyhow::anyhow!("Some parse error"))
            }
        })
        .await;

        assert!(result.is_err());
        // Only 1 call — propagated immediately without retrying
        assert_eq!(call_count.load(Ordering::SeqCst), 1);
    }

    /// Retry-After header is respected over the computed delay.
    #[tokio::test]
    async fn test_retry_after_header_respected() {
        // 1-second Retry-After from "server"
        let wait = parse_retry_after(Some("1"), Duration::from_secs(60));
        assert_eq!(wait, Duration::from_secs(1));
        // ...and not the 60-second fallback
        assert_ne!(wait, Duration::from_secs(60));
    }

    /// Network error triggers a retry.
    #[tokio::test]
    async fn test_retry_on_network_error() {
        let config = RetryConfig {
            max_retries: 2,
            initial_delay_ms: 1,
            max_delay_ms: 5,
            backoff_multiplier: 1.5,
        };
        let io = NullIO;
        let call_count = Arc::new(AtomicU32::new(0));
        let cc = call_count.clone();

        let result = retry_with_backoff(&config, &io, || {
            let cc = cc.clone();
            async move {
                let n = cc.fetch_add(1, Ordering::SeqCst);
                if n < 1 {
                    Err(anyhow::Error::new(RetryableError::Network(
                        "connection refused".into(),
                    )))
                } else {
                    Ok("recovered")
                }
            }
        })
        .await;

        assert!(result.is_ok());
        assert_eq!(call_count.load(Ordering::SeqCst), 2);
    }
}
