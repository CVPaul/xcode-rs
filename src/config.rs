use crate::agent::context_manager::ContextConfig;
use crate::llm::retry::RetryConfig;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Top-level config. Every section uses `#[serde(default)]` so that
/// existing config files written by older versions (missing new fields)
/// are still loaded successfully — new fields silently get their defaults.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct Config {
    pub provider: ProviderConfig,
    pub model: String,
    pub project_dir: Option<PathBuf>,
    pub sandbox: SandboxConfig,
    pub agent: AgentConfig,
    /// LSP server configuration.  Disabled by default — enabled by
    /// providing a `lsp.server_command` or by auto-detection at runtime.
    #[serde(default)]
    pub lsp: LspConfig,
    /// List of external MCP servers to connect on startup.  Each entry
    /// describes a subprocess to spawn; xcodeai will register all tools
    /// the server advertises.  Empty by default — add entries to enable MCP.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    /// User-defined custom tools.  Each entry describes a shell command that
    /// the LLM can invoke as a tool.  Empty by default.
    #[serde(default)]
    pub custom_tools: Vec<CustomToolConfig>,
    /// Permission rules for tool execution.  Each rule matches a tool name
    /// pattern and specifies whether that tool requires user confirmation.
    /// Empty by default (all tools run without confirmation).
    #[serde(default)]
    pub permissions: Vec<PermissionRule>,
    /// Code formatters to run after file_write / file_edit.  Keys are file
    /// extensions (e.g. "rs", "py"), values are shell commands that receive
    /// the file path as `{}`.  Example: `{"rs": "rustfmt {}", "py": "black {}"}`.
    #[serde(default)]
    pub formatters: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub api_base: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct SandboxConfig {
    pub enabled: bool,
    pub sbox_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct AgentConfig {
    pub max_iterations: u32,
    pub max_tool_calls_per_response: u32,
    /// Maximum number of auto-continue injections when the LLM stops mid-task
    /// without the `[TASK_COMPLETE]` marker.  Prevents infinite loops if the
    /// LLM never produces the marker.  Default: 20.
    pub max_auto_continues: u32,
    /// Retry/back-off configuration for LLM HTTP calls.
    /// Stored under the `"agent"` section in config.json.
    pub retry: RetryConfig,
    /// Smart context-window management configuration.
    /// Stored under the `"agent"` section in config.json.
    pub context: ContextConfig,
    /// Compact mode: reduces tool output to 50 lines and adds a brevity
    /// instruction to the system prompt.  Useful for quick tasks where
    /// minimising token usage matters more than full output visibility.
    /// Default: false.  Toggled at runtime by `/compact` in the REPL.
    pub compact_mode: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ConfigOverrides {
    pub api_key: Option<String>,
    pub api_base: Option<String>,
    pub model: Option<String>,
    pub project_dir: Option<PathBuf>,
    pub no_sandbox: bool,
    /// Enable compact mode (shorter tool output, brevity-focused system prompt).
    pub compact: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            provider: ProviderConfig {
                api_base: "https://api.openai.com/v1".to_string(),
                api_key: String::new(),
            },
            model: "gpt-4o".to_string(),
            project_dir: None,
            sandbox: SandboxConfig {
                enabled: true,
                sbox_path: None,
            },
            agent: AgentConfig {
                max_iterations: 25,
                max_tool_calls_per_response: 10,
                max_auto_continues: 20,
                retry: RetryConfig::default(),
                context: ContextConfig::default(),
                compact_mode: false,
            },
            lsp: LspConfig::default(),
            mcp_servers: Vec::new(),
            custom_tools: Vec::new(),
            permissions: Vec::new(),
            formatters: std::collections::HashMap::new(),
        }
    }
}

impl Default for ProviderConfig {
    fn default() -> Self {
        ProviderConfig {
            api_base: "https://api.openai.com/v1".to_string(),
            api_key: String::new(),
        }
    }
}

impl Default for SandboxConfig {
    fn default() -> Self {
        SandboxConfig {
            enabled: true,
            sbox_path: None,
        }
    }
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            max_iterations: 25,
            max_tool_calls_per_response: 10,
            max_auto_continues: 20,
            retry: RetryConfig::default(),
            context: ContextConfig::default(),
            compact_mode: false,
        }
    }
}

