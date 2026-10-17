#![allow(unsafe_code)]
use crate::{Event, PluginSpec, read_manifest};
mod code;
mod completion_frontend;
mod editing;
mod hooks;
mod matching;
mod snippets;
mod syntax;
mod undo;
use code::Code;
mod language_server;
mod semantic;
mod services;
pub use hooks::{HookDescriptor, HookHandle};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    path::PathBuf,
    ptr::NonNull,
    sync::{Arc, Weak},
};
use velk_plugin_api::*;

#[derive(Clone, Debug)]
pub struct PluginInfo {
    pub id: String,
    pub version: String,
    pub generation: u64,
    /// Private loaded copy. Ordinary code is removed after teardown; resident
    /// builds retain their copy and reuse it across logical generations.
    pub loaded_path: PathBuf,
    /// Host-owned copy; remains valid after the library is unloaded.
    pub description: String,
    /// Owned display name, Markdown README and declared access requirements.
    pub metadata: velk_plugin_contracts::PluginMetadata,
    pub settings_values: velk_plugin_contracts::settings::Values,
    pub settings_error: Option<String>,
    /// Logical unload releases sessions/instances, while this build stays mapped.
    pub resident_code: bool,
    /// Names of the hooks this generation registered, in registration order.
    pub hooks: Vec<String>,
    pub dependencies: Vec<String>,
}
struct Generation {
    info: PluginInfo,
    api: PluginApi,
    instance: NonNull<c_void>,
    destroyed: bool,
    host: Option<Arc<services::Endpoint>>,
    hooks: Vec<hooks::Hook>,
    markdown: Option<MarkupApi>,
    syntax: Option<SyntaxApi>,
    matching: Option<MatchingApi>,
    completion_frontend: Option<CompletionFrontendApi>,
    snippet_provider: Option<SnippetApi>,
    undo: Option<UndoApi>,
    language: Option<LanguageApi>,
    semantic: Option<SemanticApi>,
    editing: BTreeMap<velk_editing::Role, EditingApi>,
    services_active: Arc<std::sync::RwLock<bool>>,
    // Explicit drop order: instance, library, then its on-disk copy.
    library: Option<Arc<Code>>,
}
impl Generation {
    /// Caller guarantees the plugin ABI and unload contract.
    #[tracing::instrument(level = "debug", skip_all, fields(plugin = %spec.id, generation))]
    unsafe fn load(
        spec: &PluginSpec,
        generation: u64,
        code: Arc<Code>,
        settings: &velk_plugin_contracts::settings::Values,
        host: Option<Arc<crate::services::Bridge>>,
    ) -> Result<Self, String> {
        let api = *code
            .apis
            .iter()
            .find(|api| metadata(&api.id).is_ok_and(|id| id == spec.id))
            .ok_or_else(|| format!("library does not provide {}", spec.id))?;
        let id = metadata(&api.id)?;
        let version = metadata(&api.version)?;
        let create = api.create.unwrap();
        let mut instance = std::ptr::null_mut();
        // SAFETY: validated function table; output storage remains live for the call.
        let status = unsafe { create(&mut instance) };
        if status != OK {
            return Err(format!("initialization failed with status {status}"));
        }
        let instance = NonNull::new(instance).ok_or("plugin returned a null instance")?;
        let mut loaded = Self {
            info: PluginInfo {
                id,
                version,
                generation,
                loaded_path: code.path.clone(),
                description: String::new(),
                metadata: Default::default(),
                settings_values: Default::default(),
                settings_error: None,
                resident_code: code.resident,
                hooks: vec![],
                dependencies: spec.dependencies.clone(),
            },
            api,
            instance,
            destroyed: false,
            host: None,
            hooks: vec![],
            markdown: None,
            syntax: None,
            matching: None,
            completion_frontend: None,
            snippet_provider: None,
            undo: None,
            language: None,
            semantic: None,
            editing: Default::default(),
            services_active: Arc::new(std::sync::RwLock::new(true)),
            library: Some(code),
        };
        loaded.discover_hooks()?;
        loaded.info.hooks = loaded.hooks.iter().map(|h| h.info.name.clone()).collect();
        loaded.info.description = loaded.describe()?;
        loaded.info.metadata =
            loaded
                .read_metadata()?
                .unwrap_or_else(|| velk_plugin_contracts::PluginMetadata {
                    name: loaded.info.id.clone(),
                    ..Default::default()
                });
        if loaded
            .hooks
            .iter()
            .any(|h| h.info.name == velk_plugin_contracts::buffers::HOOK)
            && !loaded
                .info
                .metadata
                .capabilities
                .iter()
                .any(|c| c.id == velk_plugin_contracts::host::BUFFER_READ)
        {
            return Err("buffer events require editor.buffer.read".into());
        }
        for command in loaded.info.metadata.commands.iter().chain(
            loaded
                .info
                .metadata
                .contributions
                .iter()
                .map(|source| &source.command),
        ) {
            use velk_plugin_contracts::commands::Handler;
            let hooks: Vec<&str> = match &command.handler {
                Handler::Request { hook } | Handler::Picker { hook, .. } => vec![hook],
                Handler::Collect {
                    hook, automatic, ..
                } => std::iter::once(hook.as_str())
                    .chain(automatic.as_deref())
                    .collect(),
                Handler::Semantic { .. } => vec![],
            };
            for hook in hooks {
                if !loaded.hooks.iter().any(|h| {
                    h.info.name == hook
                        && h.info.contract == BYTES_CONTRACT
                        && h.info.version == 1
                        && h.info.kind == REQUEST
                }) {
                    return Err(format!(
                        "command {} requires a request hook: {hook}",
                        command.id
                    ));
                }
            }
        }
        if let Some(host) = host {
            loaded.attach_host(host)?;
        }
        loaded.configure_settings(settings)?;
        Ok(loaded)
    }
    fn describe(&mut self) -> Result<String, String> {
        let mut bytes = vec![0; 1024];
        let mut length = 0;
        for _ in 0..2 {
            // SAFETY: exclusive live instance, valid caller-owned output buffers.
            let status = unsafe {
                self.api.describe.unwrap()(
                    self.instance.as_ptr(),
                    bytes.as_mut_ptr(),
                    bytes.len(),
                    &mut length,
                )
            };
            if status == BUFFER_TOO_SMALL && length <= 64 * 1024 && length > bytes.len() {
                bytes.resize(length, 0);
                continue;
            }
            if status != OK || length > bytes.len() {
                return Err(format!(
                    "describe failed with status {status}, length {length}"
                ));
            }
            bytes.truncate(length);
            return String::from_utf8(bytes).map_err(|e| e.to_string());
        }
        Err("plugin description changed size repeatedly".into())
    }
}
struct Output {
    document: velk_markup::Document,
    emitted: bool,
    valid: bool,
}
unsafe extern "C" fn collect_runs(
    context: *mut c_void,
    runs: *const StyledRun,
    length: usize,
) -> i32 {
    // No Rust unwind may cross the callback's C boundary either.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if context.is_null() || runs.is_null() || length > MAX_RUNS {
            return INVALID;
        }
        // SAFETY: the host created this live Output for the duration of the call.
        let output = unsafe { &mut *context.cast::<Output>() };
        if output.emitted {
            output.valid = false;
            return INVALID;
        }
        output.emitted = true;
        // SAFETY: the trusted provider supplies length initialized ABI records.
        let runs = unsafe { std::slice::from_raw_parts(runs, length) };
        let mut total = 0usize;
        for run in runs {
            total = match total
                .checked_add(run.text.length)
                .and_then(|n| n.checked_add(run.link.length))
            {
                Some(n) if n <= MAX_OUTPUT_BYTES => n,
                _ => return INVALID,
            };
            if run.flags & !(BOLD | ITALIC | CODE | STRIKETHROUGH | LINK) != 0
                || run.heading > 6
                || run.text.data.is_null()
                || run.link.data.is_null()
            {
                return INVALID;
            }
            // SAFETY: provider-owned bytes remain readable throughout this callback.
            let text = unsafe { std::slice::from_raw_parts(run.text.data, run.text.length) };
            let link = unsafe { std::slice::from_raw_parts(run.link.data, run.link.length) };
            let (Ok(text), Ok(link)) = (std::str::from_utf8(text), std::str::from_utf8(link))
            else {
                return INVALID;
            };
            output.document.runs.push(velk_markup::Run {
                text: text.into(),
                style: velk_markup::Style {
                    bold: run.flags & BOLD != 0,
                    italic: run.flags & ITALIC != 0,
                    code: run.flags & CODE != 0,
                    strikethrough: run.flags & STRIKETHROUGH != 0,
                    heading: run.heading,
                    link: (run.flags & LINK != 0).then(|| link.to_owned()),
                },
            });
        }
        output.valid = true;
        OK
    }))
    .unwrap_or(PANICKED)
}
impl Generation {
    fn render_markdown(&mut self, source: &str) -> Result<velk_markup::Document, String> {
        let api = self
            .markdown
            .ok_or("provider has no Markdown registration")?;
        let mut output = Output {
            document: Default::default(),
            emitted: false,
            valid: false,
        };
        // SAFETY: exclusive live instance, borrowed UTF-8 input, synchronous callback;
        // the library remains owned by self until all output has been copied.
        let status = unsafe {
            (api.render)(
                api.context,
                source.as_ptr(),
                source.len(),
                collect_runs,
                (&mut output as *mut Output).cast(),
            )
        };
        if status != OK || !output.valid {
            return Err(format!("Markdown provider failed with status {status}"));
        }
        Ok(output.document)
    }
}
impl Generation {
    fn deactivate(&self) {
        if let Ok(mut active) = self.services_active.write() {
            *active = false;
        }
        if let Some(host) = &self.host {
            host.active
                .store(false, std::sync::atomic::Ordering::Release);
        }
    }
    #[tracing::instrument(level = "debug", skip_all, fields(plugin = %self.info.id, generation = self.info.generation))]
    fn retire(&mut self) -> bool {
        self.deactivate();
        if self.destroyed {
            return self.library.is_some();
        }
        self.destroyed = true;
        // SAFETY: serial owner destroys exactly once, with library still loaded.
        let status = unsafe { self.api.destroy.unwrap()(self.instance.as_ptr()) };
        if status != OK {
            eprintln!(
                "velk: plugin {} failed shutdown ({status}); library retained until process exit",
                self.info.id
            );
            // Failed teardown does not prove quiescence. Keep code mapped.
            if let Some(library) = self.library.take() {
                std::mem::forget(library);
            }
            return false;
        }
        true
    }
}
impl Drop for Generation {
    fn drop(&mut self) {
        self.retire();
        // Close before TempDir deletes its files (required on Windows).
        drop(self.library.take());
    }
}

