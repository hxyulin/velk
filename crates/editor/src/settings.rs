//! Small host-owned layout preferences. Plugin settings can use separate schemas.
use serde::{Deserialize, Serialize};
use std::{io::Write, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub status_bar: bool,
    pub activity_bar: bool,
    pub search_backend: String,
    // Preserve settings written by newer versions when changing the shell.
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}
impl Default for Preferences {
    fn default() -> Self {
        Self {
            status_bar: true,
            activity_bar: false,
            search_backend: velk_search::DEFAULT_BACKEND.into(),
            extra: Default::default(),
        }
    }
}

#[derive(Default)]
pub struct Store {
    pub preferences: Preferences,
    pub path: Option<PathBuf>,
    pub error: Option<String>,
}
impl Store {
    pub fn load() -> Self {
        let path = settings_path();
        match path {
            Some(path) => Self::read(path),
            None => Self {
                error: Some("Settings location unavailable; changes apply to this window.".into()),
                ..Self::default()
            },
        }
    }
    fn read(path: PathBuf) -> Self {
        let result = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| e.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Preferences::default()),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(preferences) => Self {
                preferences,
                path: Some(path),
                error: None,
            },
            Err(error) => Self {
                // Do not replace an unreadable or malformed configuration.
                error: Some(format!(
                    "Cannot read {}: {error}. Changes apply to this window until the file is fixed and Velk restarts.",
                    path.display()
                )),
                ..Self::default()
            },
        }
    }
}
fn settings_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("VELK_SETTINGS_PATH") {
        return Some(PathBuf::from(path));
    }
    #[cfg(target_os = "macos")]
    return std::env::var_os("HOME")
        .map(|p| PathBuf::from(p).join("Library/Application Support/Velk/settings.json"));
    #[cfg(not(target_os = "macos"))]
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".config")))
        .map(|p| p.join("velk/settings.json"))
}

pub fn save(path: &std::path::Path, preferences: &Preferences) -> Result<(), String> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    serde_json::to_writer_pretty(&mut file, preferences).map_err(|e| e.to_string())?;
    file.write_all(b"\n").map_err(|e| e.to_string())?;
    file.as_file().sync_all().map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preferences_default_merge_roundtrip_and_preserve_unknown_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/settings.json");
        assert_eq!(
            Store::read(path.clone()).preferences,
            Preferences::default()
        );
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, r#"{"activity_bar":true,"future":{"value":42}}"#).unwrap();
        let mut store = Store::read(path.clone());
        assert!(store.preferences.status_bar && store.preferences.activity_bar);
        store.preferences.status_bar = false;
        save(&path, &store.preferences).unwrap();
        assert_eq!(Store::read(path.clone()).preferences, store.preferences);
        assert_eq!(store.preferences.extra["future"]["value"], 42);
        std::fs::write(&path, "{broken").unwrap();
        let broken = Store::read(path.clone());
        assert!(broken.path.is_none() && broken.error.is_some());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{broken");
    }
}
