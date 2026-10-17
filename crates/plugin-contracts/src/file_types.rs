//! Plugin-declared file identities. Associations do not select an implementation.
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileType {
    pub id: String,
    pub name: String,
    pub extensions: Vec<String>,
    pub filenames: Vec<String>,
}
impl FileType {
    pub fn validate(&self) -> Result<(), String> {
        if self.id.is_empty()
            || self.id.len() > 64
            || !self
                .id
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"_-".contains(&b))
            || self.name.trim().is_empty()
            || self.name.len() > 128
            || self.name.chars().any(char::is_control)
            || self.extensions.len() > 64
            || self.filenames.len() > 64
            || self.extensions.iter().any(|e| {
                e.is_empty()
                    || e.len() > 64
                    || !e
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
            || self.filenames.iter().any(|f| {
                f.is_empty()
                    || f.len() > 256
                    || f.contains(['/', '\\'])
                    || f.chars().any(char::is_control)
            })
        {
            return Err("invalid file type declaration".into());
        }
        Ok(())
    }
}
/// Exact filenames precede extensions. Conflicting declarations require a user override.
/// Overrides accept exact filenames or `*.extension`, and must name a loaded type.
pub fn resolve<'a>(
    path: &Path,
    types: impl Iterator<Item = &'a FileType>,
    overrides: &BTreeMap<String, String>,
) -> Option<FileType> {
    let filename = path.file_name()?.to_str()?;
    let extension = path
        .extension()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let types: Vec<_> = types.collect();
    if let Some(id) = overrides
        .get(filename)
        .or_else(|| overrides.get(&format!("*.{extension}")))
    {
        return types.iter().find(|t| &t.id == id).map(|t| (*t).clone());
    }
    let named: Vec<_> = types
        .iter()
        .copied()
        .filter(|t| t.filenames.iter().any(|f| f == filename))
        .collect();
    let candidates: Vec<_> = if named.is_empty() {
        types
            .iter()
            .copied()
            .filter(|t| {
                t.extensions
                    .iter()
                    .any(|e| e.eq_ignore_ascii_case(&extension))
            })
            .collect()
    } else {
        named
    };
    let first = candidates.first()?;
    candidates
        .iter()
        .all(|t| t.id == first.id)
        .then(|| (*first).clone())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn kind(id: &str) -> FileType {
        FileType {
            id: id.into(),
            name: id.into(),
            extensions: vec!["rs".into()],
            filenames: vec![],
        }
    }
    #[test]
    fn associations_are_extensible_and_conflicts_need_explicit_selection() {
        let rust = kind("rust");
        let mut other = kind("other");
        assert_eq!(
            resolve(Path::new("MAIN.RS"), [&rust].into_iter(), &BTreeMap::new())
                .unwrap()
                .id,
            "rust"
        );
        assert!(
            resolve(
                Path::new("main.rs"),
                [&rust, &other].into_iter(),
                &BTreeMap::new()
            )
            .is_none()
        );
        let overrides = BTreeMap::from([("*.rs".into(), "other".into())]);
        assert_eq!(
            resolve(
                Path::new("main.rs"),
                [&rust, &other].into_iter(),
                &overrides
            )
            .unwrap()
            .id,
            "other"
        );
        other.filenames.push("special.rs".into());
        assert_eq!(
            resolve(
                Path::new("special.rs"),
                [&rust, &other].into_iter(),
                &BTreeMap::new()
            )
            .unwrap()
            .id,
            "other"
        );
        assert!(resolve(Path::new("main.rs"), [&rust].into_iter(), &overrides).is_none());
        other.extensions = vec!["py".into()];
        assert_eq!(
            resolve(Path::new("main.py"), [&other].into_iter(), &BTreeMap::new())
                .unwrap()
                .id,
            "other"
        );
    }
}
