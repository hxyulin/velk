//! Discovery validates named contracts once; hot paths use copied typed tables.
use super::*;
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HookDescriptor {
    pub name: String,
    pub contract: String,
    pub version: u32,
    pub kind: u32,
}
#[derive(Clone, Debug)]
pub struct HookHandle {
    owner: u64,
    plugin: String,
    generation: u64,
    slot: usize,
}
pub(super) struct Hook {
    pub(super) info: HookDescriptor,
    bytes: Option<BytesApi>,
    event: Option<EventApi>,
}
impl HookHandle {
    pub fn generation(&self) -> u64 {
        self.generation
    }
}
impl Generation {
    pub(super) unsafe fn table<T: Copy>(
        &self,
        index: usize,
        contract: &str,
        version: u32,
    ) -> Result<T, String> {
        let mut output = std::mem::MaybeUninit::<T>::uninit();
        // SAFETY: trusted contract fixes T's C layout and signatures; the SDK
        // verifies contract/version/size before writing the caller-owned table.
        let status = unsafe {
            self.api.get_hook.unwrap()(
                self.instance.as_ptr(),
                index,
                Bytes {
                    data: contract.as_ptr(),
                    length: contract.len(),
                },
                version,
                std::mem::size_of::<T>(),
                output.as_mut_ptr().cast(),
            )
        };
        if status != OK {
            return Err(format!("hook table query failed ({status})"));
        }
        Ok(unsafe { output.assume_init() })
    }
    pub(super) fn discover_hooks(&mut self) -> Result<(), String> {
        let count = unsafe { self.api.hook_count.unwrap()(self.instance.as_ptr()) };
        if count > MAX_HOOKS {
            return Err("too many hook registrations".into());
        }
        let mut names = BTreeSet::new();
        for index in 0..count {
            let mut info = std::mem::MaybeUninit::<HookInfo>::uninit();
            let status = unsafe {
                self.api.hook_info.unwrap()(self.instance.as_ptr(), index, info.as_mut_ptr())
            };
            if status != OK {
                return Err(format!("hook metadata query failed ({status})"));
            }
            let info = unsafe { info.assume_init() };
            let name = metadata(&info.name)?;
            let contract = metadata(&info.contract)?;
            if !names.insert(name.clone())
                || info.version == 0
                || !(REQUEST..=CONTRIBUTION).contains(&info.kind)
                || info.table_size == 0
                || info.table_size > 4096
            {
                return Err("invalid hook metadata or duplicate name".into());
            }
            let mut hook = Hook {
                info: HookDescriptor {
                    name: name.clone(),
                    contract: contract.clone(),
                    version: info.version,
                    kind: info.kind,
                },
                bytes: None,
                event: None,
            };
            macro_rules! typed {
                ($ty:ty,$kind:expr) => {
                    typed!($ty, $kind, 1)
                };
                ($ty:ty,$kind:expr,$version:expr) => {{
                    if info.version != $version
                        || info.kind != $kind
                        || info.table_size != std::mem::size_of::<$ty>()
                    {
                        return Err(format!("incompatible hook contract: {name}"));
                    }
                    unsafe { self.table::<$ty>(index, &contract, $version)? }
                }};
            }
            match contract.as_str() {
                HOST_HOOK if name == HOST_HOOK => {
                    let _ = typed!(HostApi, SERVICE);
                }
                DIAGNOSTICS_HOOK if name == DIAGNOSTICS_HOOK => {
                    let api = typed!(DiagnosticsApi, SERVICE);
                    if let Some(sink) = velk_diagnostics::host_sink() {
                        // The copied callback table is executable-owned until
                        // process exit. It never holds a plugin callback alive.
                        if unsafe { (api.attach)(&sink) } != OK {
                            return Err("cannot attach plugin diagnostics".into());
                        }
                    }
                }
                MARKUP_CONTRACT if name == MARKUP_HOOK => {
                    self.markdown = Some(typed!(MarkupApi, REQUEST))
                }
                LANGUAGE_CONTRACT if name == LANGUAGE_HOOK => {
                    self.language = Some(typed!(LanguageApi, CONTRIBUTION))
                }
                SEMANTIC_CONTRACT if name == SEMANTIC_HOOK => {
                    self.semantic = Some(typed!(SemanticApi, SERVICE, 1))
                }
                velk_editing::CONTRACT
                    if name == velk_editing::INDENT_HOOK || name == velk_editing::PAIRS_HOOK =>
                {
                    let role = if name == velk_editing::INDENT_HOOK {
                        velk_editing::Role::Indentation
                    } else {
                        velk_editing::Role::Pairing
                    };
                    self.editing.insert(role, typed!(EditingApi, SERVICE));
                }
                BYTES_CONTRACT => hook.bytes = Some(typed!(BytesApi, REQUEST)),
                EVENT_CONTRACT => hook.event = Some(typed!(EventApi, EVENT)),
                _ => {} // Unknown contracts are discoverable, not reinterpreted.
            }
            self.hooks.push(hook);
        }
        Ok(())
    }
}
pub(super) struct Reply {
    pub(super) result: Option<Result<Vec<u8>, String>>,
    pub(super) calls: usize,
}
pub(super) unsafe extern "C" fn collect(context: *mut c_void, bytes: Bytes, status: i32) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if context.is_null() {
            return INVALID;
        }
        let reply = unsafe { &mut *context.cast::<Reply>() };
        reply.calls += 1;
        if reply.calls != 1
            || bytes.length > MAX_OUTPUT_BYTES
            || (bytes.length != 0 && bytes.data.is_null())
        {
            reply.result = Some(Err("invalid request reply".into()));
            return INVALID;
        }
        let value = if bytes.length == 0 {
            vec![]
        } else {
            unsafe { std::slice::from_raw_parts(bytes.data, bytes.length) }.to_vec()
        };
        reply.result = Some(if status == OK {
            Ok(value)
        } else {
            Err(String::from_utf8(value)
                .unwrap_or_else(|_| "request failed with non-UTF-8 error".into()))
        });
        OK
    }))
    .unwrap_or(PANICKED)
}
impl Manager {
    pub fn hooks(&self, plugin: &str) -> Result<Vec<HookDescriptor>, String> {
        let plugin = self
            .plugins
            .get(plugin)
            .filter(|p| !p.destroyed)
            .ok_or("plugin not loaded")?;
        Ok(plugin.hooks.iter().map(|h| h.info.clone()).collect())
    }
    pub fn bind_hook(&self, plugin: &str, name: &str) -> Result<HookHandle, String> {
        let generation = self
            .plugins
            .get(plugin)
            .filter(|p| !p.destroyed)
            .ok_or("plugin not loaded")?;
        let slot = generation
            .hooks
            .iter()
            .position(|h| h.info.name == name && h.bytes.is_some())
            .ok_or("named request not registered")?;
        Ok(HookHandle {
            owner: self.owner,
            plugin: plugin.into(),
            generation: generation.info.generation,
            slot,
        })
    }
    #[tracing::instrument(level = "trace", skip_all, fields(bytes = input.len()))]
    pub fn call_hook(&mut self, handle: &HookHandle, input: &[u8]) -> Result<Vec<u8>, String> {
        if handle.owner != self.owner {
            return Err("hook handle belongs to another runtime".into());
        }
        if input.len() > MAX_INPUT_BYTES {
            return Err("hook input exceeds 1 MiB".into());
        }
        let generation = self
            .plugins
            .get(&handle.plugin)
            .filter(|p| !p.destroyed && p.info.generation == handle.generation)
            .ok_or("hook handle has retired")?;
        let api = generation
            .hooks
            .get(handle.slot)
            .and_then(|h| h.bytes)
            .ok_or("request hook unavailable")?;
        call_bytes(api, input)
    }
    /// Ordered notification fan-out on the background worker. Accepted events
    /// are delivered even if their acknowledgement receiver is dropped.
    pub fn emit_hook(&mut self, name: &str, input: &[u8]) -> Result<usize, String> {
        if input.len() > MAX_INPUT_BYTES {
            return Err("hook event exceeds 1 MiB".into());
        }
        let mut delivered = 0;
        let mut errors = vec![];
        for plugin in self.plugins.values().filter(|p| !p.destroyed) {
            for hook in plugin.hooks.iter().filter(|h| h.info.name == name) {
                if let Some(api) = hook.event {
                    let status = unsafe {
                        (api.notify)(
                            api.context,
                            Bytes {
                                data: input.as_ptr(),
                                length: input.len(),
                            },
                        )
                    };
                    if status == OK {
                        delivered += 1;
                    } else {
                        errors.push(format!(
                            "{}: event handler failed ({status})",
                            plugin.info.id
                        ));
                    }
                }
            }
        }
        if errors.is_empty() {
            Ok(delivered)
        } else {
            Err(errors.join("; "))
        }
    }
}

