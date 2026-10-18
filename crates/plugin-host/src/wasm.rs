//! Wasmtime transport for the bounded, synchronous matching contract.
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::Mutex,
};
use velk_plugin_contracts::commands::completion::matching::{Policy, Score};
use velk_plugin_wasm_api as api;
use wasmtime::{
    Cache, CacheConfig, Config, Engine, Instance, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder, TypedFunc,
};
pub(crate) const MAX_MODULE_BYTES: usize = 4 * 1024 * 1024;
const FUEL: u64 = 50_000_000;
const MAX_REQUEST_FUEL: u64 = 250_000_000;
const INITIAL_BUFFER_BYTES: usize = 4096;

fn request_fuel(input_bytes: usize) -> u64 {
    FUEL.saturating_add((input_bytes as u64).saturating_mul(256))
        .min(MAX_REQUEST_FUEL)
}
const MEMORY_BYTES: usize = 32 * 1024 * 1024;

const MAX_CACHED_MODULES: usize = 8;

pub(crate) struct Runtime {
    engine: Engine,
    modules: Mutex<VecDeque<(blake3::Hash, Module)>>,
    cache: Option<Cache>,
}

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub struct CacheStats {
    pub enabled: bool,
    pub hits: usize,
    pub misses: usize,
}

pub fn default_cache_directory() -> Option<PathBuf> {
    directories_next::ProjectDirs::from("", "", "velk")
        .map(|directories| directories.cache_dir().join("wasm"))
}

pub(crate) fn engine(directory: Option<&Path>) -> Result<Runtime, String> {
    let mut config = Config::new();
    config.consume_fuel(true).max_wasm_stack(512 * 1024);
    let cache = directory.and_then(|directory| {
        let mut cache = CacheConfig::new();
        cache.with_directory(directory)
            .with_files_total_size_soft_limit(256 * 1024 * 1024)
            .with_file_count_soft_limit(2048);
        match Cache::new(cache) {
            Ok(cache) => Some(cache),
            Err(error) => {
                tracing::warn!(%error, "Wasm compilation cache unavailable; compiling without persistence");
                None
            }
        }
    });
    config.cache(cache.clone());
    Ok(Runtime {
        engine: Engine::new(&config).map_err(|e| e.to_string())?,
        modules: Mutex::new(VecDeque::new()),
        cache,
    })
}

impl Runtime {
    pub(crate) fn cache_stats(&self) -> CacheStats {
        self.cache
            .as_ref()
            .map_or_else(CacheStats::default, |cache| CacheStats {
                enabled: true,
                hits: cache.cache_hits(),
                misses: cache.cache_misses(),
            })
    }

