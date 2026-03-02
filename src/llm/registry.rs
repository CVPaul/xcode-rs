// src/llm/registry.rs
//
// Provider registry and auto-detection for xcodeai.
//
// # Purpose
//
// This module centralises the "which provider do I create for this api_base?" logic
// that was previously duplicated across `context.rs` (in `new()`, `switch_provider()`,
// and `switch_model()`).  All three call-sites now delegate to `create_provider()`.
//
// # Auto-detection rules
//
// | api_base value                            | Provider selected    |
// |------------------------------------------|----------------------|
// | `"copilot"` (COPILOT_API_BASE)            | OpenAiProvider (copilot mode) |
// | `"anthropic"` (ANTHROPIC_API_BASE)        | AnthropicProvider    |
// | `"https://api.anthropic.com/…"`           | AnthropicProvider    |
// | `"gemini"` (GEMINI_API_BASE)              | GeminiProvider       |
// | `"https://generativelanguage.googleapis…"`| GeminiProvider       |
// | anything else                             | OpenAiProvider       |
//
// The string sentinel constants ("copilot", "anthropic", "gemini") remain the
// canonical way to select a provider; the URL prefix rules are a convenience
// so users can paste the full API base URL without needing to know the sentinel.

use std::sync::Arc;

use crate::config::ProviderConfig;
use crate::io::AgentIO;
use crate::llm::anthropic::{AnthropicProvider, ANTHROPIC_API_BASE};
use crate::llm::gemini::{GeminiProvider, GEMINI_API_BASE};
use crate::llm::openai::{OpenAiProvider, COPILOT_API_BASE};
use crate::llm::retry::RetryConfig;
use crate::llm::LlmProvider;

// ─── ProviderRegistry ─────────────────────────────────────────────────────────

/// Factory that maps an `api_base` string to the correct `LlmProvider` implementation.
///
/// The registry is stateless — it contains no mutable data.  Every call to
/// `create_provider` produces a *new* `Arc<dyn LlmProvider>`.
///
/// # Example
///
/// ```rust,ignore
/// use xcodeai::llm::registry::ProviderRegistry;
/// use xcodeai::config::ProviderConfig;
/// use xcodeai::llm::retry::RetryConfig;
/// use xcodeai::io::NullIO;
/// use std::sync::Arc;
/// let config = ProviderConfig {
///     api_base: "anthropic".to_string(),
///     api_key: "sk-ant-test".to_string(),
/// };
/// let io: Arc<dyn xcodeai::io::AgentIO> = Arc::new(NullIO);
/// let provider = ProviderRegistry::create_provider(
///     &config,
///     "claude-3-5-sonnet-20241022",
///     &RetryConfig::default(),
///     &io,
///     None,
/// );
/// ```
pub struct ProviderRegistry;

impl ProviderRegistry {
    /// Determine which `LlmProvider` to construct for the given `api_base`.
    ///
    /// # Arguments
    ///
    /// * `config`      — provider config (api_base + api_key)
    /// * `model`       — model name string (e.g. `"gpt-4o"`, `"claude-3-5-sonnet-20241022"`)
    /// * `retry`       — retry configuration (used only for OpenAI-compatible providers)
    /// * `io`          — I/O handle for retry status messages (OpenAI-compat only)
    /// * `oauth_token` — GitHub Copilot OAuth token (only used when api_base == "copilot")
    ///
    /// # Provider selection
    ///
    /// The selection follows the table in the module docstring.  Sentinel string
    /// constants take priority; URL prefix matching is a fallback convenience.
    pub fn create_provider(
        config: &ProviderConfig,
        model: &str,
        retry: &RetryConfig,
        io: &Arc<dyn AgentIO>,
        oauth_token: Option<String>,
    ) -> Arc<dyn LlmProvider> {
        let api_base = &config.api_base;
        let api_key = &config.api_key;

        if api_base == COPILOT_API_BASE {
            // ── GitHub Copilot ───────────────────────────────────────────────────
            // Uses the OpenAI-compat provider in Copilot mode.
            // The caller is responsible for loading the OAuth token from disk.
            let token = oauth_token.unwrap_or_default();
            Arc::new(
                OpenAiProvider::new_copilot(token, model.to_string())
                    .with_io(io.clone())
                    .with_retry(retry.clone()),
            ) as Arc<dyn LlmProvider>
        } else if is_anthropic(api_base) {
            // ── Anthropic native ─────────────────────────────────────────────────
            Arc::new(AnthropicProvider::new(api_key.clone(), model.to_string()))
                as Arc<dyn LlmProvider>
        } else if is_gemini(api_base) {
            // ── Google Gemini native ──────────────────────────────────────────────
            Arc::new(GeminiProvider::new(api_key.clone(), model.to_string()))
                as Arc<dyn LlmProvider>
        } else {
            // ── OpenAI-compatible (default) ───────────────────────────────────────
            Arc::new(
                OpenAiProvider::new(api_base.clone(), api_key.clone(), model.to_string())
                    .with_io(io.clone())
                    .with_retry(retry.clone()),
            ) as Arc<dyn LlmProvider>
        }
    }