fn call_bytes(api: BytesApi, input: &[u8]) -> Result<Vec<u8>, String> {
    let mut reply = Reply {
        result: None,
        calls: 0,
    };
    let status = unsafe {
        (api.call)(
            api.context,
            Bytes {
                data: input.as_ptr(),
                length: input.len(),
            },
            collect,
            (&mut reply as *mut Reply).cast(),
        )
    };
    if status != OK {
        return Err(format!("hook request failed ({status})"));
    }
    reply.result.ok_or("hook returned without a reply")?
}

impl Generation {
    pub(super) fn read_metadata(
        &self,
    ) -> Result<Option<velk_plugin_contracts::PluginMetadata>, String> {
        let Some(hook) = self.hooks.iter().find(|h| h.info.name == METADATA_HOOK) else {
            return Ok(None);
        };
        let api = hook
            .bytes
            .ok_or("metadata hook must use the bytes request contract")?;
        let bytes = call_bytes(api, &[])?;
        if bytes.len() > 512 * 1024 {
            return Err("plugin metadata exceeds 512 KiB".into());
        }
        let metadata: velk_plugin_contracts::PluginMetadata =
            serde_json::from_slice(&bytes).map_err(|e| format!("invalid plugin metadata: {e}"))?;
        metadata.validate()?;
        Ok(Some(metadata))
    }
}

impl Generation {
    pub(super) fn configure_settings(
        &mut self,
        overrides: &velk_plugin_contracts::settings::Values,
    ) -> Result<(), String> {
        use velk_plugin_contracts::settings::{Values, resolve};
        let schema = &self.info.metadata.settings;
        if schema.is_empty() {
            return Ok(());
        }
        let values = match resolve(schema, overrides) {
            Ok(values) => values,
            Err(error) => {
                self.info.settings_error = Some(format!(
                    "{error}; using plugin defaults until corrected and restarted."
                ));
                resolve(schema, &Values::new())?
            }
        };
        let hook = self
            .hooks
            .iter()
            .find(|h| h.info.name == SETTINGS_HOOK)
            .ok_or("plugin declares settings without a configuration hook")?;
        let api = hook
            .bytes
            .ok_or("settings hook must use the bytes request contract")?;
        call_bytes(
            api,
            &serde_json::to_vec(&values).map_err(|e| e.to_string())?,
        )?;
        self.info.settings_values = values;
        Ok(())
    }
}
