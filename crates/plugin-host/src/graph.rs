use crate::{Manifest, PluginSpec};
use std::collections::{BTreeMap, BTreeSet};

impl Manifest {
    /// Resolve interface selections to ordinary dependency edges before loading.
    pub fn resolve_interfaces(&self) -> Result<Self, String> {
        let mut result = self.clone();
        for plugin in &mut result.plugins {
            for requirement in &mut plugin.interfaces.requires {
                let provider = &requirement.default_provider;
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
}
