//! Small host-owned layout preferences. Plugin settings can use separate schemas.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{io::Write, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    pub status_bar: bool,
    pub activity_bar: bool,
    pub search_backend: String,
    /// Plugin IDs the user turned off; every other manifest plugin loads.
    pub disabled_plugins: Vec<String>,
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
            disabled_plugins: Vec::new(),
            extra: Default::default(),
        }
    }
}

// Settings are addressed by their settings.json key, so registry rows work the
// same for typed fields and keys preserved in `extra`.
impl Preferences {
    pub fn get(&self, key: &str) -> Option<Value> {
        match serde_json::to_value(self) {
            Ok(Value::Object(mut map)) => map.remove(key),
            _ => None,
        }
    }
    /// `None` removes the key, restoring a field's default.
    pub fn set(&mut self, key: &str, value: Option<Value>) -> Result<(), String> {
        let Ok(Value::Object(mut map)) = serde_json::to_value(&*self) else {
            return Err("settings are not an object".into());
        };
        match value {
            Some(value) => map.insert(key.into(), value),
            None => map.remove(key),
        };
        *self = serde_json::from_value(Value::Object(map)).map_err(|e| e.to_string())?;
        Ok(())
    }
    pub fn is_modified(&self, key: &str) -> bool {
        self.get(key) != Preferences::default().get(key)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Category {
    Editor,
    Appearance,
    Layout,
    Keymap,
    Files,
    Search,
}
impl Category {
    pub const ALL: [Self; 6] = [
        Self::Editor,
        Self::Appearance,
        Self::Layout,
        Self::Keymap,
        Self::Files,
        Self::Search,
    ];
    pub fn title(self) -> &'static str {
        match self {
            Self::Editor => "Editor",
            Self::Appearance => "Appearance",
            Self::Layout => "Layout",
            Self::Keymap => "Keymap",
            Self::Files => "Files",
            Self::Search => "Search",
        }
    }
}
pub enum Control {
    Toggle,
    /// Options as (value, caption), computed from the loaded plugins.
    Choice(fn(&[velk_plugin_host::PluginInfo]) -> Vec<(String, String)>),
    // Rendered by the settings page; no registered setting uses them yet.
    #[allow(dead_code)]
    Text,
    #[allow(dead_code)]
    Number,
}
/// One settings page row. Static for now; plugin-contributed rows can later be
/// appended from a plugin hook with the same shape (their keys live in `extra`).
pub struct Setting {
    pub key: &'static str,
    pub title: &'static str,
    pub description: &'static str,
    pub category: Category,
    pub control: Control,
}
impl Setting {
    /// Every whitespace-separated term must occur in the title, description or key.
    pub fn matches(&self, query: &str) -> bool {
        let haystack = format!("{} {} {}", self.title, self.description, self.key).to_lowercase();
        query
            .split_whitespace()
            .all(|term| haystack.contains(&term.to_lowercase()))
    }
}
fn layouts(_: &[velk_plugin_host::PluginInfo]) -> Vec<(String, String)> {
    ["quiet", "rail", "tiled"]
        .into_iter()
        .map(|v| (v.into(), format!("{}{}", v[..1].to_uppercase(), &v[1..])))
        .collect()
}
fn search_backends(plugins: &[velk_plugin_host::PluginInfo]) -> Vec<(String, String)> {
    let mut ids = vec![velk_search::DEFAULT_BACKEND.to_owned()];
    for plugin in plugins {
        if plugin.hooks.iter().any(|h| h == velk_search::HOOK) && !ids.contains(&plugin.id) {
            ids.push(plugin.id.clone());
        }
    }
    ids.into_iter().map(|id| (id.clone(), id)).collect()
}
pub static REGISTRY: &[Setting] = &[
    Setting {
        // Stored in `extra` until Preferences gains a typed `layout` field.
        key: "layout",
        title: "Layout preset",
        description: "Quiet keeps panels hidden until you open them. Rail adds an icon bar and sidebar.",
        category: Category::Layout,
        control: Control::Choice(layouts),
    },
    Setting {
        key: "status_bar",
        title: "Status bar",
        description: "Buffer state, language server status and launchers.",
        category: Category::Layout,
        control: Control::Toggle,
    },
    Setting {
        key: "activity_bar",
        title: "Activity bar",
        description: "A column of launchers on the left edge of the window.",
        category: Category::Layout,
        control: Control::Toggle,
    },
    Setting {
        key: "search_backend",
        title: "Search backend",
        description: "Plugin that finds matches in the current buffer.",
        category: Category::Search,
        control: Control::Choice(search_backends),
    },
];

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
    #[test]
    fn registry_filters_by_title_description_and_key() {
        let keys = |query: &str| -> Vec<_> {
            REGISTRY
                .iter()
                .filter(|s| s.matches(query))
                .map(|s| s.key)
                .collect()
        };
        assert_eq!(keys("").len(), REGISTRY.len());
        assert_eq!(keys("STATUS"), ["status_bar"]);
        assert_eq!(keys("search_backend"), ["search_backend"]);
        assert_eq!(keys("icon sidebar"), ["layout"]);
        assert!(keys("nothing-matches-this").is_empty());
    }
    #[test]
    fn settings_by_key_track_modification_reset_and_reject_bad_values() {
        let mut preferences = Preferences::default();
        assert!(REGISTRY.iter().all(|s| !preferences.is_modified(s.key)));
        preferences
            .set("status_bar", Some(Value::Bool(false)))
            .unwrap();
        assert!(!preferences.status_bar && preferences.is_modified("status_bar"));
        preferences.set("layout", Some("rail".into())).unwrap();
        assert_eq!(preferences.extra["layout"], "rail");
        assert!(preferences.is_modified("layout"));
        assert!(preferences.set("status_bar", Some("yes".into())).is_err());
        assert!(!preferences.status_bar);
        for key in ["status_bar", "layout"] {
            preferences.set(key, None).unwrap();
            assert!(!preferences.is_modified(key));
        }
        assert_eq!(preferences, Preferences::default());
    }
    #[test]
    fn disabled_plugins_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        let preferences = Preferences {
            disabled_plugins: vec!["velk.terminal".into()],
            ..Preferences::default()
        };
        save(&path, &preferences).unwrap();
        assert_eq!(Store::read(path).preferences, preferences);
        let legacy: Preferences = serde_json::from_str(r#"{"status_bar":false}"#).unwrap();
        assert!(legacy.disabled_plugins.is_empty());
    }
}
