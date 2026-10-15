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
    pub name: String,
    pub readme: String,
    pub capabilities: Vec<Capability>,
}
impl PluginMetadata {
    pub(crate) fn of<P: crate::Plugin>() -> Self {
        Self {
            name: P::NAME.into(),
            readme: P::README.into(),
            capabilities: P::CAPABILITIES
                .iter()
                .map(|c| Capability {
                    id: c.id.into(),
                    reason: c.reason.into(),
                    optional: c.optional,
                })
                .collect(),
        }
    }
    pub fn validate(&self) -> Result<(), String> {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct Example;
    impl crate::Plugin for Example {
        const ID: &'static str = "test.metadata";
        const VERSION: &'static str = "1";
        const NAME: &'static str = "Example 世界";
        const README: &'static str = "# Example\n\n**Styled** `documentation`";
        const CAPABILITIES: &'static [CapabilityDeclaration] = &[
            CapabilityDeclaration::required("editor.buffer.read", "Read the current document."),
            CapabilityDeclaration::optional(
                "network.http",
                "Fetch extra documentation when available.",
            ),
        ];
        fn describe(&mut self) -> String {
            "Short summary".into()
        }
    }
    #[test]
    fn metadata_round_trips_unicode_markdown_and_required_and_optional_access() {
        let metadata = PluginMetadata::of::<Example>();
        metadata.validate().unwrap();
        let copy: PluginMetadata =
            serde_json::from_slice(&serde_json::to_vec(&metadata).unwrap()).unwrap();
        assert_eq!(copy, metadata);
        assert_eq!(copy.name, "Example 世界");
        assert!(copy.readme.contains("**Styled**"));
        assert!(!copy.capabilities[0].optional);
        assert!(copy.capabilities[1].optional);
    }
    #[test]
    fn rejects_invalid_names_duplicate_capabilities_and_oversized_documents() {
        for name in ["", "  ", "bad\nname", &"a".repeat(257)] {
            let mut metadata = PluginMetadata::of::<Example>();
            metadata.name = name.into();
            assert!(metadata.validate().is_err());
        }
        for id in [
            "",
            "filesystem",
            "filesystem..read",
            "Filesystem.read",
            "filesystem.read/write",
        ] {
            let mut metadata = PluginMetadata::of::<Example>();
            metadata.capabilities[0].id = id.into();
            assert!(metadata.validate().is_err());
        }
        let mut metadata = PluginMetadata::of::<Example>();
        metadata.capabilities.push(metadata.capabilities[0].clone());
        assert!(metadata.validate().is_err());
        metadata = PluginMetadata::of::<Example>();
        metadata.capabilities[0].reason.clear();
        assert!(metadata.validate().is_err());
        metadata = PluginMetadata::of::<Example>();
        metadata.readme = "a".repeat(256 * 1024 + 1);
        assert!(metadata.validate().is_err());
    }
}
