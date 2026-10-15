//! Small host-owned layout preferences. Plugin settings can use separate schemas.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, io::Write, path::PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Preferences {
    /// A built-in preset name or a key of `layouts`; unset means quiet.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub layout: Option<String>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub layouts: BTreeMap<String, LayoutOverride>,
    // Settings from before layout presets. They still apply on top of the
    // preset until the user picks a layout, which drops them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_bar: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity_bar: Option<bool>,
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
            layout: None,
            layouts: BTreeMap::new(),
            status_bar: None,
            activity_bar: None,
            search_backend: velk_search::DEFAULT_BACKEND.into(),
            disabled_plugins: Vec::new(),
            extra: Default::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BufferTabs {
    TitleBar,
    Editor,
    Hidden,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PanelMode {
    Docked,
    Tiled,
}
/// Where chrome goes. Plugins only say whether a panel is a sidebar or a bottom
/// panel; the layout decides how those roles are shown.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    pub buffer_tabs: BufferTabs,
    pub rail: bool,
    pub status_line: bool,
    pub panels: PanelMode,
    /// Whether panels open when a window starts or the layout is picked.
    pub show_panels: bool,
}
impl Layout {
    pub const QUIET: Self = Self {
        buffer_tabs: BufferTabs::TitleBar,
        rail: false,
        status_line: true,
        panels: PanelMode::Docked,
        show_panels: false,
    };
    pub fn preset(name: &str) -> Option<Self> {
        Some(match name {
            "quiet" => Self::QUIET,
            "rail" => Self {
                buffer_tabs: BufferTabs::Editor,
                rail: true,
                ..Self::QUIET
            },
            "tiled" => Self {
                panels: PanelMode::Tiled,
                show_panels: true,
                ..Self::QUIET
            },
            _ => return None,
        })
    }
}
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LayoutOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extends: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buffer_tabs: Option<BufferTabs>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rail: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_line: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub panels: Option<PanelMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub show_panels: Option<bool>,
}
impl LayoutOverride {
    fn apply(&self, base: Layout) -> Layout {
        Layout {
            buffer_tabs: self.buffer_tabs.unwrap_or(base.buffer_tabs),
            rail: self.rail.unwrap_or(base.rail),
            status_line: self.status_line.unwrap_or(base.status_line),
            panels: self.panels.unwrap_or(base.panels),
            show_panels: self.show_panels.unwrap_or(base.show_panels),
        }
    }
}
impl Preferences {
    pub fn layout_name(&self) -> &str {
        match (&self.layout, self.activity_bar) {
            (Some(name), _) => name,
            (None, Some(true)) => "rail",
            _ => "quiet",
        }
    }
    /// The active layout, or quiet with an error for an unknown or cyclic name.
    pub fn resolve_layout(&self) -> (Layout, Option<String>) {
        let (mut layout, error) = match self.lookup(self.layout_name(), 0) {
            Ok(layout) => (layout, None),
            Err(error) => (
                Layout::QUIET,
                Some(format!("{error}; using the quiet layout.")),
            ),
        };
        if let Some(rail) = self.activity_bar {
            layout.rail = rail;
        }
        if let Some(status) = self.status_bar {
            layout.status_line = status;
        }
        (layout, error)
    }
    fn lookup(&self, name: &str, depth: u8) -> Result<Layout, String> {
        let preset = Layout::preset(name);
        let Some(custom) = self.layouts.get(name) else {
            return preset.ok_or_else(|| format!("Unknown layout \"{name}\""));
        };
        if depth > 8 {
            return Err(format!("Layout \"{name}\" extends itself"));
        }
        // A custom entry named after a preset overrides that preset.
        let base = match &custom.extends {
            Some(parent) if parent != name => self.lookup(parent, depth + 1)?,
            _ => preset.unwrap_or(Layout::QUIET),
        };
        Ok(custom.apply(base))
    }
    /// Picks a layout and drops the pre-preset keys so they stop overriding it.
    pub fn set_layout(&mut self, name: &str) {
        self.layout = Some(name.into());
        self.status_bar = None;
        self.activity_bar = None;
    }
    /// Changes the active layout through its user override.
    pub fn override_layout(&mut self, f: impl FnOnce(&mut LayoutOverride)) {
        let (rail, status_line) = (self.activity_bar, self.status_bar);
        let name = self.layout_name().to_owned();
        self.set_layout(&name);
        let custom = self.layouts.entry(name).or_default();
        // Migrate the legacy keys' effect into the override.
        custom.rail = rail.or(custom.rail);
        custom.status_line = status_line.or(custom.status_line);
        f(custom);
    }
}

