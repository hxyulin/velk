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
    /// Resolve interface selections to ordinary dependency edges before loading.
    pub fn resolve_interfaces(
        &self,
        bindings: &velk_plugin_contracts::interfaces::Bindings,
    ) -> Result<Self, String> {
        let mut result = self.clone();
        for (interface, provider) in bindings {
            if !self.plugins.iter().any(|p| {
                &p.id == provider
                    && p.interfaces
                        .implements
                        .iter()
                        .any(|i| &i.interface == interface)
            }) {
                return Err(format!("{provider} does not implement {interface}"));
            }
        }
        for plugin in &mut result.plugins {
            for requirement in &mut plugin.interfaces.requires {
                let provider = bindings
                    .get(&requirement.interface)
                    .unwrap_or(&requirement.default_provider);
                if provider == &plugin.id {
                    return Err("an interface cannot depend on its own implementation".into());
                }
                self.plugins
                    .iter()
                    .find(|p| &p.id == provider)
                    .and_then(|p| {
                        p.interfaces.implements.iter().find(|i| {
                            i.interface == requirement.interface && i.version == requirement.version
                        })
                    })
                    .ok_or_else(|| {
                        format!(
                            "{provider} does not implement {} version {}",
                            requirement.interface, requirement.version
                        )
                    })?;
                if !plugin.dependencies.contains(provider) {
                    plugin.dependencies.push(provider.clone());
                }
                requirement.default_provider = provider.clone();
            }
        }
        result.dependency_order()?;
        Ok(result)
    }
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
        let resolved = self
            .resolve_interfaces(&Default::default())
            .unwrap_or_else(|_| self.clone());
        let roots = resolved
            .plugins
            .iter()
            .filter(|p| p.install_reason != InstallReason::Dependency)
            .map(|p| p.id.clone())
            .collect::<Vec<_>>();
        let retained = resolved.dependency_closure(&roots);
        resolved
            .plugins
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
            runtime: Default::default(),
            interfaces: Default::default(),
            id: id.into(),
            library: "test.so".into(),
            digest: "a".repeat(64),
            pack: true,
            dependencies: dependencies.iter().map(|s| (*s).into()).collect(),
            install_reason,
        }
    }
    #[test]
    fn interface_selection_rebuilds_edges_and_rejects_missing_wrong_and_cyclic_providers() {
        use velk_plugin_contracts::interfaces::{Implementation, Requirement};
        let mut frontend = spec("test.frontend", &[], InstallReason::Explicit);
        frontend.interfaces.requires.push(Requirement {
            interface: "test.files".into(),
            version: 1,
            default_provider: "test.first".into(),
        });
        let mut first = spec("test.first", &[], InstallReason::Dependency);
        first.interfaces.implements.push(Implementation {
            interface: "test.files".into(),
            title: "Files".into(),
            hook: "test.query".into(),
            version: 1,
        });
        let mut second = first.clone();
        second.id = "test.second".into();
        let manifest = Manifest {
            plugins: vec![frontend, first, second],
        };
        let defaults = manifest.resolve_interfaces(&Default::default()).unwrap();
        assert_eq!(defaults.plugins[0].dependencies, ["test.first"]);
        let bindings =
            std::collections::BTreeMap::from([("test.files".into(), "test.second".into())]);
        let selected = manifest.resolve_interfaces(&bindings).unwrap();
        assert_eq!(selected.plugins[0].dependencies, ["test.second"]);
        assert_eq!(defaults.orphaned_dependencies(), ["test.second"]);
        assert_eq!(selected.orphaned_dependencies(), ["test.first"]);
        assert_eq!(
            selected.dependency_closure(&["test.frontend".into()]),
            std::collections::BTreeSet::from(["test.frontend".into(), "test.second".into()])
        );
        for provider in ["test.missing", "test.frontend"] {
            assert!(
                manifest
                    .resolve_interfaces(&std::collections::BTreeMap::from([(
                        "test.files".into(),
                        provider.into()
                    )]))
                    .is_err()
            );
        }
        let mut broken = manifest.clone();
        broken.plugins[2].interfaces.implements[0].version = 2;
        assert!(broken.resolve_interfaces(&bindings).is_err());
        let mut cyclic = manifest;
        cyclic.plugins[2].dependencies.push("test.frontend".into());
        assert!(cyclic.resolve_interfaces(&bindings).is_err());
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
