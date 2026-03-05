use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::env;
use std::fs;
use std::path::PathBuf;

/// MCP Configuration matching ~/.config/cultra/mcp.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub api: APIConfig,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct APIConfig {
    pub base_url: String,
    pub key: String,
}

impl std::fmt::Debug for APIConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("APIConfig")
            .field("base_url", &self.base_url)
            .field("key", &if self.key.is_empty() { "<empty>" } else { "<redacted>" })
            .finish()
    }
}

impl Config {
    pub fn load() -> Result<Self> {
        // Try environment variable first
        if let Ok(config_path) = env::var("CULTRA_MCP_CONFIG") {
            return Self::load_from_file(&PathBuf::from(config_path));
        }

        // Try ~/.config/cultra/mcp.json (skip exists() check to avoid TOCTOU)
        if let Some(home) = dirs::home_dir() {
            let default_path = home.join(".config/cultra/mcp.json");
            match Self::load_from_file(&default_path) {
                Ok(config) => return Ok(config),
                Err(_) => {} // File doesn't exist or is invalid — fall through to env vars
            }
        }

        // Fall back to environment variables
        let key = env::var("CULTRA_API_KEY").unwrap_or_default();
        if key.is_empty() {
            tracing::warn!("CULTRA_API_KEY not set — API requests will fail with 401");
        }
        Ok(Config {
            api: APIConfig {
                base_url: env::var("CULTRA_API_URL")
                    .unwrap_or_else(|_| "http://localhost:8080".to_string()),
                key,
            },
        })
    }

    fn load_from_file(path: &PathBuf) -> Result<Self> {
        let content = fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let config: Config = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
        Ok(config)
    }
}
