//! GitHub Copilot OAuth device-code authentication and token management.
//!
//! Flow:
//!   1. POST /login/device/code  → device_code + user_code + verification_uri
//!   2. Show user_code, ask user to visit verification_uri in browser
//!   3. Poll POST /login/oauth/access_token until `access_token` arrives
//!   4. GET /copilot_internal/v2/token  → short-lived Copilot API token (TTL ~25 min)
//!   5. Before every LLM call: if token is expired, repeat step 4 only.
//!
//! The OAuth access_token (step 3) is persisted to
//! ~/.config/xcode/copilot_auth.json so the user only has to do device-code
//! once per machine.

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

/// GitHub's public client-id for Copilot extensions.
/// This is the same identifier used by neovim/copilot.vim, VS Code, etc.
pub const COPILOT_CLIENT_ID: &str = "Iv1.b507a08c87ecfe98";

// User-agent string accepted by the GitHub Copilot API.
const USER_AGENT: &str = "GithubCopilot/1.155.0";

// ─── Persistent OAuth token (stored on disk) ──────────────────────────────────

/// Persisted after device-code flow. Only needs to be refreshed via device-code
/// flow if revoked by the user; otherwise `access_token` is long-lived.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CopilotOAuthToken {
    /// Long-lived GitHub OAuth access token (prefix `gho_`).
    pub access_token: String,
    /// Token type, usually "bearer".
    pub token_type: String,
}

impl CopilotOAuthToken {
    /// Default path: ~/.config/xcode/copilot_auth.json
    pub fn default_path() -> Result<PathBuf> {
        let base = dirs::config_dir().context("Could not determine config directory")?;
        Ok(base.join("xcode").join("copilot_auth.json"))
    }

    pub fn load() -> Result<Option<Self>> {
        let path = Self::default_path()?;
        if !path.exists() {
            return Ok(None);
        }
        let content =
            std::fs::read_to_string(&path).with_context(|| format!("Failed to read {:?}", path))?;
        let token: Self =
            serde_json::from_str(&content).context("Failed to parse copilot_auth.json")?;
        Ok(Some(token))
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::default_path()?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(&path, json).with_context(|| format!("Failed to write {:?}", path))?;
        Ok(())
    }

    pub fn delete() -> Result<()> {
        let path = Self::default_path()?;
        if path.exists() {
            std::fs::remove_file(&path)?;
        }
        Ok(())
    }
}

// ─── Short-lived Copilot API token (in-memory, refreshed automatically) ───────

/// Short-lived Copilot token returned by the GitHub internal token endpoint.
/// Expires in ~25 minutes; we refresh automatically before every LLM call.
#[derive(Debug, Clone)]
pub struct CopilotApiToken {
    /// The `Bearer` token to use with api.githubcopilot.com
    pub token: String,
    /// Unix timestamp (seconds) at which this token expires.
    pub expires_at: u64,
}

impl CopilotApiToken {
    /// Returns true if the token has expired (or expires within 60 seconds).
    pub fn is_expired(&self) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        now + 60 >= self.expires_at
    }
}

// ─── Device-code flow ─────────────────────────────────────────────────────────

#[derive(Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    expires_in: u64,
    interval: u64,
}

#[derive(Deserialize)]
struct AccessTokenResponse {
    access_token: Option<String>,
    token_type: Option<String>,
    error: Option<String>,
}

