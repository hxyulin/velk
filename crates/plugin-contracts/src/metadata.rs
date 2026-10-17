//! Owned metadata travels as bounded JSON through the standard request contract.
use serde::{Deserialize, Serialize};

/// An access requirement and its purpose. Optional access must be safe to omit:
/// a plugin should degrade gracefully when a future host denies that access.
/// Native plugins currently have ambient process access; declarations do not
/// grant access, enforce a sandbox, or configure enabled features.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CapabilityDeclaration {
    pub id: &'static str,
    pub reason: &'static str,
    pub optional: bool,
}
impl CapabilityDeclaration {
    pub const fn required(id: &'static str, reason: &'static str) -> Self {
        Self {
            id,
            reason,
            optional: false,
        }
    }
    pub const fn optional(id: &'static str, reason: &'static str) -> Self {
        Self {
            id,
            reason,
            optional: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capability {
    pub id: String,
    pub reason: String,
    pub optional: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginMetadata {
    pub commands: Vec<velk_commands::Declaration>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contributions: Vec<velk_commands::contributions::Contribution>,
    pub name: String,
    pub readme: String,
    pub capabilities: Vec<Capability>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub settings: Vec<crate::settings::Definition>,
}
impl PluginMetadata {
    pub fn validate(&self) -> Result<(), String> {
        crate::settings::validate_schema(&self.settings)?;
        if self.commands.len() > 128 {
            return Err("too many plugin commands".into());
        }
        if self.contributions.len() > 32 {
            return Err("too many plugin contributions".into());
        }
        let mut contributions = std::collections::BTreeSet::new();
        for contribution in &self.contributions {
            contribution.validate()?;
            if !contributions.insert((&contribution.topic, &contribution.command.id)) {
                return Err("duplicate plugin contribution".into());
            }
        }
        let mut commands = std::collections::BTreeSet::new();
        for command in &self.commands {
            command.validate()?;
            if !commands.insert(&command.id) {
                return Err("duplicate plugin command".into());
            }
        }
        if self.name.trim().is_empty()
            || self.name.len() > 256
            || self.name.chars().any(char::is_control)
        {
            return Err("invalid plugin display name".into());
        }
        if self.readme.len() > 256 * 1024
            || self.readme.contains('\0')
            || self.capabilities.len() > 64
        {
            return Err("plugin metadata exceeds limits".into());
        }
        let mut ids = std::collections::BTreeSet::new();
        for c in &self.capabilities {
            if c.id.len() > 128
                || !c.id.contains('.')
                || !c.id.split('.').all(|s| {
                    !s.is_empty()
                        && s.bytes().all(|b| {
                            b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b)
                        })
                })
                || !ids.insert(&c.id)
                || c.reason.trim().is_empty()
                || c.reason.len() > 1024
                || c.reason.chars().any(char::is_control)
            {
                return Err("invalid or duplicate plugin capability".into());
            }
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 512 * 1024 {
            return Err("encoded plugin metadata exceeds 512 KiB".into());
        }
        Ok(())
    }
}
