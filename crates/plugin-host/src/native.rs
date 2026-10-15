#![allow(unsafe_code)]
use crate::{Event, PluginSpec, read_manifest};
mod semantic;
use libloading::Library;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    path::PathBuf,
    ptr::NonNull,
    sync::Arc,
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
    /// Logical unload releases sessions/instances, while this build stays mapped.
    pub resident_code: bool,
}
struct Code {
    library: Library,
    path: PathBuf,
    _copy: tempfile::TempDir,
}
struct Generation {
    info: PluginInfo,
    api: PluginApi,
    instance: NonNull<c_void>,
    destroyed: bool,
    // Explicit drop order: instance, library, then its on-disk copy.
    library: Option<Arc<Code>>,
}
impl Generation {
    /// Caller guarantees the plugin ABI and unload contract.
    unsafe fn load(
        directory: &std::path::Path,
        spec: PluginSpec,
        generation: u64,
        cached: Option<Arc<Code>>,
    ) -> Result<Self, String> {
        let source = directory.join(&spec.library);
        let copy = tempfile::Builder::new()
            .prefix("velk-plugin-")
            .tempdir()
            .map_err(|e| e.to_string())?;
        let filename = source.file_name().ok_or("library has no filename")?;
        let path = copy.path().join(filename);
        std::fs::copy(&source, &path).map_err(|e| format!("{}: {e}", source.display()))?;
        let mut hash = blake3::Hasher::new();
        hash.update_reader(std::fs::File::open(&path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        if hash.finalize().to_hex().as_str() != spec.digest {
            return Err("library digest mismatch".into());
        }
        // SAFETY: caller explicitly trusts libraries selected by this manifest.
        let code = match cached {
            Some(code) => code,
            None => Arc::new(Code {
                library: unsafe { Library::new(&path) }.map_err(|e| e.to_string())?,
                path,
                _copy: copy,
            }),
        };
        let mut output = std::mem::MaybeUninit::<PluginApi>::uninit();
        // SAFETY: entry symbol/type and successful initialization are required by v1.
        let api = unsafe {
            let entry = code
                .library
                .get::<Entry>(ENTRY_SYMBOL)
                .map_err(|e| e.to_string())?;
            let status = entry(
                ABI_VERSION,
                output.as_mut_ptr(),
                std::mem::size_of::<PluginApi>(),
            );
            if status != OK {
                return Err(format!("ABI query failed with status {status}"));
            }
            output.assume_init()
        };
        if api.abi_version != ABI_VERSION
            || api.struct_size as usize != std::mem::size_of::<PluginApi>()
        {
            return Err("incompatible plugin API layout".into());
        }
        if api.flags & !RESIDENT_CODE != 0 {
            return Err("unknown plugin lifetime flags".into());
        }
        let id = metadata(&api.id)?;
        let version = metadata(&api.version)?;
        if id != spec.id {
            return Err(format!("expected {}, library reports {id}", spec.id));
        }
        let create = api.create.ok_or("missing create function")?;
        let semantic_count = usize::from(api.semantic_create.is_some())
            + usize::from(api.semantic_input.is_some())
            + usize::from(api.semantic_destroy.is_some());
        if semantic_count != 0 && semantic_count != 3 {
            return Err("incomplete semantic session interface".into());
        }
        if api.destroy.is_none() || api.describe.is_none() {
            return Err("missing lifecycle function".into());
        }
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
                resident_code: api.flags & RESIDENT_CODE != 0,
            },
            api,
            instance,
            destroyed: false,
            library: Some(code),
        };
        loaded.info.description = loaded.describe()?;
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
        let render = self
            .api
            .render_markdown
            .ok_or("provider has no Markdown capability")?;
        let mut output = Output {
            document: Default::default(),
            emitted: false,
            valid: false,
        };
        // SAFETY: exclusive live instance, borrowed UTF-8 input, synchronous callback;
        // the library remains owned by self until all output has been copied.
        let status = unsafe {
            render(
                self.instance.as_ptr(),
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
    fn retire(&mut self) -> bool {
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
pub struct Manager {
    directory: PathBuf,
    plugins: BTreeMap<String, Generation>,
    next_generation: u64,
    blocked: BTreeSet<String>,
    sessions: BTreeMap<u64, semantic::DocumentSession>,
    resident_code: BTreeMap<String, Arc<Code>>,
}
impl Manager {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            plugins: BTreeMap::new(),
            next_generation: 1,
            blocked: BTreeSet::new(),
            sessions: BTreeMap::new(),
            resident_code: BTreeMap::new(),
        }
    }
    pub fn plugins(&self) -> impl Iterator<Item = &PluginInfo> {
        self.plugins
            .values()
            .filter(|p| !p.destroyed)
            .map(|p| &p.info)
    }
    pub fn describe(&mut self, id: &str) -> Result<String, String> {
        self.plugins
            .get_mut(id)
            .ok_or_else(|| format!("plugin {id} is not loaded"))?
            .describe()
    }
    pub fn render_markdown(&mut self, source: &str) -> Result<velk_markup::Document, String> {
        if source.len() > MAX_INPUT_BYTES {
            return Err("Markdown input exceeds 1 MiB".into());
        }
        let count = self
            .plugins
            .values()
            .filter(|p| !p.destroyed && p.api.render_markdown.is_some())
            .count();
        if count > 1 {
            return Err(
                "multiple Markdown providers loaded; provider selection is not implemented yet"
                    .into(),
            );
        }
        match self
            .plugins
            .values_mut()
            .find(|p| !p.destroyed && p.api.render_markdown.is_some())
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
    pub unsafe fn load(&mut self) -> Result<Vec<Event>, String> {
        if !self.directory.join(crate::MANIFEST).exists() {
            return Ok(vec![]);
        }
        let manifest = read_manifest(&self.directory)?;
        let mut events = Vec::new();
        for spec in manifest.plugins {
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
            let generation = self.next_generation;
            self.next_generation += 1;
            // SAFETY: inherited from load's caller.
            let digest = spec.digest.clone();
            let cached = self.resident_code.get(&digest).cloned();
            match unsafe { Generation::load(&self.directory, spec, generation, cached) } {
                Ok(candidate) => {
                    if candidate.api.flags & RESIDENT_CODE != 0 {
                        self.resident_code.entry(digest).or_insert_with(|| {
                            let code = candidate.library.as_ref().unwrap().clone();
                            // Process-wide hooks can still call this code after all
                            // instances/runtimes are gone. One permanent lease per
                            // build, reused across ordinary unload/load cycles.
                            std::mem::forget(code.clone());
                            code
                        });
                    }
                    events.push(Event::Loaded(candidate.info.clone()));
                    self.plugins.insert(id, candidate);
                }
                Err(message) => events.push(Event::Rejected { id, message }),
            }
        }
        Ok(events)
    }
    pub fn unload(&mut self, id: &str) -> Option<Event> {
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
        let ids: Vec<_> = self.plugins.keys().cloned().collect();
        ids.into_iter().filter_map(|id| self.unload(&id)).collect()
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
                    resident_code: false,
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
                    render_markdown: None,
                    semantic_create: None,
                    semantic_input: None,
                    semantic_destroy: None,
                },
                // This local test destructor never dereferences the opaque instance.
                instance: NonNull::dangling(),
                destroyed: false,
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
                id: "test.failed".into(),
                library: "absent-library".into(),
                digest: "0".repeat(64),
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
    pub fn manager(control: Control) -> Manager {
        let copy = tempfile::tempdir().unwrap();
        let mut manager = Manager::new(copy.path().into());
        manager.plugins.insert(
            "test.markdown".into(),
            Generation {
                info: PluginInfo {
                    id: "test.markdown".into(),
                    version: "1".into(),
                    generation: 1,
                    loaded_path: copy.path().into(),
                    description: String::new(),
                    resident_code: false,
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
                    render_markdown: Some(render),
                    semantic_create: None,
                    semantic_input: None,
                    semantic_destroy: None,
                },
                instance: NonNull::new(Box::into_raw(Box::new(control)).cast()).unwrap(),
                destroyed: false,
                library: None,
            },
        );
        manager
    }
}

impl Drop for Manager {
    fn drop(&mut self) {
        self.unload_all();
    }
}