/// Runs the full GitHub device-code OAuth flow interactively.
/// Prints the user_code and verification URL to stdout, then polls until the
/// user completes authorization.
///
/// Returns a `CopilotOAuthToken` which should be persisted with `.save()`.
pub async fn device_code_flow(client: &reqwest::Client) -> Result<CopilotOAuthToken> {
    // Step 1: request device code
    let resp: DeviceCodeResponse = client
        .post("https://github.com/login/device/code")
        .header("Accept", "application/json")
        .header("User-Agent", USER_AGENT)
        .json(&serde_json::json!({
            "client_id": COPILOT_CLIENT_ID,
            "scope": "read:user"
        }))
        .send()
        .await
        .context("Failed to request device code")?
        .json()
        .await
        .context("Failed to parse device code response")?;

    // Step 2: show instructions to user
    println!("\n┌─────────────────────────────────────────────────────────┐");
    println!("│  GitHub Copilot — Device Authorization                  │");
    println!("├─────────────────────────────────────────────────────────┤");
    println!("│                                                         │");
    println!("│  1. Visit: {:<47}│", resp.verification_uri);
    println!("│  2. Enter code:  {:<40}│", resp.user_code);
    println!("│                                                         │");
    println!(
        "│  Waiting for authorization... (expires in {}s)      │",
        resp.expires_in
    );
    println!("└─────────────────────────────────────────────────────────┘\n");

    // Step 3: poll for access token
    let poll_interval = Duration::from_secs(resp.interval.max(5));
    let deadline = SystemTime::now() + Duration::from_secs(resp.expires_in);

    loop {
        if SystemTime::now() > deadline {
            bail!("Device authorization timed out. Please try :login again.");
        }

        sleep(poll_interval).await;

        let token_resp: AccessTokenResponse = client
            .post("https://github.com/login/oauth/access_token")
            .header("Accept", "application/json")
            .header("User-Agent", USER_AGENT)
            .json(&serde_json::json!({
                "client_id": COPILOT_CLIENT_ID,
                "device_code": resp.device_code,
                "grant_type": "urn:ietf:params:oauth:grant-type:device_code"
            }))
            .send()
            .await
            .context("Failed to poll access token")?
            .json()
            .await
            .context("Failed to parse access token response")?;

        match token_resp.error.as_deref() {
            None | Some("") => {
                // success
            }
            Some("authorization_pending") => {
                print!(".");
                std::io::Write::flush(&mut std::io::stdout()).ok();
                continue;
            }
            Some("slow_down") => {
                sleep(Duration::from_secs(5)).await;
                continue;
            }
            Some("expired_token") => {
                bail!("Device code expired. Please run :login again.");
            }
            Some("access_denied") => {
                bail!("Authorization was denied by the user.");
            }
            Some(other) => {
                bail!("OAuth error: {}", other);
            }
        }

        if let Some(access_token) = token_resp.access_token {
            println!("\n✓ Authorized! Fetching Copilot API token...");
            return Ok(CopilotOAuthToken {
                access_token,
                token_type: token_resp
                    .token_type
                    .unwrap_or_else(|| "bearer".to_string()),
            });
        }
    }
}

// ─── Copilot API token refresh ────────────────────────────────────────────────

#[derive(Deserialize)]
struct CopilotTokenResponse {
    token: String,
    /// GitHub returns this as a float (Unix timestamp with fractional seconds),
    /// so we accept `f64` and truncate to `u64`.
    expires_at: f64,
}

/// Exchange the long-lived OAuth `access_token` for a short-lived Copilot API
/// token. Call this before every LLM request when `CopilotApiToken::is_expired()`.
///
/// If the OAuth token is expired or revoked, GitHub returns a JSON error body
/// like `{"message": "Bad credentials", "status": "401"}`. We detect that and
/// surface a clear user-facing message telling them to run `/login` again.
pub async fn refresh_copilot_token(
    client: &reqwest::Client,
    oauth_token: &str,
) -> Result<CopilotApiToken> {
    // Fetch raw bytes so we can inspect the body before deserializing.
    let bytes = client
        .get("https://api.github.com/copilot_internal/v2/token")
        .header("Authorization", format!("token {}", oauth_token))
        .header("User-Agent", USER_AGENT)
        .send()
        .await
        .context("Failed to refresh Copilot token")?
        .bytes()
        .await
        .context("Failed to read Copilot token response body")?;

    // Try to parse as the happy-path struct first.
    match serde_json::from_slice::<CopilotTokenResponse>(&bytes) {
        Ok(resp) => Ok(CopilotApiToken {
            token: resp.token,
            expires_at: resp.expires_at as u64,
        }),
        Err(_) => {
            // Parse failed — try to extract a human-readable error message.
            // GitHub error bodies look like: {"message": "Bad credentials", "status": "401"}
            let msg = serde_json::from_slice::<serde_json::Value>(&bytes)
                .ok()
                .and_then(|v| v["message"].as_str().map(|s| s.to_string()))
                .unwrap_or_else(|| {
                    String::from_utf8_lossy(&bytes)
                        .chars()
                        .take(120)
                        .collect::<String>()
                });
            bail!(
                "Copilot token refresh failed: {}\n\nYour saved credentials may be expired or revoked.\nRun /login to re-authenticate.",
                msg
            );
        }
    }
}