// Settings are addressed by their settings.json key, so registry rows work the
// same for typed fields and keys preserved in `extra`. The layout rows are the
// exception: they read and edit the active layout rather than a key of their own.
impl Preferences {
    pub fn get(&self, key: &str) -> Option<Value> {
        let layout = self.resolve_layout().0;
        match key {
            "layout" => return Some(self.layout_name().into()),
            "status_line" => return Some(layout.status_line.into()),
            "rail" => return Some(layout.rail.into()),
            _ => {}
        }
        match serde_json::to_value(self) {
            Ok(Value::Object(mut map)) => map.remove(key),
            _ => None,
        }
    }
    /// `None` removes the key, restoring a field's default.
    pub fn set(&mut self, key: &str, value: Option<Value>) -> Result<(), String> {
        match (key, value) {
            ("layout", Some(Value::String(name))) => self.set_layout(&name),
            ("layout", None) => {
                self.set_layout("quiet");
                self.layout = None;
            }
            ("status_line" | "rail", value @ (Some(Value::Bool(_)) | None)) => {
                // A value equal to the layout's own is not an override.
                self.override_layout(|o| *layout_flag(o, key) = None);
                let base = self.get(key).and_then(|v| v.as_bool());
                let value = value.and_then(|v| v.as_bool()).filter(|v| Some(*v) != base);
                self.override_layout(|o| *layout_flag(o, key) = value);
            }
            ("layout" | "status_line" | "rail", Some(_)) => {
                return Err(format!("invalid value for {key}"));
            }
            (_, value) => return self.set_generic(key, value),
        }
        Ok(())
    }
    fn set_generic(&mut self, key: &str, value: Option<Value>) -> Result<(), String> {
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
        let custom = self.layouts.get(self.layout_name());
        match key {
            "layout" => self.layout_name() != "quiet",
            "status_line" => {
                self.status_bar.is_some() || custom.is_some_and(|c| c.status_line.is_some())
            }
            "rail" => self.activity_bar.is_some() || custom.is_some_and(|c| c.rail.is_some()),
            _ => self.get(key) != Preferences::default().get(key),
        }
    }
}