    pub(crate) fn prepare(&self, bytes: &[u8]) -> Result<(), String> {
        self.module(bytes).map(|_| ())
    }
    fn module(&self, bytes: &[u8]) -> Result<Module, String> {
        if bytes.len() > MAX_MODULE_BYTES {
            return Err("Wasm module exceeds 4 MiB".into());
        }
        let digest = blake3::hash(bytes);
        let mut modules = self
            .modules
            .lock()
            .map_err(|_| "Wasm module cache poisoned")?;
        if let Some(index) = modules.iter().position(|(key, _)| *key == digest) {
            let entry = modules.remove(index).unwrap();
            let module = entry.1.clone();
            modules.push_back(entry);
            return Ok(module);
        }
        let module = Module::new(&self.engine, bytes).map_err(|e| e.to_string())?;
        if module.imports().len() != 0 {
            return Err("matching guests may not import host services or memory".into());
        }
        if modules.len() == MAX_CACHED_MODULES {
            modules.pop_front();
        }
        modules.push_back((digest, module.clone()));
        Ok(module)
    }
}
struct Guest {
    store: Store<StoreLimits>,
    memory: Memory,
    rank: TypedFunc<(u32, u32, u32, u32), i32>,
    input: usize,
    capacity: usize,
    allocate: TypedFunc<u32, u32>,
}
impl Guest {
    fn new(runtime: &Runtime, bytes: &[u8]) -> Result<Self, String> {
        let module = runtime.module(bytes)?;
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_BYTES)
            .instances(1)
            .memories(1)
            .tables(1)
            .table_elements(4096)
            .build();
        let mut store = Store::new(&runtime.engine, limits);
        store.limiter(|limits| limits);
        store.set_fuel(FUEL).map_err(|e| e.to_string())?;
        let instance = Instance::new(&mut store, &module, &[]).map_err(|e| e.to_string())?;
        let version = instance
            .get_typed_func::<(), u32>(&mut store, "velk_abi_version")
            .map_err(|e| e.to_string())?;
        if version.call(&mut store, ()).map_err(|e| e.to_string())? != api::VERSION {
            return Err("incompatible Wasm ABI version".into());
        }
        let memory = instance
            .get_memory(&mut store, "memory")
            .ok_or("missing Wasm memory export")?;
        let allocate = instance
            .get_typed_func::<u32, u32>(&mut store, "velk_allocate")
            .map_err(|e| e.to_string())?;
        let input = allocate
            .call(&mut store, INITIAL_BUFFER_BYTES as u32)
            .map_err(|e| e.to_string())? as usize;
        let end = input
            .checked_add(INITIAL_BUFFER_BYTES)
            .ok_or("Wasm buffer overflow")?;
        if input == 0 || end > memory.data_size(&store) {
            return Err("invalid Wasm buffer allocation".into());
        }
        let rank = instance
            .get_typed_func(&mut store, "velk_matching_v1")
            .map_err(|e| e.to_string())?;
        Ok(Self {
            store,
            memory,
            rank,
            input,
            capacity: INITIAL_BUFFER_BYTES,
            allocate,
        })
    }
    fn scores(&mut self, query: &str, candidates: &[&str]) -> Result<Vec<Option<Score>>, String> {
        let length = api::request_size(query, candidates)?;
        let output_bytes = candidates.len() * api::SCORE_BYTES;
        let required = length + output_bytes;
        self.store
            .set_fuel(request_fuel(length))
            .map_err(|e| e.to_string())?;
        if required > self.capacity {
            let capacity = required.next_power_of_two().min(api::BUFFER_BYTES);
            let input = self
                .allocate
                .call(&mut self.store, capacity as u32)
                .map_err(|e| format!("Wasm buffer allocation trapped: {e:#}"))?
                as usize;
            let end = input.checked_add(capacity).ok_or("Wasm buffer overflow")?;
            if input == 0 || end > self.memory.data_size(&self.store) {
                return Err("invalid Wasm buffer allocation".into());
            }
            self.input = input;
            self.capacity = capacity;
        }
        let output = self.input + length;
        // Write directly into guest memory; do not retain its borrow across execution.
        let buffer = &mut self.memory.data_mut(&mut self.store)[self.input..output];
        api::write_request(buffer, query, candidates)?;
        let count = self
            .rank
            .call(
                &mut self.store,
                (
                    self.input as u32,
                    length as u32,
                    output as u32,
                    output_bytes as u32,
                ),
            )
            .map_err(|e| format!("Wasm matching trapped: {e:#}"))?;
        if count < 0 || count as usize != candidates.len() {
            return Err("invalid Wasm matching result count".into());
        }
        let end = output + count as usize * api::SCORE_BYTES;
        let bytes = self
            .memory
            .data(&self.store)
            .get(output..end)
            .ok_or("Wasm output outside memory")?;
        api::read_scores(bytes, count as usize).map_err(Into::into)
    }
}
/// One isolated store per provider generation. Retirement drops its entire memory.
/// Busy calls decline promptly; guest traps permanently retire this generation.
pub(crate) struct Matching {
    guest: Mutex<Option<Guest>>,
}
impl std::fmt::Debug for Matching {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WasmMatching")
    }
}
impl Matching {
    pub(crate) fn load(engine: &Runtime, bytes: &[u8]) -> Result<Self, String> {
        Ok(Self {
            guest: Mutex::new(Some(Guest::new(engine, bytes)?)),
        })
    }
    pub(crate) fn retire(&self) {
        if let Ok(mut guest) = self.guest.lock() {
            guest.take();
        }
    }
}
impl Policy for Matching {
    fn scores(&self, query: &str, candidates: &[&str]) -> Result<Vec<Option<Score>>, String> {
        let _scope =
            velk_diagnostics::Scope::new("plugin.wasm.matching", "velk_plugin_host", false);
        api::request_size(query, candidates)?;
        let mut state = self
            .guest
            .try_lock()
            .map_err(|_| "Wasm matching provider busy or poisoned")?;
        let guest = state.as_mut().ok_or("Wasm matching provider retired")?;
        let result = guest.scores(query, candidates);
        if let Err(error) = &result {
            tracing::warn!(%error, "Wasm matching generation retired");
            state.take();
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn module(body: &str) -> Vec<u8> {
        wat::parse_str(format!(
            r#"(module
            (memory (export "memory") 65)
            (func (export "velk_abi_version") (result i32) i32.const 1)
            (func (export "velk_allocate") (param i32) (result i32) i32.const 8)
            (func (export "velk_matching_v1") (param i32 i32 i32 i32) (result i32) {body}))"#
        ))
        .unwrap()
    }
    fn provider(body: &str) -> Matching {
        Matching::load(&engine(None).unwrap(), &module(body)).unwrap()
    }
    #[test]
    fn disk_cache_survives_new_engines_and_separates_compiler_settings() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = module("i32.const 0");
        let first = engine(Some(directory.path())).unwrap();
        first.prepare(&bytes).unwrap();
        assert_eq!(first.cache_stats().misses, 1);
        drop(first);
        let second = engine(Some(directory.path())).unwrap();
        second.prepare(&bytes).unwrap();
        assert_eq!(second.cache_stats().hits, 1);
        assert_eq!(second.cache_stats().misses, 0);

        let mut cache_config = CacheConfig::new();
        cache_config.with_directory(directory.path());
        let cache = Cache::new(cache_config).unwrap();
        let mut config = Config::new();
        config
            .consume_fuel(false)
            .max_wasm_stack(512 * 1024)
            .cache(Some(cache.clone()));
        let different = Engine::new(&config).unwrap();
        Module::new(&different, &bytes).unwrap();
        assert_eq!(cache.cache_hits(), 0);
        assert_eq!(cache.cache_misses(), 1);
    }

