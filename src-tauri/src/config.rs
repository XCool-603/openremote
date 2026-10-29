use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub device_id: String,
    #[serde(default = "generate_numeric_device_code")]
    pub device_code: String,
    #[serde(default = "generate_random_password")]
    pub access_password: String,
    pub device_name: String,
    pub listen_port: u16,
    pub signal_server_url: String,
    pub auth_token: Option<String>,
}

fn generate_numeric_device_code() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let num: u32 = rng.gen_range(100_000_000..999_999_999);
    format!("{:03}-{:03}-{:03}", num / 1_000_000, (num / 1_000) % 1_000, num % 1_000)
}

fn generate_random_password() -> String {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let pass: u32 = rng.gen_range(100_000..999_999);
    format!("{:06}", pass)
}

impl Default for AppConfig {
    fn default() -> Self {
        let hostname = match std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")) {
            Ok(name) => name,
            Err(_) => "OpenRemote-Device".to_string(),
        };

        Self {
            device_id: Uuid::new_v4().to_string(),
            device_code: generate_numeric_device_code(),
            access_password: generate_random_password(),
            device_name: hostname,
            listen_port: 28843,
            signal_server_url: "http://127.0.0.1:28844".to_string(),
            auth_token: None,
        }
    }
}

impl AppConfig {
    /// Returns default config file path
    pub fn config_path() -> PathBuf {
        let base_dir = dirs_or_local();
        base_dir.join("openremote").join("config.json")
    }

    /// Load config from disk, or create and save default config if absent
    pub fn load() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let data = fs::read_to_string(&path)
                .with_context(|| format!("Failed to read config file from {:?}", path))?;
            let config: AppConfig = serde_json::from_str(&data)
                .with_context(|| "Failed to parse config JSON")?;
            Ok(config)
        } else {
            let config = Self::default();
            config.save()?;
            Ok(config)
        }
    }

    /// Save current config to disk
    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create config directory {:?}", parent))?;
        }
        let serialized = serde_json::to_string_pretty(self)
            .with_context(|| "Failed to serialize config to JSON")?;
        fs::write(&path, serialized)
            .with_context(|| format!("Failed to write config file to {:?}", path))?;
        Ok(())
    }
}

fn dirs_or_local() -> PathBuf {
    if let Ok(app_data) = std::env::var("APPDATA") {
        PathBuf::from(app_data)
    } else if let Ok(home) = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")) {
        PathBuf::from(home).join(".config")
    } else {
        PathBuf::from(".")
    }
}
