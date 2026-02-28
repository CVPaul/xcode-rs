// Config module — implementation in Task 2
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[allow(dead_code)]
#[derive(Debug, Deserialize, Serialize)]
pub struct Config {
    pub provider: ProviderConfig,
    pub model: String,
    pub project_dir: Option<PathBuf>,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Serialize)]
pub struct ProviderConfig {
    pub api_base: String,
    pub api_key: String,
}
