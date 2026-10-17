use crate::{Manifest, PluginSpec};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallReason {
    #[default]
    Explicit,
    Bundled,
    Dependency,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyRow {
    pub id: String,
    pub depth: usize,
    /// Shared subgraphs are shown once, with references at subsequent edges.
    pub reference: bool,
}
impl Manifest {
    /// Validate all edges and return dependencies before their consumers.
    pub fn dependency_order(&self) -> Result<Vec<String>, String> {
        let specs: BTreeMap<_, _> = self.plugins.iter().map(|p| (p.id.as_str(), p)).collect();
        fn visit<'a>(
            id: &'a str,
            specs: &BTreeMap<&'a str, &'a PluginSpec>,
            active: &mut BTreeSet<&'a str>,
            done: &mut BTreeSet<&'a str>,
            order: &mut Vec<String>,
        ) -> Result<(), String> {
            if done.contains(id) {
                return Ok(());
            }
            if !active.insert(id) {
                return Err(format!("plugin dependency cycle at {id}"));
            }
            let spec = specs
                .get(id)
                .ok_or_else(|| format!("missing plugin dependency: {id}"))?;
            let mut unique = BTreeSet::new();
            for dependency in &spec.dependencies {
                if !unique.insert(dependency) {
                    return Err(format!("duplicate dependency in {id}: {dependency}"));
                }
                visit(dependency, specs, active, done, order)?;
            }
            active.remove(id);
            done.insert(id);
            order.push(id.into());
            Ok(())
        }
        let mut order = Vec::new();
        let mut active = BTreeSet::new();
        let mut done = BTreeSet::new();
        for id in specs.keys() {
            visit(id, &specs, &mut active, &mut done, &mut order)?;
        }
        Ok(order)
    }
    pub fn dependency_closure(&self, roots: &[String]) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        let mut pending = roots.to_vec();
        while let Some(id) = pending.pop() {
            if result.insert(id.clone())
                && let Some(spec) = self.plugins.iter().find(|p| p.id == id)
            {
                pending.extend(spec.dependencies.clone());
            }
        }
        result
    }
    /// Dependency-only entries unreachable from any installed root. This is a
    /// plan only: shared native images must never be removed per logical member.
    pub fn orphaned_dependencies(&self) -> Vec<String> {
        let roots = self
            .plugins
            .iter()
            .filter(|p| p.install_reason != InstallReason::Dependency)
            .map(|p| p.id.clone())
            .collect::<Vec<_>>();
        let retained = self.dependency_closure(&roots);
        self.plugins
            .iter()
            .filter(|p| p.install_reason == InstallReason::Dependency && !retained.contains(&p.id))
            .map(|p| p.id.clone())
            .collect()
    }
    pub fn dependency_tree(&self) -> Vec<DependencyRow> {
        let dependencies: BTreeSet<_> = self
            .plugins
            .iter()
            .flat_map(|p| p.dependencies.iter())
            .collect();
        let mut roots: Vec<_> = self
            .plugins
            .iter()
            .filter(|p| {
                p.install_reason != InstallReason::Dependency || !dependencies.contains(&p.id)
            })
            .map(|p| p.id.clone())
            .collect();
        roots.sort();
        fn append(
            id: &str,
            depth: usize,
            manifest: &Manifest,
            seen: &mut BTreeSet<String>,
            rows: &mut Vec<DependencyRow>,
        ) {
            let reference = !seen.insert(id.into());
            rows.push(DependencyRow {
                id: id.into(),
                depth,
                reference,
            });
            if !reference && let Some(spec) = manifest.plugins.iter().find(|p| p.id == id) {
                for dependency in &spec.dependencies {
                    append(dependency, depth + 1, manifest, seen, rows);
                }
            }
        }
        let mut rows = Vec::new();
        let mut seen = BTreeSet::new();
        for root in roots {
            append(&root, 0, self, &mut seen, &mut rows);
        }
        // Also display malformed inventories without recursing forever.
        for spec in &self.plugins {
            if !seen.contains(&spec.id) {
                append(&spec.id, 0, self, &mut seen, &mut rows);
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn spec(id: &str, dependencies: &[&str], install_reason: InstallReason) -> PluginSpec {
        PluginSpec {
            id: id.into(),
            library: "test.so".into(),
            digest: "a".repeat(64),
            pack: true,
            dependencies: dependencies.iter().map(|s| (*s).into()).collect(),
            install_reason,
        }
    }
    #[test]
    fn shared_dependencies_preserve_manual_install_intent_and_have_finite_tree() {
        let mut manifest = Manifest {
            plugins: vec![
                spec("test.frontend", &["test.backend"], InstallReason::Explicit),
                spec("test.other", &["test.backend"], InstallReason::Explicit),
                spec("test.backend", &["test.engine"], InstallReason::Dependency),
                spec("test.engine", &[], InstallReason::Explicit),
            ],
        };
        manifest.validate().unwrap();
        let order = manifest.dependency_order().unwrap();
        assert!(
            order.iter().position(|p| p == "test.backend")
                < order.iter().position(|p| p == "test.frontend")
        );
        assert!(manifest.orphaned_dependencies().is_empty());
        assert!(
            manifest
                .dependency_tree()
                .iter()
                .any(|r| r.id == "test.backend" && r.reference)
        );
        manifest.plugins.retain(|p| p.id != "test.frontend");
        assert!(manifest.orphaned_dependencies().is_empty());
        manifest.plugins.retain(|p| p.id != "test.other");
        assert_eq!(manifest.orphaned_dependencies(), ["test.backend"]);
        assert!(
            !manifest
                .orphaned_dependencies()
                .contains(&"test.engine".into())
        );
    }
    #[test]
    fn missing_duplicate_and_cyclic_edges_are_rejected() {
        for edges in [
            vec!["test.missing"],
            vec!["test.backend", "test.backend"],
            vec!["test.frontend"],
        ] {
            let manifest = Manifest {
                plugins: vec![
                    spec("test.frontend", &edges, InstallReason::Explicit),
                    spec("test.backend", &[], InstallReason::Dependency),
                ],
            };
            assert!(manifest.validate().is_err());
        }
    }
}
