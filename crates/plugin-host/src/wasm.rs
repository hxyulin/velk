//! Wasmtime transport for the bounded, synchronous matching contract.
use std::sync::Mutex;
use velk_plugin_contracts::commands::completion::matching::{Policy, Score};
use velk_plugin_wasm_api as api;
use wasmtime::{
    Config, Engine, Instance, Memory, Module, Store, StoreLimits, StoreLimitsBuilder, TypedFunc,
};
pub(crate) const MAX_MODULE_BYTES: usize = 4 * 1024 * 1024;
const FUEL: u64 = 50_000_000;
const MEMORY_BYTES: usize = 32 * 1024 * 1024;

pub(crate) fn engine() -> Result<Engine, String> {
    let mut config = Config::new();
    config.consume_fuel(true).max_wasm_stack(512 * 1024);
    Engine::new(&config).map_err(|e| e.to_string())
}
struct Guest {
    store: Store<StoreLimits>,
    memory: Memory,
    rank: TypedFunc<(u32, u32, u32, u32), i32>,
    input: usize,
    output: usize,
}
impl Guest {
    fn new(engine: &Engine, bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_MODULE_BYTES {
            return Err("Wasm module exceeds 4 MiB".into());
        }
        let module = Module::new(engine, bytes).map_err(|e| e.to_string())?;
        if module.imports().len() != 0 {
            return Err("matching guests may not import host services or memory".into());
        }
        let limits = StoreLimitsBuilder::new()
            .memory_size(MEMORY_BYTES)
            .instances(1)
            .memories(1)
            .tables(1)
            .table_elements(4096)
            .build();
        let mut store = Store::new(engine, limits);
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
            .call(&mut store, api::BUFFER_BYTES as u32)
            .map_err(|e| e.to_string())? as usize;
        let output = input
            .checked_add(api::MAX_INPUT)
            .ok_or("Wasm buffer overflow")?;
        let end = input
            .checked_add(api::BUFFER_BYTES)
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
            output,
        })
    }
    fn scores(&mut self, query: &str, candidates: &[&str]) -> Result<Vec<Option<Score>>, String> {
        self.store.set_fuel(FUEL).map_err(|e| e.to_string())?;
        // Write directly into guest memory; do not retain its borrow across execution.
        let buffer = &mut self.memory.data_mut(&mut self.store)[self.input..self.output];
        let length = api::write_request(buffer, query, candidates)?;
        let count = self
            .rank
            .call(
                &mut self.store,
                (
                    self.input as u32,
                    length as u32,
                    self.output as u32,
                    api::MAX_OUTPUT as u32,
                ),
            )
            .map_err(|e| format!("Wasm matching trapped: {e}"))?;
        if count < 0 || count as usize != candidates.len() {
            return Err("invalid Wasm matching result count".into());
        }
        let end = self.output + count as usize * api::SCORE_BYTES;
        let bytes = self
            .memory
            .data(&self.store)
            .get(self.output..end)
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
    pub(crate) fn load(engine: &Engine, bytes: &[u8]) -> Result<Self, String> {
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
        Matching::load(&engine().unwrap(), &module(body)).unwrap()
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
            assert!(provider.scores("x", &["x"]).is_err());
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
        let engine = engine().unwrap();
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
