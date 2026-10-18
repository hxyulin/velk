use crate::{Manifest, PluginSpec};

pub(crate) fn manifest() -> Manifest {
    Manifest {
        plugins: velk_plugin_builtin::entries()
            .into_iter()
            .map(|api| {
                let end = api.id.iter().position(|byte| *byte == 0).unwrap();
                let id = String::from_utf8(api.id[..end].to_vec()).unwrap();
                PluginSpec {
                    interfaces: bundled_interfaces(&id),
                    dependencies: if id == "velk.language-server.rust-analyzer" {
                        vec!["velk.lsp".into()]
                    } else {
                        vec![]
                    },
                    id,
                }
            })
            .collect(),
    }
}

fn bundled_interfaces(id: &str) -> velk_plugin_contracts::interfaces::Interfaces {
    use velk_plugin_contracts::interfaces::{Implementation, Interfaces, Requirement};
    let mut interfaces = Interfaces::default();
    for (provider, consumer, interface, title, hook) in [
        (
            "velk.files.backend",
            "velk.files",
            "velk.files.tree",
            "File tree",
            "velk.files.tree",
        ),
        (
            "velk.project.files",
            "velk.quick-open",
            "velk.project.files",
            "Project file queries",
            "velk.project.query",
        ),
        (
            "velk.project.search",
            "velk.workspace-find",
            "velk.project.search",
            "Workspace text search",
            "velk.project.query",
        ),
    ] {
        if id == provider {
            interfaces.implements.push(Implementation {
                interface: interface.into(),
                title: title.into(),
                hook: hook.into(),
                version: 1,
            });
        }
        if id == consumer {
            interfaces.requires.push(Requirement {
                interface: interface.into(),
                version: 1,
                default_provider: provider.into(),
            });
        }
    }
    interfaces
}