// ─── McpServerConfig ────────────────────────────────────────────────────────

/// Configuration for a single external MCP (Model Context Protocol) server.
///
/// Each entry in `config.mcp_servers` describes one subprocess to spawn at
/// startup.  xcodeai will run `command` with `args`, perform the MCP
/// `initialize` handshake, and register all discovered tools into the tool
/// registry so the LLM can call them like any built-in tool.
///
/// # Example `config.json` entry
///
/// ```json
/// {
///   "mcp_servers": [
///     {
///       "name": "filesystem",
///       "command": "npx",
///       "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
///       "env": {}
///     }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct McpServerConfig {
    /// Human-readable label, used only in the `/mcp` REPL command output.
    pub name: String,

    /// The executable to run, e.g. `"npx"` or `"python"` or `"/usr/local/bin/my-mcp-server"`.
    pub command: String,

    /// CLI arguments forwarded to the subprocess, e.g. `["-y", "@mcp/fs", "/tmp"]`.
    pub args: Vec<String>,

    /// Extra environment variables injected into the subprocess.
    /// Useful for passing secrets (API keys) without embedding them in `command`.
    pub env: std::collections::HashMap<String, String>,
}

// ─── CustomToolConfig ─────────────────────────────────────────────────────

/// Configuration for a user-defined custom tool.
///
/// Each entry in `config.custom_tools` describes a shell command the LLM can
/// invoke as a tool.  Placeholders like `{{path}}` in `command` are replaced
/// with the corresponding parameter value from the LLM's tool call.
///
/// # Example `config.json` entry
///
/// ```json
/// {
///   "custom_tools": [
///     {
///       "name": "deploy",
///       "description": "Deploy the application to staging",
///       "command": "make deploy-staging",
///       "parameters": {}
///     }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct CustomToolConfig {
    /// Tool name as seen by the LLM (e.g. `"deploy"`).
    pub name: String,
    /// Human-readable description shown in the tool list.
    pub description: String,
    /// Shell command to execute.  May contain `{{param}}` placeholders.
    pub command: String,
    /// JSON Schema properties for the tool parameters.  Empty object `{}` means no parameters.
    pub parameters: serde_json::Value,
}

// ─── PermissionRule ──────────────────────────────────────────────────────────

/// A permission rule that controls whether a tool requires user confirmation.
///
/// When `confirm` is `true`, the tool will prompt before execution even if the
/// agent is running in auto-approve mode.  Tool names support glob patterns:
/// `"bash"` matches exactly, `"git_*"` matches all git tools.
///
/// # Example
///
/// ```json
/// {
///   "permissions": [
///     { "tool": "bash", "confirm": true },
///     { "tool": "git_*", "confirm": true }
///   ]
/// }
/// ```
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct PermissionRule {
    /// Tool name or glob pattern (e.g. `"bash"`, `"git_*"`).
    pub tool: String,
    /// Whether this tool requires explicit user confirmation before execution.
    pub confirm: bool,
}

// ─── LspConfig ──────────────────────────────────────────────────────────────

/// Configuration for the LSP (Language Server Protocol) client.
///
/// By default LSP is disabled (`enabled: false`).  To activate it:
/// - set `enabled: true` in config.json, OR
/// - rely on `LspClient::detect_server()` auto-detection at runtime.
#[derive(Debug, Clone, Deserialize, Serialize, Default)]
#[serde(default)]
pub struct LspConfig {
    /// Whether the LSP integration is active.  Default: false.
    pub enabled: bool,
    /// Override the auto-detected server command.  E.g. `"rust-analyzer"`.
    pub server_command: Option<String>,
    /// Extra arguments passed to the server binary.  Default: empty.
    pub args: Vec<String>,
}

impl Config {
    /// Load config with precedence: defaults → file → env → CLI overrides
    #[allow(dead_code)]
    pub fn load(overrides: &ConfigOverrides) -> Result<Self> {
        Self::load_from_path(None, overrides)
    }