    /// Returns a human-readable list of all built-in provider names and their
    /// `api_base` sentinels.  Used by the `/connect` REPL command to display
    /// available choices.
    ///
    /// Each entry is `(display_name, api_base_sentinel, example_model)`.
    #[allow(dead_code)]
    pub fn builtin_providers() -> Vec<(&'static str, &'static str, &'static str)> {
        vec![
            // (display name, api_base sentinel, example model)
            ("OpenAI", "https://api.openai.com/v1", "gpt-4o"),
            (
                "Anthropic",
                ANTHROPIC_API_BASE,
                "claude-3-5-sonnet-20241022",
            ),
            ("Google Gemini", GEMINI_API_BASE, "gemini-2.0-flash"),
            ("DeepSeek", "https://api.deepseek.com/v1", "deepseek-chat"),
            (
                "Qwen",
                "https://dashscope.aliyuncs.com/compatible-mode/v1",
                "qwen-turbo",
            ),
            ("GLM", "https://open.bigmodel.cn/api/paas/v4", "glm-4"),
            ("Ollama", "http://localhost:11434/v1", "llama3"),
            ("GitHub Copilot", COPILOT_API_BASE, "gpt-4o"),
        ]
    }
}

// ─── Auto-detection helpers ───────────────────────────────────────────────────

/// Returns true if `api_base` indicates the Anthropic native API.
///
/// Accepts both the short sentinel (`"anthropic"`) and the full base URL
/// (any URL starting with `https://api.anthropic.com`).
fn is_anthropic(api_base: &str) -> bool {
    api_base == ANTHROPIC_API_BASE || api_base.starts_with("https://api.anthropic.com")
}

