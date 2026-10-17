//! Completion providers contribute normal commands to this topic. They have no
//! runtime dependency on a particular reducer or popup implementation.
pub const TOPIC: &str = "velk.completion.source";
pub const REDUCE: &str = "velk.completion.reduce";
pub const COMMAND: &str = "velk.completion.complete";
pub use velk_commands::contributions::{Collection, Contribution};

pub const AUTOMATIC: &str = "velk.completion.automatic";
pub use velk_commands::completion::{Documentation, Item};

/// Explicit requests use no input. Automatic requests carry the committed text;
/// language providers decide whether their current capabilities support it.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Invocation {
    Typing,
    Character { character: String },
}
impl Invocation {
    pub fn decode(input: Option<&str>) -> Result<Option<Self>, String> {
        input
            .map(|input| serde_json::from_str(input).map_err(|e| e.to_string()))
            .transpose()
    }
}

/// Activation policy advertised by a completion contributor. Dynamic character
/// sources must check the current provider capabilities before sending requests.
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Triggers {
    pub typing: bool,
    pub characters: Vec<String>,
    pub dynamic_characters: bool,
}
impl Triggers {
    pub fn metadata(self) -> serde_json::Value {
        serde_json::to_value(self).expect("completion trigger metadata is serializable")
    }
}
