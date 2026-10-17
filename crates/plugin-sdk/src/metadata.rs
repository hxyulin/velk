use crate::{Capability, PluginMetadata};

pub(crate) fn of<P: crate::Plugin>() -> PluginMetadata {
    PluginMetadata {
        file_types: P::file_types(),
        commands: P::commands(),
        contributions: P::contributions(),
        name: P::NAME.into(),
        readme: P::README.into(),
        settings: P::settings(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapabilityDeclaration;
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
        let metadata = of::<Example>();
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
            let mut metadata = of::<Example>();
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
            let mut metadata = of::<Example>();
            metadata.capabilities[0].id = id.into();
            assert!(metadata.validate().is_err());
        }
        let mut metadata = of::<Example>();
        metadata.capabilities.push(metadata.capabilities[0].clone());
        assert!(metadata.validate().is_err());
        metadata = of::<Example>();
        metadata.capabilities[0].reason.clear();
        assert!(metadata.validate().is_err());
        metadata = of::<Example>();
        metadata.readme = "a".repeat(256 * 1024 + 1);
        assert!(metadata.validate().is_err());
    }
}