    /// Load config from a specific path (or default path if None)
    pub fn load_from_path(path: Option<&Path>, overrides: &ConfigOverrides) -> Result<Self> {
        let mut config = Config::default();

        // Determine config file path
        let config_path = if let Some(p) = path {
            p.to_path_buf()
        } else {
            let base = dirs::config_dir().context("Could not determine config directory")?;
            base.join("xcode").join("config.json")
        };

        // Try to read and parse config file
        if config_path.exists() {
            let content = fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read config file: {:?}", config_path))?;
            let file_config: Config =
                serde_json::from_str(&content).context("Failed to parse config JSON")?;
            config = file_config;
        } else {
            // Create default config file
            if let Some(parent) = config_path.parent() {
                fs::create_dir_all(parent)
                    .with_context(|| format!("Failed to create config directory: {:?}", parent))?;
            }
            let default_json = serde_json::to_string_pretty(&config)
                .context("Failed to serialize default config")?;
            fs::write(&config_path, default_json)
                .with_context(|| format!("Failed to write default config: {:?}", config_path))?;
        }

        // Apply env var overrides
        if let Ok(api_key) = std::env::var("XCODE_API_KEY") {
            config.provider.api_key = api_key;
        }
        if let Ok(api_base) = std::env::var("XCODE_API_BASE") {
            config.provider.api_base = api_base;
        }
        if let Ok(model) = std::env::var("XCODE_MODEL") {
            config.model = model;
        }

        // Apply CLI overrides (highest priority)
        if let Some(api_key) = &overrides.api_key {
            config.provider.api_key = api_key.clone();
        }
        if let Some(api_base) = &overrides.api_base {
            config.provider.api_base = api_base.clone();
        }
        if let Some(model) = &overrides.model {
            config.model = model.clone();
        }
        if let Some(project_dir) = &overrides.project_dir {
            config.project_dir = Some(project_dir.clone());
        }
        if overrides.no_sandbox {
            config.sandbox.enabled = false;
        }
        // Apply compact mode override from CLI flag.
        // Once set true here, it persists for the life of this config instance.
        if overrides.compact {
            config.agent.compact_mode = true;
        }

        Ok(config)
    }