fn layout_flag<'a>(o: &'a mut LayoutOverride, key: &str) -> &'a mut Option<bool> {
    if key == "rail" {
        &mut o.rail
    } else {
        &mut o.status_line
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
        key: "layout",
        title: "Layout preset",
        description: "Quiet keeps panels hidden until you open them. Rail adds an icon bar and sidebar. Tiled shows panels as splits.",
        category: Category::Layout,
        control: Control::Choice(layouts),
    },
    Setting {
        key: "status_line",
        title: "Status line",
        description: "File, diagnostics, language and cursor position. Applies to the current layout.",
        category: Category::Layout,
        control: Control::Toggle,
    },
    Setting {
        key: "rail",
        title: "Icon rail",
        description: "Sidebar panels, plugins and settings on the left edge. Applies to the current layout.",
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
        assert_eq!(store.preferences.layout_name(), "rail");
        store
            .preferences
            .override_layout(|o| o.status_line = Some(false));
        save(&path, &store.preferences).unwrap();
        assert_eq!(Store::read(path.clone()).preferences, store.preferences);
        assert_eq!(store.preferences.extra["future"]["value"], 42);
        std::fs::write(&path, "{broken").unwrap();
        let broken = Store::read(path.clone());
        assert!(broken.path.is_none() && broken.error.is_some());
        assert_eq!(std::fs::read_to_string(path).unwrap(), "{broken");
    }
    fn parse(json: &str) -> Preferences {
        serde_json::from_str(json).unwrap()
    }
    #[test]
    fn layout_presets_overrides_and_unknown_fallback() {
        assert_eq!(
            Preferences::default().resolve_layout(),
            (Layout::QUIET, None)
        );
        let rail = parse(r#"{"layout":"rail"}"#).resolve_layout().0;
        assert!(rail.rail && rail.buffer_tabs == BufferTabs::Editor);
        let tiled = parse(r#"{"layout":"tiled"}"#).resolve_layout().0;
        assert_eq!(tiled.panels, PanelMode::Tiled);
        let mine = parse(
            r#"{"layout":"mine","layouts":{
                "mine":{"extends":"rail","status_line":false,"buffer_tabs":"title_bar"},
                "rail":{"rail":false}}}"#,
        );
        let (layout, error) = mine.resolve_layout();
        assert!(error.is_none());
        // "rail" itself is overridden, and "mine" inherits that override.
        assert!(!layout.rail && !layout.status_line);
        assert_eq!(layout.buffer_tabs, BufferTabs::TitleBar);
        for json in [
            r#"{"layout":"nope"}"#,
            r#"{"layout":"a","layouts":{"a":{"extends":"b"},"b":{"extends":"a"}}}"#,
        ] {
            let (layout, error) = parse(json).resolve_layout();
            assert_eq!(layout, Layout::QUIET);
            assert!(error.unwrap().contains("quiet"));
        }
    }
    #[test]
    fn legacy_chrome_keys_map_onto_presets_until_a_layout_is_picked() {
        let mut legacy = parse(r#"{"activity_bar":true,"status_bar":false}"#);
        let layout = legacy.resolve_layout().0;
        assert!(layout.rail && !layout.status_line);
        assert_eq!(layout.buffer_tabs, BufferTabs::Editor);
        let quiet = parse(r#"{"activity_bar":false,"status_bar":false}"#)
            .resolve_layout()
            .0;
        assert!(!quiet.rail && !quiet.status_line);
        legacy.set_layout("quiet");
        assert_eq!(legacy.resolve_layout().0, Layout::QUIET);
        let json = serde_json::to_value(&legacy).unwrap();
        assert!(json.get("activity_bar").is_none() && json["layout"] == "quiet");
        legacy.override_layout(|o| o.rail = Some(true));
        assert!(legacy.resolve_layout().0.rail);
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
        assert_eq!(keys("STATUS"), ["status_line"]);
        assert_eq!(keys("search_backend"), ["search_backend"]);
        assert_eq!(keys("splits"), ["layout"]);
        assert!(keys("nothing-matches-this").is_empty());
    }
    #[test]
    fn settings_by_key_track_modification_reset_and_reject_bad_values() {
        let mut preferences = Preferences::default();
        assert!(REGISTRY.iter().all(|s| !preferences.is_modified(s.key)));
        preferences.set("layout", Some("rail".into())).unwrap();
        assert_eq!(preferences.layout.as_deref(), Some("rail"));
        assert!(preferences.is_modified("layout"));
        // Rail's own icon rail is the preset, not a modification.
        assert!(!preferences.is_modified("rail"));
        preferences
            .set("status_line", Some(Value::Bool(false)))
            .unwrap();
        assert!(!preferences.resolve_layout().0.status_line);
        assert!(preferences.is_modified("status_line"));
        assert!(preferences.set("status_line", Some("yes".into())).is_err());
        assert!(
            preferences
                .set("search_backend", Some(Value::Bool(true)))
                .is_err()
        );
        for key in ["status_line", "layout"] {
            preferences.set(key, None).unwrap();
            assert!(!preferences.is_modified(key));
        }
        assert_eq!(preferences.resolve_layout().0, Layout::QUIET);
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