/// Returns true if `api_base` indicates the Google Gemini native API.
///
/// Accepts both the short sentinel (`"gemini"`) and the full base URL
/// (any URL starting with `https://generativelanguage.googleapis.com`).
fn is_gemini(api_base: &str) -> bool {
    api_base == GEMINI_API_BASE || api_base.starts_with("https://generativelanguage.googleapis.com")
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::NullIO;

    fn make_config(api_base: &str) -> ProviderConfig {
        ProviderConfig {
            api_base: api_base.to_string(),
            api_key: "test-key".to_string(),
        }
    }

    fn null_io() -> Arc<dyn AgentIO> {
        Arc::new(NullIO)
    }

    fn default_retry() -> RetryConfig {
        RetryConfig::default()
    }

    // ── is_anthropic() ────────────────────────────────────────────────────────

    /// The "anthropic" sentinel is recognised.
    #[test]
    fn test_is_anthropic_sentinel() {
        assert!(is_anthropic(ANTHROPIC_API_BASE));
        assert!(is_anthropic("anthropic"));
    }

    /// Full Anthropic API URL is recognised.
    #[test]
    fn test_is_anthropic_full_url() {
        assert!(is_anthropic("https://api.anthropic.com/v1"));
        assert!(is_anthropic("https://api.anthropic.com"));
    }

    /// Non-Anthropic URLs are not confused with Anthropic.
    #[test]
    fn test_is_anthropic_false_for_others() {
        assert!(!is_anthropic("https://api.openai.com/v1"));
        assert!(!is_anthropic("gemini"));
        assert!(!is_anthropic("copilot"));
        assert!(!is_anthropic("https://api.deepseek.com/v1"));
    }

    // ── is_gemini() ───────────────────────────────────────────────────────────

    /// The "gemini" sentinel is recognised.
    #[test]
    fn test_is_gemini_sentinel() {
        assert!(is_gemini(GEMINI_API_BASE));
        assert!(is_gemini("gemini"));
    }

    /// Full Gemini API URL is recognised.
    #[test]
    fn test_is_gemini_full_url() {
        assert!(is_gemini(
            "https://generativelanguage.googleapis.com/v1beta"
        ));
        assert!(is_gemini("https://generativelanguage.googleapis.com"));
    }

    /// Non-Gemini URLs are not confused with Gemini.
    #[test]
    fn test_is_gemini_false_for_others() {
        assert!(!is_gemini("https://api.openai.com/v1"));
        assert!(!is_gemini("anthropic"));
        assert!(!is_gemini("copilot"));
    }

    // ── builtin_providers() ───────────────────────────────────────────────────

    /// builtin_providers() lists all expected providers.
    #[test]
    fn test_builtin_providers_contains_all() {
        let providers = ProviderRegistry::builtin_providers();
        // Must include the four main ones
        let bases: Vec<&str> = providers.iter().map(|(_, base, _)| *base).collect();
        assert!(bases.contains(&ANTHROPIC_API_BASE), "Anthropic missing");
        assert!(bases.contains(&GEMINI_API_BASE), "Gemini missing");
        assert!(bases.contains(&COPILOT_API_BASE), "Copilot missing");
        // At least one OpenAI-compat
        assert!(
            bases
                .iter()
                .any(|b| b.starts_with("https://api.openai.com")),
            "OpenAI missing"
        );
    }

    /// Each builtin provider entry has non-empty display name and model.
    #[test]
    fn test_builtin_providers_fields_non_empty() {
        for (name, base, model) in ProviderRegistry::builtin_providers() {
            assert!(!name.is_empty(), "empty display name for base={}", base);
            assert!(!base.is_empty(), "empty api_base for name={}", name);
            assert!(!model.is_empty(), "empty model for name={}", name);
        }
    }

    // ── create_provider() ─────────────────────────────────────────────────────

    /// create_provider() with Copilot api_base doesn't panic and returns a provider.
    #[test]
    fn test_create_provider_copilot() {
        let config = make_config(COPILOT_API_BASE);
        let _provider = ProviderRegistry::create_provider(
            &config,
            "gpt-4o",
            &default_retry(),
            &null_io(),
            Some("fake-token".to_string()),
        );
        // Just verifying it doesn't panic and type-checks
    }

    /// create_provider() with Anthropic sentinel returns a provider.
    #[test]
    fn test_create_provider_anthropic_sentinel() {
        let config = make_config(ANTHROPIC_API_BASE);
        let _provider = ProviderRegistry::create_provider(
            &config,
            "claude-3-5-sonnet-20241022",
            &default_retry(),
            &null_io(),
            None,
        );
    }

    /// create_provider() with Gemini sentinel returns a provider.
    #[test]
    fn test_create_provider_gemini_sentinel() {
        let config = make_config(GEMINI_API_BASE);
        let _provider = ProviderRegistry::create_provider(
            &config,
            "gemini-2.0-flash",
            &default_retry(),
            &null_io(),
            None,
        );
    }

    /// create_provider() with full Anthropic URL also works.
    #[test]
    fn test_create_provider_anthropic_url() {
        let config = make_config("https://api.anthropic.com/v1");
        let _provider = ProviderRegistry::create_provider(
            &config,
            "claude-3-5-sonnet-20241022",
            &default_retry(),
            &null_io(),
            None,
        );
    }

    /// create_provider() with full Gemini URL also works.
    #[test]
    fn test_create_provider_gemini_url() {
        let config = make_config("https://generativelanguage.googleapis.com/v1beta");
        let _provider = ProviderRegistry::create_provider(
            &config,
            "gemini-2.0-flash",
            &default_retry(),
            &null_io(),
            None,
        );
    }

    /// create_provider() falls back to OpenAI-compat for unknown api_base.
    #[test]
    fn test_create_provider_openai_fallback() {
        let config = make_config("https://api.deepseek.com/v1");
        let _provider = ProviderRegistry::create_provider(
            &config,
            "deepseek-chat",
            &default_retry(),
            &null_io(),
            None,
        );
    }

    /// create_provider() with localhost Ollama URL uses OpenAI-compat fallback.
    #[test]
    fn test_create_provider_ollama() {
        let config = make_config("http://localhost:11434/v1");
        let _provider = ProviderRegistry::create_provider(
            &config,
            "llama3",
            &default_retry(),
            &null_io(),
            None,
        );
    }
}
