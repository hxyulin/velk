/// Borrowed interpretation of author.[namespace.]name. Namespace may contain dots;
/// nesting has no implicit dependency, permissions, or ownership semantics.
#[derive(Debug, PartialEq, Eq)]
pub struct PluginId<'a> {
    pub author: &'a str,
    pub namespace: Option<&'a str>,
    pub name: &'a str,
}
pub fn parse_plugin_id(id: &str) -> Option<PluginId<'_>> {
    if id.len() >= 64 {
        return None;
    }
    if id.split('.').any(|s| {
        s.is_empty()
            || !s.as_bytes()[0].is_ascii_lowercase()
            || !s
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
    }) {
        return None;
    }
    let first = id.find('.')?;
    let last = id.rfind('.')?;
    Some(PluginId {
        author: &id[..first],
        namespace: (first != last).then(|| &id[first + 1..last]),
        name: &id[last + 1..],
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_flat_and_nested_namespaces_and_rejects_ambiguous_ids() {
        assert_eq!(
            parse_plugin_id("velk.markdown"),
            Some(PluginId {
                author: "velk",
                namespace: None,
                name: "markdown"
            })
        );
        assert_eq!(
            parse_plugin_id("velk.language-server.rust-analyzer"),
            Some(PluginId {
                author: "velk",
                namespace: Some("language-server"),
                name: "rust-analyzer"
            })
        );
        assert_eq!(
            parse_plugin_id("author.tools.language.server.rust"),
            Some(PluginId {
                author: "author",
                namespace: Some("tools.language.server"),
                name: "rust"
            })
        );
        for id in [
            "",
            "velk",
            ".velk.rust",
            "velk.rust.",
            "velk..rust",
            "Velk.rust",
            "velk.世界",
            "velk.a/b",
            "velk.-rust",
            "velk.1rust",
        ] {
            assert!(parse_plugin_id(id).is_none(), "accepted {id}");
        }
        assert!(parse_plugin_id(&format!("velk.{}", "a".repeat(59))).is_none());
    }
}
