//! Host broker for copied language-server definitions. Definitions select a
//! transport by exact plugin ID, never by namespace prefix or Rust linkage.
use super::*;
use velk_plugin_api::wire::language_server as wire;
use velk_plugin_contracts::LanguageServer;
struct Output {
    value: Option<Result<LanguageServer, String>>,
    calls: usize,
}
unsafe extern "C" fn collect(
    context: *mut c_void,
    definition: *const LanguageServerDefinition,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if context.is_null() || definition.is_null() {
            return INVALID;
        }
        let output = unsafe { &mut *context.cast::<Output>() };
        output.calls += 1;
        if output.calls != 1 {
            output.value = Some(Err(
                "language-server query emitted multiple definitions".into()
            ));
            return INVALID;
        }
        // SAFETY: provider promises readable borrowed records until callback return.
        output.value = Some(unsafe { wire::decode(&*definition) });
        if output.value.as_ref().unwrap().is_ok() {
            OK
        } else {
            INVALID
        }
    }))
    .unwrap_or(PANICKED)
}
impl Generation {
    fn language_server(&mut self, language: &str) -> Result<Option<LanguageServer>, String> {
        let Some(api) = self.language else {
            return Ok(None);
        };
        let mut output = Output {
            value: None,
            calls: 0,
        };
        // SAFETY: serialized live instance, synchronous callback with stack-owned
        // context, and all result strings copied before this call returns.
        let status = unsafe {
            (api.query)(
                api.context,
                language.as_ptr(),
                language.len(),
                collect,
                (&mut output as *mut Output).cast(),
            )
        };
        if status == NOT_FOUND && output.calls == 0 {
            return Ok(None);
        }
        if let Some(Err(error)) = output.value {
            return Err(format!("{}: {error}", self.info.id));
        }
        if status != OK || output.calls != 1 {
            return Err(format!(
                "{}: language-server query failed ({status})",
                self.info.id
            ));
        }
        Ok(Some(output.value.unwrap()?))
    }
}
impl Manager {
    pub(super) fn resolve_language_server(
        &mut self,
        language: &str,
    ) -> Result<Option<(String, LanguageServer)>, String> {
        let profiles: Vec<_> = self
            .plugins
            .values()
            .filter(|p| !p.destroyed && p.language.is_some())
            .map(|p| p.info.id.clone())
            .collect();
        let mut selected = None;
        for id in profiles {
            let Some(definition) = self
                .plugins
                .get_mut(&id)
                .unwrap()
                .language_server(language)?
            else {
                continue;
            };
            if definition.language != language {
                return Err(format!("{id} returned a definition for the wrong language"));
            }
            if let Some((other, _)) = &selected {
                return Err(format!(
                    "conflicting language-server plugins for {language}: {other}, {id}"
                ));
            }
            selected = Some((id, definition));
        }
        if let Some((id, definition)) = &selected
            && !self
                .plugins
                .get(&definition.transport)
                .is_some_and(|p| !p.destroyed && p.semantic.is_some())
        {
            return Err(format!(
                "{id} requires {} (not loaded or not a semantic transport)",
                definition.transport
            ));
        }
        Ok(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    unsafe extern "C" fn query(
        _: *mut c_void,
        _: *const u8,
        _: usize,
        sink: LanguageServerSink,
        context: *mut c_void,
    ) -> i32 {
        let definition = LanguageServer {
            transport: "velk.lsp".into(),
            language: "rust".into(),
            executable: "test-server".into(),
            arguments: vec![],
            root: None,
            initialization_options: "{}".into(),
            configuration: "{}".into(),
        };
        wire::with_definition(&definition, |raw| unsafe { sink(context, raw) }).unwrap()
    }
    unsafe extern "C" fn destroy(_: *mut c_void) -> i32 {
        OK
    }
    fn profile(id: &str) -> Generation {
        Generation {
            info: PluginInfo {
                id: id.into(),
                version: "1".into(),
                generation: 1,
                loaded_path: PathBuf::new(),
                description: String::new(),
                metadata: Default::default(),
                settings_values: Default::default(),
                settings_error: None,
                resident_code: false,
                hooks: vec![],
            },
            api: PluginApi {
                abi_version: ABI_VERSION,
                struct_size: std::mem::size_of::<PluginApi>() as u32,
                flags: 0,
                id: [0; 64],
                version: [0; 32],
                create: None,
                destroy: Some(destroy),
                describe: None,
                hook_count: None,
                hook_info: None,
                get_hook: None,
            },
            instance: NonNull::dangling(),
            destroyed: false,
            hooks: vec![],
            markdown: None,
            language: Some(LanguageApi {
                context: NonNull::<c_void>::dangling().as_ptr(),
                query,
            }),
            semantic: None,
            library: None,
        }
    }
    #[test]
    fn dependencies_use_exact_ids_and_competing_language_definitions_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = Manager::new(directory.path().into());
        assert!(manager.resolve_language_server("rust").unwrap().is_none());
        manager.plugins.insert(
            "velk.language-server.rust-analyzer".into(),
            profile("velk.language-server.rust-analyzer"),
        );
        assert!(
            manager
                .resolve_language_server("rust")
                .unwrap_err()
                .contains("requires velk.lsp")
        );
        // A longer ID sharing the same prefix cannot satisfy the dependency.
        manager
            .plugins
            .insert("velk.lsp.other".into(), profile("velk.lsp.other"));
        manager.plugins.get_mut("velk.lsp.other").unwrap().language = None;
        assert!(
            manager
                .resolve_language_server("rust")
                .unwrap_err()
                .contains("requires velk.lsp")
        );
        manager.plugins.insert(
            "other.language-server.rust".into(),
            profile("other.language-server.rust"),
        );
        let error = manager.resolve_language_server("rust").unwrap_err();
        assert!(error.contains("conflicting"));
        assert!(error.contains("other.language-server.rust"));
    }
}