fn metadata(bytes: &[u8]) -> Result<String, String> {
    let end = bytes
        .iter()
        .position(|b| *b == 0)
        .ok_or("unterminated plugin metadata")?;
    if end == 0 {
        return Err("empty plugin metadata".into());
    }
    String::from_utf8(bytes[..end].to_vec()).map_err(|e| e.to_string())
}
/// Serial, worker-owned lifecycle manager. It exposes no raw function pointers or
/// plugin-owned values. Session producers are joined before instance destruction.
static NEXT_MANAGER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
pub struct Manager {
    pub(crate) host: Option<Arc<crate::services::Bridge>>,
    owner: u64,
    directory: PathBuf,
    plugins: BTreeMap<String, Generation>,
    next_generation: u64,
    blocked: BTreeSet<String>,
    excluded: BTreeSet<String>,
    sessions: BTreeMap<u64, semantic::DocumentSession>,
    resident_code: BTreeMap<(String, bool), Arc<Code>>,
    images: BTreeMap<(String, bool), Weak<Code>>,
    settings: velk_plugin_contracts::settings::PluginValues,
}
impl Manager {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            host: None,
            owner: NEXT_MANAGER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            directory,
            plugins: BTreeMap::new(),
            next_generation: 1,
            blocked: BTreeSet::new(),
            excluded: BTreeSet::new(),
            sessions: BTreeMap::new(),
            resident_code: BTreeMap::new(),
            images: BTreeMap::new(),
            settings: Default::default(),
        }
    }
    pub fn set_settings(&mut self, settings: velk_plugin_contracts::settings::PluginValues) {
        self.settings = settings;
    }
    pub fn plugins(&self) -> impl Iterator<Item = &PluginInfo> {
        self.plugins
            .values()
            .filter(|p| !p.destroyed)
            .map(|p| &p.info)
    }
    pub(crate) fn call_dependency(
        &mut self,
        caller: &str,
        generation: u64,
        provider: &str,
        hook: &str,
        input: &[u8],
    ) -> Result<Vec<u8>, String> {
        let caller = self
            .plugins
            .get(caller)
            .filter(|p| p.info.generation == generation && !p.destroyed)
            .ok_or("calling plugin retired")?;
        if !caller.info.dependencies.iter().any(|id| id == provider) {
            return Err("plugin call requires a declared direct dependency".into());
        }
        let consumer_id = caller.info.id.clone();
        let handle = self.bind_hook(provider, hook)?;
        let input = serde_json::to_vec(&velk_plugin_contracts::backend::Call {
            consumer: velk_plugin_contracts::backend::Consumer {
                plugin: consumer_id,
                generation,
            },
            input: input.to_vec(),
        })
        .map_err(|e| e.to_string())?;
        self.call_hook(&handle, &input)
    }
    pub fn describe(&mut self, id: &str) -> Result<String, String> {
        self.plugins
            .get_mut(id)
            .ok_or_else(|| format!("plugin {id} is not loaded"))?
            .describe()
    }
    pub fn render_markdown(&mut self, source: &str) -> Result<velk_markup::Document, String> {
        self.render_markdown_with(crate::DEFAULT_MARKDOWN_PROVIDER, source)
    }
    pub fn render_markdown_with(
        &mut self,
        provider: &str,
        source: &str,
    ) -> Result<velk_markup::Document, String> {
        if velk_plugin_contracts::parse_plugin_id(provider).is_none() {
            return Err("invalid Markdown provider ID".into());
        }
        if source.len() > MAX_INPUT_BYTES {
            return Err("Markdown input exceeds 1 MiB".into());
        }
        match self
            .plugins
            .get_mut(provider)
            .filter(|p| !p.destroyed && p.markdown.is_some())
        {
            Some(provider) => provider.render_markdown(source),
            None => Ok(velk_markup::Document::plain(source)),
        }
    }

    /// Load inactive plugins listed on disk. Already-active IDs are left alone,
    /// even when a newer build is staged. Unload explicitly before replacing one.
    /// # Safety
    /// All libraries selected by the directory must implement the trusted v1 ABI,
    /// including its join-before-release semantic session contract.
    #[tracing::instrument(level = "debug", skip_all)]
    pub unsafe fn load(&mut self) -> Result<Vec<Event>, String> {
        unsafe { self.load_except(&[]) }
    }
    /// Like `load`, skipping the given IDs (for example user-disabled plugins).
    /// # Safety
    /// Same trusted native-code requirements as `load`.
    pub unsafe fn load_except(&mut self, excluded: &[String]) -> Result<Vec<Event>, String> {
        if !self.directory.join(crate::MANIFEST).exists() {
            return Ok(vec![]);
        }
        self.excluded = excluded.iter().cloned().collect();
        let manifest = read_manifest(&self.directory)?;
        let roots = manifest
            .plugins
            .iter()
            .filter(|p| {
                p.install_reason != crate::InstallReason::Dependency && !excluded.contains(&p.id)
            })
            .map(|p| p.id.clone())
            .collect::<Vec<_>>();
        let selected = manifest.dependency_closure(&roots);
        let specs = manifest
            .plugins
            .into_iter()
            .filter(|p| selected.contains(&p.id) && !excluded.contains(&p.id))
            .collect();
        unsafe { self.load_specs(specs) }
    }
    /// IDs listed in the current manifest, loaded or not. Unreadable is empty.
    pub fn inventory(&self) -> Vec<PluginSpec> {
        read_manifest(&self.directory)
            .map(|m| m.plugins)
            .unwrap_or_default()
    }
    pub fn available(&self) -> Vec<String> {
        if !self.directory.join(crate::MANIFEST).exists() {
            return vec![];
        }
        read_manifest(&self.directory)
            .map(|m| m.plugins.into_iter().map(|p| p.id).collect())
            .unwrap_or_default()
    }
    /// # Safety
    /// Same trusted native-code requirements as `load`.
    pub unsafe fn load_selected(&mut self, ids: &[String]) -> Result<Vec<Event>, String> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let manifest = read_manifest(&self.directory)?;
        let mut events: Vec<_> = ids
            .iter()
            .filter(|id| !manifest.plugins.iter().any(|p| &p.id == *id))
            .map(|id| Event::Rejected {
                id: id.clone(),
                message: "requested plugin is missing from plugins.json".into(),
            })
            .collect();
        for id in ids {
            self.excluded.remove(id);
        }
        let selected = manifest.dependency_closure(ids);
        let specs = manifest
            .plugins
            .into_iter()
            .filter(|spec| selected.contains(&spec.id) && !self.excluded.contains(&spec.id))
            .collect();
        events.extend(unsafe { self.load_specs(specs)? });
        Ok(events)
    }
    unsafe fn load_specs(&mut self, specs: Vec<PluginSpec>) -> Result<Vec<Event>, String> {
        let mut events = Vec::new();
        let mut verified = BTreeMap::new();
        // Excluded or failed dependencies stay absent; consumers are rejected.
        let order = crate::Manifest {
            plugins: self.inventory(),
        }
        .dependency_order()?;
        let mut specs = specs;
        specs.sort_by_key(|spec| order.iter().position(|id| id == &spec.id));
        for spec in specs {
            let id = spec.id.clone();
            if self.blocked.contains(&id) {
                events.push(Event::Rejected {
                    id,
                    message: "previous shutdown failed; restart required".into(),
                });
                continue;
            }
            if self.plugins.contains_key(&id) {
                continue;
            }
            if let Some(dependency) = spec
                .dependencies
                .iter()
                .find(|id| !self.plugins.contains_key(*id))
            {
                events.push(Event::Rejected {
                    id,
                    message: format!("required dependency is unavailable: {dependency}"),
                });
                continue;
            }
            let generation = self.next_generation;
            self.next_generation += 1;
            // SAFETY: inherited from load's caller.
            let key = (spec.digest.clone(), spec.pack);
            let cached = self
                .resident_code
                .get(&key)
                .cloned()
                .or_else(|| self.images.get(&key).and_then(Weak::upgrade));
            // Always verify the selected artifact, even when its code is cached.
            let artifact = (spec.library.clone(), key.clone());
            let code = if let Some(code) = verified.get(&artifact) {
                Ok(Arc::clone(code))
            } else {
                unsafe { Code::load(&self.directory, &spec, cached) }.inspect(|code| {
                    verified.insert(artifact, Arc::clone(code));
                })
            };
            let candidate = code.and_then(|code| {
                self.images.insert(key.clone(), Arc::downgrade(&code));
                // Residency applies to the entire image before any factory runs,
                // including factories which fail during initialization.
                if code.resident {
                    self.resident_code.entry(key).or_insert_with(|| {
                        std::mem::forget(code.clone());
                        code.clone()
                    });
                }
                unsafe {
                    Generation::load(
                        &spec,
                        generation,
                        code,
                        self.settings.get(&id).unwrap_or(&Default::default()),
                        self.host.clone(),
                    )
                }
            });
            self.images.retain(|_, image| image.strong_count() != 0);
            match candidate {
                Ok(candidate) => {
                    events.push(Event::Loaded(Box::new(candidate.info.clone())));
                    self.plugins.insert(id, candidate);
                }
                Err(message) => events.push(Event::Rejected { id, message }),
            }
        }
        Ok(events)
    }
    /// Recreate active instances as one worker operation. Inactive plugins stay
    /// inactive. Optional named hooks transfer owned bytes, never native handles.
    /// # Safety
    /// As with `load`, all selected native libraries must obey the v1 contract.
    #[tracing::instrument(level = "debug", skip_all, fields(plugin = id))]
    pub unsafe fn restart(&mut self, id: Option<&str>) -> Result<Vec<Event>, String> {
        let mut ids: Vec<_> = self
            .plugins
            .keys()
            .filter(|key| id.is_none_or(|id| id == key.as_str()))
            .cloned()
            .collect();
        if id.is_some() && ids.is_empty() {
            return Err("plugin is not loaded".into());
        }
        if ids.is_empty() {
            return Ok(vec![]);
        }
        loop {
            let before = ids.len();
            for plugin in self.plugins.values() {
                if !ids.contains(&plugin.info.id)
                    && plugin.info.dependencies.iter().any(|id| ids.contains(id))
                {
                    ids.push(plugin.info.id.clone());
                }
            }
            if ids.len() == before {
                break;
            }
        }
        let manifest = read_manifest(&self.directory)?;
        let specs: Vec<_> = manifest
            .plugins
            .into_iter()
            .filter(|spec| ids.contains(&spec.id))
            .collect();
        if specs.len() != ids.len() {
            return Err("an active plugin is missing from plugins.json; restart cancelled".into());
        }
        let states = self.snapshot_plugins(&ids)?;
        let mut events = self.unload_ids(&ids.iter().cloned().collect());
        events.extend(unsafe { self.load_specs(specs)? });
        events.extend(self.restore_plugins(states));
        Ok(events)
    }
    pub(crate) fn snapshot_plugins(
        &mut self,
        ids: &[String],
    ) -> Result<Vec<(String, Vec<u8>)>, String> {
        let mut states = Vec::new();
        // Capture every state before retiring anything. A failed capture leaves
        // all existing instances and sessions alive.
        for id in ids {
            let hooks = self.hooks(id)?;
            let snapshot = hooks.iter().any(|h| h.name == LIFECYCLE_SNAPSHOT_HOOK);
            let restore = hooks.iter().any(|h| h.name == LIFECYCLE_RESTORE_HOOK);
            if snapshot != restore {
                return Err(format!(
                    "{id}: lifecycle snapshot and restore hooks must be paired"
                ));
            }
            if snapshot {
                let handle = self.bind_hook(id, LIFECYCLE_SNAPSHOT_HOOK)?;
                let state = self.call_hook(&handle, &[])?;
                if state.len() > MAX_INPUT_BYTES {
                    return Err(format!("{id}: lifecycle state exceeds 1 MiB"));
                }
                states.push((id.clone(), state));
            }
        }
        Ok(states)
    }
    pub(crate) fn restore_plugins(&mut self, states: Vec<(String, Vec<u8>)>) -> Vec<Event> {
        let mut events = Vec::new();
        for (id, state) in states {
            let result = self
                .bind_hook(&id, LIFECYCLE_RESTORE_HOOK)
                .and_then(|handle| self.call_hook(&handle, &state));
            if let Err(message) = result {
                events.extend(self.unload_with_dependents(&id));
                events.push(Event::Rejected {
                    id,
                    message: format!("state restore failed: {message}"),
                });
            }
        }
        events
    }
    #[tracing::instrument(level = "debug", skip_all, fields(plugin = id))]
    pub fn unload_with_dependents(&mut self, id: &str) -> Vec<Event> {
        self.excluded.insert(id.to_owned());
        let mut selected = BTreeSet::from([id.to_owned()]);
        loop {
            let before = selected.len();
            for plugin in self.plugins.values() {
                if plugin
                    .info
                    .dependencies
                    .iter()
                    .any(|id| selected.contains(id))
                {
                    selected.insert(plugin.info.id.clone());
                }
            }
            if selected.len() == before {
                break;
            }
        }
        self.unload_ids(&selected)
    }
    fn unload_ids(&mut self, ids: &BTreeSet<String>) -> Vec<Event> {
        // Deactivate every endpoint first so workers waiting for dispatch can join.
        for id in ids {
            if let Some(plugin) = self.plugins.get(id) {
                plugin.deactivate();
            }
        }
        let mut order = crate::Manifest {
            plugins: self.inventory(),
        }
        .dependency_order()
        .unwrap_or_default();
        for id in ids {
            if !order.contains(id) {
                order.push(id.clone());
            }
        }
        order
            .into_iter()
            .rev()
            .filter(|id| ids.contains(id))
            .filter_map(|id| self.unload_one(&id))
            .collect()
    }
    pub fn unload(&mut self, id: &str) -> Option<Event> {
        if let Some(consumer) = self.plugins.values().find(|p| {
            p.info
                .dependencies
                .iter()
                .any(|dependency| dependency == id)
                && *p.services_active.read().unwrap()
        }) {
            return Some(Event::Rejected {
                id: id.into(),
                message: format!(
                    "{} still depends on this plugin; unload dependents first",
                    consumer.info.id
                ),
            });
        }
        self.unload_one(id)
    }
    fn unload_one(&mut self, id: &str) -> Option<Event> {
        let plugin = self.plugins.get(id)?;
        let consumer = velk_plugin_contracts::backend::Consumer {
            plugin: id.into(),
            generation: plugin.info.generation,
        };
        plugin.deactivate();
        if let Ok(bytes) = serde_json::to_vec(&consumer) {
            let _ = self.emit_hook(velk_plugin_contracts::backend::RELEASE_HOOK, &bytes);
        }
        let sessions_retired = self.retire_semantics(id);
        let mut plugin = self.plugins.remove(id)?;
        if sessions_retired && plugin.retire() {
            Some(Event::Removed(id.to_owned()))
        } else {
            self.blocked.insert(id.to_owned());
            Some(Event::Rejected {
                id: id.to_owned(),
                message: "shutdown failed; library retained and restart required".into(),
            })
        }
    }
    pub fn unload_all(&mut self) -> Vec<Event> {
        let ids = self.plugins.keys().cloned().collect();
        self.unload_ids(&ids)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    static DESTROYS: AtomicUsize = AtomicUsize::new(0);
    unsafe extern "C" fn fails_to_destroy(_: *mut c_void) -> i32 {
        DESTROYS.fetch_add(1, Ordering::SeqCst);
        PANICKED
    }
    #[test]
    fn failed_teardown_is_not_repeated_and_blocks_reactivation() {
        let directory = tempfile::tempdir().unwrap();
        let copy = tempfile::tempdir().unwrap();
        let mut manager = Manager::new(directory.path().into());
        manager.plugins.insert(
            "test.failed".into(),
            Generation {
                info: PluginInfo {
                    id: "test.failed".into(),
                    version: "1".into(),
                    generation: 1,
                    loaded_path: copy.path().into(),
                    description: String::new(),
                    metadata: Default::default(),
                    settings_values: Default::default(),
                    settings_error: None,
                    resident_code: false,
                    hooks: vec![],
                    dependencies: Vec::new(),
                },
                api: PluginApi {
                    abi_version: ABI_VERSION,
                    struct_size: std::mem::size_of::<PluginApi>() as u32,
                    flags: 0,
                    id: [0; 64],
                    version: [0; 32],
                    create: None,
                    destroy: Some(fails_to_destroy),
                    describe: None,
                    hook_count: None,
                    hook_info: None,
                    get_hook: None,
                },
                // This local test destructor never dereferences the opaque instance.
                instance: NonNull::dangling(),
                destroyed: false,
                host: None,
                hooks: vec![],
                markdown: None,
                syntax: None,
                matching: None,
                completion_frontend: None,
                snippet_provider: None,
                undo: None,
                language: None,
                semantic: None,
                editing: Default::default(),
                services_active: Arc::new(std::sync::RwLock::new(true)),
                library: None,
            },
        );
        assert!(matches!(
            manager.unload("test.failed"),
            Some(Event::Rejected { .. })
        ));
        assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
        assert!(manager.unload("test.failed").is_none());
        let manifest = crate::Manifest {
            plugins: vec![PluginSpec {
                dependencies: Vec::new(),
                install_reason: Default::default(),
                id: "test.failed".into(),
                library: "absent-library".into(),
                digest: "0".repeat(64),
                pack: false,
            }],
        };
        std::fs::write(
            directory.path().join(crate::MANIFEST),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        // SAFETY: the blocked ID is rejected before any artifact can be loaded.
        let events = unsafe { manager.load() }.unwrap();
        assert!(
            matches!(&events[..], [Event::Rejected { message, .. }] if message.contains("restart required"))
        );
        assert_eq!(DESTROYS.load(Ordering::SeqCst), 1);
    }
    #[test]
    fn load_except_skips_excluded_ids_and_available_lists_the_manifest() {
        let directory = tempfile::tempdir().unwrap();
        let mut manager = Manager::new(directory.path().into());
        assert!(manager.available().is_empty());
        let spec = |id: &str| PluginSpec {
            dependencies: Vec::new(),
            install_reason: Default::default(),
            id: id.into(),
            library: format!("{id}.dylib").into(),
            digest: "0".repeat(64),
            pack: false,
        };
        let manifest = crate::Manifest {
            plugins: vec![spec("test.one"), spec("test.two")],
        };
        std::fs::write(
            directory.path().join(crate::MANIFEST),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        assert_eq!(manager.available(), ["test.one", "test.two"]);
        // SAFETY: the libraries are absent, so nothing native is loaded.
        let events = unsafe { manager.load_except(&["test.two".into()]) }.unwrap();
        assert!(matches!(&events[..], [Event::Rejected { id, .. }] if id == "test.one"));
    }
}

#[cfg(test)]
pub(super) mod test_provider {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };
    pub struct Control {
        pub entered: mpsc::SyncSender<()>,
        pub release: mpsc::Receiver<()>,
        pub calls: Arc<AtomicUsize>,
        pub drops: Arc<AtomicUsize>,
    }
    unsafe extern "C" fn render(
        instance: *mut c_void,
        source: *const u8,
        length: usize,
        sink: RunSink,
        context: *mut c_void,
    ) -> i32 {
        // SAFETY: this fixture's instance and input are owned by the serial worker.
        let state = unsafe { &mut *instance.cast::<Control>() };
        state.calls.fetch_add(1, Ordering::SeqCst);
        let _ = state.entered.try_send(());
        if state
            .release
            .recv_timeout(std::time::Duration::from_secs(3))
            .is_err()
        {
            return INVALID;
        }
        let run = StyledRun {
            text: Bytes {
                data: source,
                length,
            },
            link: Bytes {
                data: b"".as_ptr(),
                length: 0,
            },
            flags: BOLD,
            heading: 0,
        };
        // SAFETY: the host callback consumes this initialized run before returning.
        unsafe { sink(context, &run, 1) }
    }
    unsafe extern "C" fn destroy(instance: *mut c_void) -> i32 {
        // SAFETY: fixture instance is destroyed once by its owner.
        let state = unsafe { Box::from_raw(instance.cast::<Control>()) };
        state.drops.fetch_add(1, Ordering::SeqCst);
        OK
    }
    pub fn manager_for(control: Control, id: &str) -> Manager {
        let mut manager = manager(control);
        let mut plugin = manager
            .plugins
            .remove(crate::DEFAULT_MARKDOWN_PROVIDER)
            .unwrap();
        plugin.info.id = id.into();
        manager.plugins.insert(id.into(), plugin);
        manager
    }
    pub fn manager(control: Control) -> Manager {
        let copy = tempfile::tempdir().unwrap();
        let mut manager = Manager::new(copy.path().into());
        manager.plugins.insert(
            crate::DEFAULT_MARKDOWN_PROVIDER.into(),
            Generation {
                info: PluginInfo {
                    id: crate::DEFAULT_MARKDOWN_PROVIDER.into(),
                    version: "1".into(),
                    generation: 1,
                    loaded_path: copy.path().into(),
                    description: String::new(),
                    metadata: Default::default(),
                    settings_values: Default::default(),
                    settings_error: None,
                    resident_code: false,
                    hooks: vec![],
                    dependencies: Vec::new(),
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
                instance: NonNull::new(Box::into_raw(Box::new(control)).cast()).unwrap(),
                destroyed: false,
                host: None,
                hooks: vec![],
                markdown: None,
                syntax: None,
                matching: None,
                completion_frontend: None,
                snippet_provider: None,
                undo: None,
                language: None,
                semantic: None,
                editing: Default::default(),
                services_active: Arc::new(std::sync::RwLock::new(true)),
                library: None,
            },
        );
        let plugin = manager
            .plugins
            .get_mut(crate::DEFAULT_MARKDOWN_PROVIDER)
            .unwrap();
        plugin.markdown = Some(MarkupApi {
            context: plugin.instance.as_ptr(),
            render,
        });
        manager
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.unload_all();
    }
}

#[cfg(test)]
mod markdown_tests {
    use super::*;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    fn provider() -> (Manager, mpsc::SyncSender<()>, Arc<AtomicUsize>) {
        let (entered, _) = mpsc::sync_channel(1);
        let (release, resume) = mpsc::sync_channel(4);
        let calls = Arc::new(AtomicUsize::new(0));
        let manager = test_provider::manager(test_provider::Control {
            entered,
            release: resume,
            calls: calls.clone(),
            drops: Default::default(),
        });
        (manager, release, calls)
    }

    #[test]
    fn providers_coexist_and_selection_is_exact_with_literal_fallback() {
        let (mut manager, release_default, default_calls) = provider();
        let (mut other, release_other, other_calls) = provider();
        let mut plugin = other
            .plugins
            .remove(crate::DEFAULT_MARKDOWN_PROVIDER)
            .unwrap();
        plugin.info.id = "community.markdown".into();
        manager.plugins.insert(plugin.info.id.clone(), plugin);
        release_default.send(()).unwrap();
        let default = manager.render_markdown("**source**").unwrap();
        assert!(default.runs[0].style.bold);
        assert_eq!(default_calls.load(Ordering::SeqCst), 1);
        assert_eq!(other_calls.load(Ordering::SeqCst), 0);
        release_other.send(()).unwrap();
        assert!(
            manager
                .render_markdown_with("community.markdown", "source")
                .unwrap()
                .runs[0]
                .style
                .bold
        );
        assert_eq!(other_calls.load(Ordering::SeqCst), 1);
        assert!(manager.render_markdown_with("invalid", "source").is_err());
        assert_eq!(
            manager
                .render_markdown_with("missing.provider", "**source**")
                .unwrap(),
            velk_markup::Document::plain("**source**")
        );
        manager
            .plugins
            .get_mut("community.markdown")
            .unwrap()
            .markdown = None;
        assert_eq!(
            manager
                .render_markdown_with("community.markdown", "source")
                .unwrap(),
            velk_markup::Document::plain("source")
        );
        manager.unload(crate::DEFAULT_MARKDOWN_PROVIDER);
        assert_eq!(
            manager.render_markdown("**source**").unwrap(),
            velk_markup::Document::plain("**source**")
        );
        assert_eq!(
            other_calls.load(Ordering::SeqCst),
            1,
            "unavailable selections never invoke another provider"
        );
    }
}
