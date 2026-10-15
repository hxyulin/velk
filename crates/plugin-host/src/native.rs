#![allow(unsafe_code)]
use crate::{Event, PluginSpec, read_manifest};
use libloading::Library;
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::c_void,
    path::PathBuf,
    ptr::NonNull,
};
use velk_plugin_api::*;

#[derive(Clone, Debug)]
pub struct PluginInfo {
    pub id: String,
    pub version: String,
    pub generation: u64,
    /// Private on-disk copy for this generation; removed after successful teardown.
    pub loaded_path: PathBuf,
    /// Host-owned copy; remains valid after the library is unloaded.
    pub description: String,
}
struct Generation {
    info: PluginInfo,
    api: PluginApi,
    instance: NonNull<c_void>,
    destroyed: bool,
    // Explicit drop order: instance, library, then its on-disk copy.
    library: Option<Library>,
    _copy: tempfile::TempDir,
}
impl Generation {
    /// Caller guarantees the plugin ABI and unload contract.
    unsafe fn load(
        directory: &std::path::Path,
        spec: PluginSpec,
        generation: u64,
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
        let library = unsafe { Library::new(&path) }.map_err(|e| e.to_string())?;
        let mut output = std::mem::MaybeUninit::<PluginApi>::uninit();
        // SAFETY: entry symbol/type and successful initialization are required by v1.
        let api = unsafe {
            let entry = library
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
        let id = metadata(&api.id)?;
        let version = metadata(&api.version)?;
        if id != spec.id {
            return Err(format!("expected {}, library reports {id}", spec.id));
        }
        let create = api.create.ok_or("missing create function")?;
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
                loaded_path: path,
                description: String::new(),
            },
            api,
            instance,
            destroyed: false,
            library: Some(library),
            _copy: copy,
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
/// plugin-owned values, so all v1 calls complete before replacement/destruction.
pub struct Manager {
    directory: PathBuf,
    plugins: BTreeMap<String, Generation>,
    next_generation: u64,
    blocked: BTreeSet<String>,
}
impl Manager {
    pub fn new(directory: PathBuf) -> Self {
        Self {
            directory,
            plugins: BTreeMap::new(),
            next_generation: 1,
            blocked: BTreeSet::new(),
        }
    }
    pub fn plugins(&self) -> impl Iterator<Item = &PluginInfo> {
        self.plugins.values().map(|p| &p.info)
    }
    pub fn describe(&mut self, id: &str) -> Result<String, String> {
        self.plugins
            .get_mut(id)
            .ok_or_else(|| format!("plugin {id} is not loaded"))?
            .describe()
    }
    /// Load inactive plugins listed on disk. Already-active IDs are left alone,
    /// even when a newer build is staged. Unload explicitly before replacing one.
    /// # Safety
    /// All libraries selected by the directory must implement the trusted v1 ABI,
    /// including its no-background-work/no-retained-borrows unload contract.
    pub unsafe fn load(&mut self) -> Result<Vec<Event>, String> {
        if !self.directory.join(crate::MANIFEST).exists() {
            return Ok(vec![]);
        }
        let manifest = read_manifest(&self.directory)?;
        let mut events = Vec::new();
        for spec in manifest.plugins {
            if self.plugins.contains_key(&spec.id) {
                continue;
            }
            let id = spec.id.clone();
            if self.blocked.contains(&id) {
                events.push(Event::Rejected {
                    id,
                    message: "previous shutdown failed; restart required".into(),
                });
                continue;
            }
            let generation = self.next_generation;
            self.next_generation += 1;
            // SAFETY: inherited from load's caller.
            match unsafe { Generation::load(&self.directory, spec, generation) } {
                Ok(candidate) => {
                    events.push(Event::Loaded(candidate.info.clone()));
                    self.plugins.insert(id, candidate);
                }
                Err(message) => events.push(Event::Rejected { id, message }),
            }
        }
        Ok(events)
    }
    pub fn unload(&mut self, id: &str) -> Option<Event> {
        let mut plugin = self.plugins.remove(id)?;
        if plugin.retire() {
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
                },
                api: PluginApi {
                    abi_version: ABI_VERSION,
                    struct_size: std::mem::size_of::<PluginApi>() as u32,
                    id: [0; 64],
                    version: [0; 32],
                    create: None,
                    destroy: Some(fails_to_destroy),
                    describe: None,
                },
                // This local test destructor never dereferences the opaque instance.
                instance: NonNull::dangling(),
                destroyed: false,
                library: None,
                _copy: copy,
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