    /// Save current config back to the default config file path.
    /// Only persists provider.api_base and provider.api_key (not CLI-only fields
    /// like project_dir which should not be written to the shared config file).
    pub fn save_provider(api_base: &str, api_key: &str) -> Result<()> {
        let base = dirs::config_dir().context("Could not determine config directory")?;
        let config_path = base.join("xcode").join("config.json");

        // Load current file config (or default), apply the new provider fields, save.
        let mut config = if config_path.exists() {
            let content = fs::read_to_string(&config_path)
                .with_context(|| format!("Failed to read config file: {:?}", config_path))?;
            serde_json::from_str::<Config>(&content).unwrap_or_default()
        } else {
            Config::default()
        };

        config.provider.api_base = api_base.to_string();
        config.provider.api_key = api_key.to_string();

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_string_pretty(&config).context("Failed to serialize config")?;
        fs::write(&config_path, json)
            .with_context(|| format!("Failed to write config file: {:?}", config_path))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // Mutex to ensure test isolation when modifying environment variables
    static TEST_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.agent.max_iterations, 25);
        assert_eq!(config.agent.max_tool_calls_per_response, 10);
        assert!(config.sandbox.enabled);
        assert_eq!(config.model, "gpt-4o");
        assert_eq!(config.provider.api_base, "https://api.openai.com/v1");
        // Retry defaults should match RetryConfig::default()
        assert_eq!(config.agent.retry.max_retries, 5);
        assert_eq!(config.agent.retry.initial_delay_ms, 1000);
    }

    #[test]
    fn test_load_from_file() {
        let _lock = TEST_LOCK.lock().unwrap();

        // Clear env vars to avoid test isolation issues
        std::env::remove_var("XCODE_API_KEY");
        std::env::remove_var("XCODE_API_BASE");
        std::env::remove_var("XCODE_MODEL");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        // Write a test config
        let test_config = Config {
            provider: ProviderConfig {
                api_base: "https://custom.api.com".to_string(),
                api_key: "test_key_123".to_string(),
            },
            model: "gpt-4-turbo".to_string(),
            project_dir: Some(PathBuf::from("/test/project")),
            sandbox: SandboxConfig {
                enabled: false,
                sbox_path: Some("/tmp/sbox".to_string()),
            },
            agent: AgentConfig {
                max_iterations: 50,
                max_tool_calls_per_response: 20,
                max_auto_continues: 20,
                retry: RetryConfig::default(),
                context: ContextConfig::default(),
                compact_mode: false,
            },
            lsp: LspConfig::default(),
            mcp_servers: vec![],
            custom_tools: vec![],
            permissions: vec![],
            formatters: std::collections::HashMap::new(),
        };

        let json = serde_json::to_string_pretty(&test_config).unwrap();
        fs::write(&config_path, json).unwrap();

        // Load config from file
        let overrides = ConfigOverrides {
            api_key: None,
            api_base: None,
            model: None,
            project_dir: None,
            no_sandbox: false,
            compact: false,
        };
        let loaded = Config::load_from_path(Some(&config_path), &overrides).unwrap();

        assert_eq!(loaded.provider.api_base, "https://custom.api.com");
        assert_eq!(loaded.provider.api_key, "test_key_123");
        assert_eq!(loaded.model, "gpt-4-turbo");
        assert_eq!(loaded.agent.max_iterations, 50);
        assert_eq!(loaded.agent.max_tool_calls_per_response, 20);
        assert!(!loaded.sandbox.enabled);
    }

    #[test]
    fn test_env_override() {
        let _lock = TEST_LOCK.lock().unwrap();

        // Clear env vars first to ensure test isolation
        std::env::remove_var("XCODE_API_KEY");
        std::env::remove_var("XCODE_API_BASE");
        std::env::remove_var("XCODE_MODEL");

        std::env::set_var("XCODE_API_KEY", "env_test_key");
        std::env::set_var("XCODE_API_BASE", "https://env.api.com");
        std::env::set_var("XCODE_MODEL", "env-model");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        let overrides = ConfigOverrides {
            api_key: None,
            api_base: None,
            model: None,
            project_dir: None,
            no_sandbox: false,
            compact: false,
        };
        let config = Config::load_from_path(Some(&config_path), &overrides).unwrap();

        assert_eq!(config.provider.api_key, "env_test_key");
        assert_eq!(config.provider.api_base, "https://env.api.com");
        assert_eq!(config.model, "env-model");

        // Cleanup
        std::env::remove_var("XCODE_API_KEY");
        std::env::remove_var("XCODE_API_BASE");
        std::env::remove_var("XCODE_MODEL");
    }

    #[test]
    fn test_cli_override_takes_precedence() {
        let _lock = TEST_LOCK.lock().unwrap();

        // Clear env vars first to ensure test isolation
        std::env::remove_var("XCODE_API_KEY");
        std::env::remove_var("XCODE_API_BASE");
        std::env::remove_var("XCODE_MODEL");

        std::env::set_var("XCODE_API_KEY", "env_key");
        std::env::set_var("XCODE_MODEL", "env-model");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        let overrides = ConfigOverrides {
            api_key: Some("cli_key".to_string()),
            api_base: Some("https://cli.api.com".to_string()),
            model: Some("cli-model".to_string()),
            project_dir: None,
            no_sandbox: false,
            compact: false,
        };
        let config = Config::load_from_path(Some(&config_path), &overrides).unwrap();

        // CLI overrides should win
        assert_eq!(config.provider.api_key, "cli_key");
        assert_eq!(config.provider.api_base, "https://cli.api.com");
        assert_eq!(config.model, "cli-model");

        // Cleanup
        std::env::remove_var("XCODE_API_KEY");
        std::env::remove_var("XCODE_MODEL");
    }

    #[test]
    fn test_sandbox_disable_override() {
        let _lock = TEST_LOCK.lock().unwrap();

        // Clear env vars to avoid test isolation issues
        std::env::remove_var("XCODE_API_KEY");
        std::env::remove_var("XCODE_API_BASE");
        std::env::remove_var("XCODE_MODEL");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        let overrides = ConfigOverrides {
            api_key: None,
            api_base: None,
            model: None,
            project_dir: None,
            no_sandbox: true,
            compact: false,
        };
        let config = Config::load_from_path(Some(&config_path), &overrides).unwrap();

        assert!(!config.sandbox.enabled);
    }

    /// Verify that a config file written by an older version (missing newer
    /// fields like `max_auto_continues` or `retry`) still loads correctly —
    /// the missing fields should silently receive their defaults.
    #[test]
    fn test_backwards_compatible_config() {
        let _lock = TEST_LOCK.lock().unwrap();

        // Clear env vars to avoid test isolation issues
        std::env::remove_var("XCODE_API_KEY");
        std::env::remove_var("XCODE_API_BASE");
        std::env::remove_var("XCODE_MODEL");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        // Simulate an old v0.7.x config file that lacks max_auto_continues and retry
        let old_config_json = r#"{
  "provider": {
    "api_base": "https://api.openai.com/v1",
    "api_key": "old-key"
  },
  "model": "gpt-4o",
  "sandbox": {
    "enabled": false
  },
  "agent": {
    "max_iterations": 25,
    "max_tool_calls_per_response": 10
  }
}"#;
        fs::write(&config_path, old_config_json).unwrap();

        let overrides = ConfigOverrides {
            api_key: None,
            api_base: None,
            model: None,
            project_dir: None,
            no_sandbox: false,
            compact: false,
        };
        let loaded = Config::load_from_path(Some(&config_path), &overrides).unwrap();

        // Explicitly-set fields should load correctly
        assert_eq!(loaded.provider.api_key, "old-key");
        assert_eq!(loaded.agent.max_iterations, 25);
        assert!(!loaded.sandbox.enabled);

        // Missing fields should get default values
        assert_eq!(loaded.agent.max_auto_continues, 20);
        assert_eq!(loaded.agent.retry.max_retries, 5);
    }

    /// Verify that mcp_servers entries in config.json deserialise correctly,
    /// and that old configs without the field still load (empty default).
    #[test]
    fn test_mcp_config_parsing() {
        let _lock = TEST_LOCK.lock().unwrap();

        std::env::remove_var("XCODE_API_KEY");
        std::env::remove_var("XCODE_API_BASE");
        std::env::remove_var("XCODE_MODEL");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        // A config that declares one MCP server.
        let json = r#"{
  "provider": {
    "api_base": "https://api.openai.com/v1",
    "api_key": "test-key"
  },
  "model": "gpt-4o",
  "mcp_servers": [
    {
      "name": "filesystem",
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
      "env": { "MCP_FS_ROOT": "/tmp" }
    }
  ]
}"#;
        fs::write(&config_path, json).unwrap();

        let overrides = ConfigOverrides {
            api_key: None,
            api_base: None,
            model: None,
            project_dir: None,
            no_sandbox: false,
            compact: false,
        };
        let cfg = Config::load_from_path(Some(&config_path), &overrides).unwrap();

        // One server should have been parsed.
        assert_eq!(cfg.mcp_servers.len(), 1);
        let srv = &cfg.mcp_servers[0];
        assert_eq!(srv.name, "filesystem");
        assert_eq!(srv.command, "npx");
        assert_eq!(
            srv.args,
            vec!["-y", "@modelcontextprotocol/server-filesystem", "/tmp"]
        );
        assert_eq!(srv.env.get("MCP_FS_ROOT").map(String::as_str), Some("/tmp"));
    }

    /// An old config without mcp_servers should load fine and default to empty.
    #[test]
    fn test_mcp_servers_defaults_to_empty() {
        let _lock = TEST_LOCK.lock().unwrap();

        std::env::remove_var("XCODE_API_KEY");
        std::env::remove_var("XCODE_API_BASE");
        std::env::remove_var("XCODE_MODEL");

        let temp_dir = tempfile::TempDir::new().unwrap();
        let config_path = temp_dir.path().join("config.json");

        // Old-style config — no mcp_servers key at all.
        let json = r#"{
  "provider": { "api_base": "https://api.openai.com/v1", "api_key": "k" },
  "model": "gpt-4o"
}"#;
        fs::write(&config_path, json).unwrap();

        let overrides = ConfigOverrides {
            api_key: None,
            api_base: None,
            model: None,
            project_dir: None,
            no_sandbox: false,
            compact: false,
        };
        let cfg = Config::load_from_path(Some(&config_path), &overrides).unwrap();
        assert!(
            cfg.mcp_servers.is_empty(),
            "mcp_servers should default to empty vec"
        );
    }
}
