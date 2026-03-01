use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Config {
    pub provider: ProviderConfig,
    pub model: String,
    pub project_dir: Option<PathBuf>,
    pub sandbox: SandboxConfig,
    pub agent: AgentConfig,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub api_base: String,
    pub api_key: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SandboxConfig {
    pub enabled: bool,
    pub sbox_path: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct AgentConfig {
    pub max_iterations: u32,
    pub max_tool_calls_per_response: u32,
}

pub struct ConfigOverrides {
    pub api_key: Option<String>,
    pub api_base: Option<String>,
    pub model: Option<String>,
    pub project_dir: Option<PathBuf>,
    pub no_sandbox: bool,
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
            },
        }
    }
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
        let json = serde_json::to_string_pretty(&config)
            .context("Failed to serialize config")?;
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
            },
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
        };
        let config = Config::load_from_path(Some(&config_path), &overrides).unwrap();

        assert!(!config.sandbox.enabled);
    }
}
