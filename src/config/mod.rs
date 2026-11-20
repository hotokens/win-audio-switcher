//! Loading and saving the config (favorites).
//!
//! Location: `%APPDATA%\SwitcherProgramm\config.json`.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use thiserror::Error;

use crate::hotkey::Hotkey;

/// Favorites structure in the config.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Favorites {
    /// List of endpoint IDs of favorite devices.
    pub devices: Vec<String>,
    /// Global switching hotkey (if set).
    /// `#[serde(default)]` — old configs without this field are read as `None`.
    #[serde(default)]
    pub hotkey: Option<Hotkey>,
}

/// Config-related errors.
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON parse error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Get the path to the config file.
fn config_path() -> PathBuf {
    let mut path = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    path.push("SwitcherProgramm");
    fs::create_dir_all(&path).ok();
    path.push("config.json");
    path
}

/// Load favorites from the config.
/// If the file does not exist or is empty — returns the default (empty favorites).
pub fn load_favorites() -> Favorites {
    let path = config_path();
    if !path.exists() {
        return Favorites::default();
    }

    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(e) => {
            log::error!("Failed to read config: {}", e);
            return Favorites::default();
        }
    };

    if content.trim().is_empty() {
        return Favorites::default();
    }

    match serde_json::from_str::<Favorites>(&content) {
        Ok(favorites) => {
            log::info!("Loaded favorites: {:?}", favorites.devices);
            favorites
        }
        Err(e) => {
            log::error!("Failed to parse config: {}", e);
            Favorites::default()
        }
    }
}

/// Save favorites to the config (atomically: temporary file + rename).
pub fn save_favorites(favorites: &Favorites) -> Result<(), ConfigError> {
    let path = config_path();

    let content = serde_json::to_string_pretty(favorites)?;

    let temp_path = path.with_extension("json.tmp");
    fs::write(&temp_path, &content)?;
    fs::rename(&temp_path, &path)?;

    log::info!("Saved favorites: {:?}", favorites.devices);
    Ok(())
}