    #[test]
    fn corrupt_and_unavailable_disk_caches_fall_back_to_compilation() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = module("i32.const 0");
        let first = engine(Some(directory.path())).unwrap();
        first.prepare(&bytes).unwrap();
        drop(first);
        let compiler = std::fs::read_dir(directory.path().join("modules"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        let entry = std::fs::read_dir(compiler)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| path.is_file() && path.extension().is_none())
            .unwrap();
        std::fs::write(entry, b"invalid compressed cache entry").unwrap();
        let repaired = engine(Some(directory.path())).unwrap();
        repaired.prepare(&bytes).unwrap();
        assert_eq!(repaired.cache_stats().misses, 1);
        drop(repaired);
        let next = engine(Some(directory.path())).unwrap();
        next.prepare(&bytes).unwrap();
        assert_eq!(next.cache_stats().hits, 1);

        let file = tempfile::NamedTempFile::new().unwrap();
        let unavailable = engine(Some(file.path())).unwrap();
        unavailable.prepare(&bytes).unwrap();
        assert!(!unavailable.cache_stats().enabled);
    }

    #[test]
    fn preparation_checks_digests_deduplicates_and_does_not_run_start_functions() {
        let directory = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let bytes = wat::parse_str(
            r#"(module
            (func $start (loop $spin br $spin)) (start $start))"#,
        )
        .unwrap();
        std::fs::write(directory.path().join("matching.wasm"), &bytes).unwrap();
        let spec = crate::PluginSpec {
            id: "test.matching".into(),
            library: "matching.wasm".into(),
            digest: blake3::hash(&bytes).to_hex().to_string(),
            runtime: crate::PluginRuntime::WasmMatching {
                metadata: crate::PluginMetadata {
                    name: "Test".into(),
                    ..Default::default()
                },
                version: "0.0.0-dev".into(),
            },
            pack: false,
            dependencies: vec![],
            install_reason: Default::default(),
            interfaces: Default::default(),
        };
        let mut second = spec.clone();
        second.id = "test.second".into();
        let mut native = spec.clone();
        native.id = "test.native".into();
        native.library = "absent.dylib".into();
        native.runtime = crate::PluginRuntime::Native;
        std::fs::write(
            directory.path().join(crate::MANIFEST),
            serde_json::to_vec(&crate::Manifest {
                plugins: vec![spec, second, native],
            })
            .unwrap(),
        )
        .unwrap();
        let mut manager = crate::Manager::new(directory.path().into());
        assert!(
            manager
                .set_wasm_cache_directory(Some("relative".into()))
                .is_err()
        );
        manager
            .set_wasm_cache_directory(Some(cache.path().into()))
            .unwrap();
        assert_eq!(manager.prepare_wasm().unwrap(), 1);
        assert_eq!(manager.plugins().count(), 0);
        assert_eq!(manager.wasm_cache_stats().misses, 1);
        assert!(manager.set_wasm_cache_directory(None).is_err());
        std::fs::write(directory.path().join("matching.wasm"), b"changed artifact").unwrap();
        assert!(
            manager
                .prepare_wasm()
                .unwrap_err()
                .contains("digest mismatch")
        );
    }
    #[test]
    fn buffers_start_small_grow_only_when_needed_and_reject_invalid_reallocation() {
        let provider = provider("i32.const 0");
        assert_eq!(
            provider.guest.lock().unwrap().as_ref().unwrap().capacity,
            INITIAL_BUFFER_BYTES
        );
        provider.scores("", &[]).unwrap();
        assert_eq!(
            provider.guest.lock().unwrap().as_ref().unwrap().capacity,
            INITIAL_BUFFER_BYTES
        );
        provider.scores(&"x".repeat(api::MAX_TEXT), &[]).unwrap();
        assert_eq!(
            provider.guest.lock().unwrap().as_ref().unwrap().capacity,
            8192
        );
        provider.scores("", &[]).unwrap();
        assert_eq!(
            provider.guest.lock().unwrap().as_ref().unwrap().capacity,
            8192
        );

        let bytes = wat::parse_str(
            r#"(module
            (memory (export "memory") 1)
            (func (export "velk_abi_version") (result i32) i32.const 1)
            (func (export "velk_allocate") (param i32) (result i32)
                local.get 0 i32.const 4096 i32.gt_u
                if (result i32) i32.const -1 else i32.const 8 end)
            (func (export "velk_matching_v1") (param i32 i32 i32 i32) (result i32) i32.const 0))"#,
        )
        .unwrap();
        let provider = Matching::load(&engine(None).unwrap(), &bytes).unwrap();
        assert!(
            provider
                .scores(&"x".repeat(api::MAX_TEXT), &[])
                .unwrap_err()
                .contains("allocation")
        );
        assert!(provider.guest.lock().unwrap().is_none());
    }
    #[test]
    fn request_fuel_scales_with_input_and_has_a_hard_ceiling() {
        assert_eq!(request_fuel(0), FUEL);
        assert!(request_fuel(300_000) > FUEL);
        assert_eq!(request_fuel(api::MAX_INPUT), MAX_REQUEST_FUEL);
        assert_eq!(request_fuel(usize::MAX), MAX_REQUEST_FUEL);
        let provider = provider("(loop $spin br $spin) i32.const 0");
        let label = "x".repeat(api::MAX_TEXT);
        assert!(
            provider
                .scores("x", &[label.as_str(); 200])
                .unwrap_err()
                .contains("all fuel consumed")
        );
        assert!(provider.guest.lock().unwrap().is_none());
    }
    #[test]
    fn compiled_modules_are_reused_bounded_and_scoped_to_the_runtime() {
        let runtime = engine(None).unwrap();
        let bytes = module("i32.const 0");
        let compiled = runtime.module(&bytes).unwrap();
        assert!(Module::same(&compiled, &runtime.module(&bytes).unwrap()));
        let first = Matching::load(&runtime, &bytes).unwrap();
        let second = Matching::load(&runtime, &bytes).unwrap();
        assert_eq!(runtime.modules.lock().unwrap().len(), 1);
        first.retire();
        assert!(second.scores("", &[]).is_ok());
        for value in 1..=MAX_CACHED_MODULES {
            runtime
                .module(&module(&format!("i32.const {value}")))
                .unwrap();
        }
        let cache = runtime.modules.lock().unwrap();
        assert_eq!(cache.len(), MAX_CACHED_MODULES);
        assert!(!cache.iter().any(|(key, _)| *key == blake3::hash(&bytes)));
        drop(cache);
        let separate = engine(None).unwrap();
        assert!(separate.modules.lock().unwrap().is_empty());
        assert!(runtime.module(b"invalid module").is_err());
        assert_eq!(runtime.modules.lock().unwrap().len(), MAX_CACHED_MODULES);
    }
    #[test]
    fn reads_in_place_and_retirement_reclaims_store_with_stale_handles() {
        let provider = provider("(i64.store (local.get 2) (i64.const 1)) i32.const 1");
        assert_eq!(provider.scores("x", &["x"]).unwrap(), [Some([0; 4])]);
        provider.retire();
        assert!(provider.guest.lock().unwrap().is_none());
        assert!(provider.scores("x", &["x"]).is_err());
    }
    #[test]
    fn exhausted_fuel_invalid_count_and_flags_permanently_retire_generation() {
        for body in [
            "(loop $spin br $spin) i32.const 1",
            "i32.const -1",
            "(i64.store (local.get 2) (i64.const 2)) i32.const 1",
            "unreachable",
        ] {
            let provider = provider(body);
            let error = provider.scores("x", &["x"]).unwrap_err();
            if body.contains("$spin") {
                assert!(error.contains("all fuel consumed"), "{error}");
            }
            assert!(provider.guest.lock().unwrap().is_none());
            assert!(provider.scores("", &[]).is_err());
        }
    }
    #[test]
    fn caller_errors_and_contention_do_not_retire_healthy_generation() {
        let provider = provider("i32.const 0");
        assert!(provider.scores(&"x".repeat(4097), &[]).is_err());
        let lock = provider.guest.lock().unwrap();
        assert!(provider.scores("", &[]).unwrap_err().contains("busy"));
        drop(lock);
        assert_eq!(provider.scores("", &[]).unwrap(), []);
    }
    #[test]
    fn imports_versions_bad_allocations_start_loops_and_missing_exports_are_rejected() {
        let engine = engine(None).unwrap();
        let bytes = module("i32.const 0");
        // Different export implementations are compiled from text for each failure.
        for text in [
            "(module (import \"env\" \"call\" (func)))",
            "(module)",
            "(module (memory (export \"memory\") 65) (func (export \"velk_abi_version\") (result i32) i32.const 2))",
            "(module (memory (export \"memory\") 65) (func (export \"velk_abi_version\") (result i32) i32.const 1) (func (export \"velk_allocate\") (param i32) (result i32) i32.const -1))",
            "(module (func $start (loop $loop br $loop)) (start $start))",
        ] {
            assert!(Matching::load(&engine, &wat::parse_str(text).unwrap()).is_err());
        }
        assert!(Matching::load(&engine, &vec![0; MAX_MODULE_BYTES + 1]).is_err());
        assert!(Matching::load(&engine, &bytes).is_ok());
    }
    #[test]
    fn memory_growth_is_bounded() {
        let provider = provider(
            "i32.const 1024 memory.grow i32.const -1 i32.ne if unreachable end i32.const 0",
        );
        assert!(provider.scores("", &[]).is_ok());
    }
    #[test]
    #[allow(unsafe_code)]
    fn worker_reports_wasm_and_shutdown_retires_surviving_handles() {
        let directory = tempfile::tempdir().unwrap();
        let bytes = module("(i64.store (local.get 2) (i64.const 1)) i32.const 1");
        std::fs::write(directory.path().join("matching.wasm"), &bytes).unwrap();
        let spec = crate::PluginSpec {
            runtime: crate::PluginRuntime::WasmMatching {
                metadata: crate::PluginMetadata {
                    name: "Test matching".into(),
                    ..Default::default()
                },
                version: "0.0.0-dev".into(),
            },
            id: "test.matching".into(),
            library: "matching.wasm".into(),
            digest: blake3::hash(&bytes).to_hex().to_string(),
            pack: false,
            dependencies: vec![],
            install_reason: Default::default(),
            interfaces: Default::default(),
        };
        let manifest = crate::Manifest {
            plugins: vec![spec],
        };
        std::fs::write(
            directory.path().join(crate::MANIFEST),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        // SAFETY: the fixture contains only the checked Wasm module above.
        let runtime = unsafe { crate::Runtime::start(directory.path().into()).unwrap() };
        let client = runtime.client();
        let report = client.load().unwrap().recv_blocking().unwrap().unwrap();
        assert_eq!(report.loaded, 1);
        assert_eq!(report.plugins[0].id, "test.matching");
        let matching = report.matching["test.matching"].clone();
        assert_eq!(matching.scores("x", &["x"]).unwrap(), [Some([0; 4])]);
        let report = client
            .restart(None)
            .unwrap()
            .recv_blocking()
            .unwrap()
            .unwrap();
        assert!(matching.scores("x", &["x"]).is_err());
        let matching = report.matching["test.matching"].clone();
        drop(runtime);
        assert!(matching.scores("x", &["x"]).is_err());
    }
}
