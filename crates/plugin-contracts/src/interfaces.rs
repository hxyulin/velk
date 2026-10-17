//! Distribution-level service contracts and provider bindings.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
pub type Bindings = BTreeMap<String, String>;
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interfaces {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implements: Vec<Implementation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<Requirement>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Implementation {
    pub interface: String,
    pub title: String,
    pub hook: String,
    pub version: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Requirement {
    pub interface: String,
    pub version: u32,
    pub default_provider: String,
}
impl Interfaces {
    pub fn validate(&self) -> Result<(), String> {
        if self.implements.len() > 32 || self.requires.len() > 32 {
            return Err("too many plugin interfaces".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for implementation in &self.implements {
            if crate::parse_plugin_id(&implementation.interface).is_none()
                || crate::parse_plugin_id(&implementation.hook).is_none()
                || implementation.version != 1
                || implementation.title.trim().is_empty()
                || implementation.title.len() > 128
                || implementation.title.chars().any(char::is_control)
                || !ids.insert(&implementation.interface)
            {
                return Err("invalid or duplicate interface implementation".into());
            }
        }
        ids.clear();
        for requirement in &self.requires {
            if crate::parse_plugin_id(&requirement.interface).is_none()
                || crate::parse_plugin_id(&requirement.default_provider).is_none()
                || requirement.version != 1
                || !ids.insert(&requirement.interface)
            {
                return Err("invalid or duplicate interface requirement".into());
            }
        }
        Ok(())
    }
}
