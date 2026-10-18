use super::{Loaded, emit, handshake, labels};
use serde_json::json;
use std::{hint::black_box, mem::MaybeUninit, path::Path, time::Instant};
use velk_plugin_host::{Manager, Manifest, PluginMetadata, PluginRuntime, PluginSpec};

#[derive(Clone, Copy)]
struct Cpu {
    user_ns: u64,
    system_ns: u64,
}

fn cpu() -> Cpu {
    let mut usage = MaybeUninit::<libc::rusage>::uninit();
    // SAFETY: getrusage writes the complete rusage structure on success.
    assert_eq!(
        unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) },
        0
    );
    // SAFETY: the successful call above initialized usage.
    let usage = unsafe { usage.assume_init() };
    let nanos =
        |time: libc::timeval| time.tv_sec as u64 * 1_000_000_000 + time.tv_usec as u64 * 1000;
    Cpu {
        user_ns: nanos(usage.ru_utime),
        system_ns: nanos(usage.ru_stime),
    }
}

impl Cpu {
    fn since(self, before: Self) -> serde_json::Value {
        json!({"user_ns":self.user_ns.saturating_sub(before.user_ns),
            "system_ns":self.system_ns.saturating_sub(before.system_ns)})
    }
}

fn measure(
    mut call: impl FnMut(usize, &str) -> Vec<Option<[u64; 4]>>,
    providers: usize,
    rounds: usize,
) -> serde_json::Value {
    let count = providers * rounds;
    let mut times = Vec::with_capacity(count);
    let before = cpu();
    let start = Instant::now();
    for index in 0..count {
        let provider = index % providers;
        let query = "pritn";
        let begin = Instant::now();
        let scores = call(provider, query);
        black_box(&scores);
        times.push(begin.elapsed().as_nanos() as u64);
        drop(scores);
    }
    let wall_ns = start.elapsed().as_nanos() as u64;
    json!({"wall_ns":wall_ns,"cpu":cpu().since(before),"calls":count,"samples_ns":times})
}

pub(super) fn run(args: &[String]) {
    let mode = &args[0];
    assert!(["native", "wasm_shared", "wasm_distinct"].contains(&mode.as_str()));
    let count: usize = args[1].parse().unwrap();
    assert!((1..=50).contains(&count));
    let artifacts = Path::new(&args[2]);
    let directory = tempfile::tempdir().unwrap();
    let mut specs = Vec::new();
    for index in 0..count {
        let filename = if mode == "native" {
            format!("native_{index:02}.{}", std::env::consts::DLL_EXTENSION)
        } else if mode == "wasm_shared" {
            "module_00.wasm".into()
        } else {
            format!("module_{index:02}.wasm")
        };
        let bytes = std::fs::read(artifacts.join(&filename)).unwrap();
        std::fs::write(directory.path().join(&filename), &bytes).unwrap();
        specs.push(PluginSpec {
            runtime: if mode == "native" {
                PluginRuntime::Native
            } else {
                PluginRuntime::WasmMatching {
                    metadata: PluginMetadata {
                        name: "Scaling matching fixture".into(),
                        ..Default::default()
                    },
                    version: "0.0.0-dev".into(),
                }
            },
            id: format!("velk.matching.scale{index:02}"),
            library: filename.into(),
            digest: blake3::hash(&bytes).to_hex().to_string(),
            pack: false,
            dependencies: vec![],
            install_reason: Default::default(),
            interfaces: Default::default(),
        });
    }
    let ids: Vec<_> = specs.iter().map(|spec| spec.id.clone()).collect();
    std::fs::write(
        directory.path().join("plugins.json"),
        serde_json::to_vec(&Manifest { plugins: specs }).unwrap(),
    )
    .unwrap();
    let mut state = Loaded {
        manager: Manager::new(directory.path().into()),
        _directory: directory,
    };
    state
        .manager
        .set_wasm_cache_directory(
            args.get(3)
                .filter(|path| path.as_str() != "off")
                .map(Into::into),
        )
        .unwrap();
    if args.get(4).is_some_and(|value| value == "prepare") {
        let before = cpu();
        let start = Instant::now();
        let count = state.manager.prepare_wasm().unwrap();
        emit(
            json!({"phase":"prepared","count":count,"wall_ns":start.elapsed().as_nanos() as u64,
            "cpu":cpu().since(before),"cache":state.manager.wasm_cache_stats()}),
        );
        return;
    }
    let input = labels(1000, false);
    let candidates: Vec<_> = input.iter().map(String::as_str).collect();
    let expected: Vec<_> = super::QUERIES
        .iter()
        .map(|query| velk_completion_matching::scores(query, &candidates))
        .collect();
    emit(json!({"phase":"baseline"}));
    handshake();

    let before = cpu();
    let start = Instant::now();
    // SAFETY: only the locally built, verified benchmark fixtures are staged here.
    let events = unsafe { state.manager.load_selected(&ids) }.unwrap();
    let load = json!({"wall_ns":start.elapsed().as_nanos() as u64,"cpu":cpu().since(before)});
    assert_eq!(events.len(), count, "{events:?}");
    assert!(
        events
            .iter()
            .all(|event| matches!(event, velk_plugin_host::Event::Loaded(_))),
        "{events:?}"
    );
    emit(json!({"phase":"loaded","load":load,"cache":state.manager.wasm_cache_stats()}));
    handshake();

    let providers: Vec<_> = ids
        .iter()
        .map(|id| state.manager.matching_provider(id).unwrap())
        .collect();
    for provider in &providers {
        for (query, expected) in super::QUERIES.iter().zip(&expected) {
            assert_eq!(&provider.scores(query, &candidates).unwrap(), expected);
        }
    }
    emit(json!({"phase":"warmed","parity":true}));
    handshake();
    let before = cpu();
    let start = Instant::now();
    std::thread::sleep(std::time::Duration::from_millis(200));
    let idle = json!({"wall_ns":start.elapsed().as_nanos() as u64,"cpu":cpu().since(before)});
    let single = measure(
        |_, query| providers[0].scores(query, &candidates).unwrap(),
        1,
        1000,
    );
    let fanout = measure(
        |index, query| providers[index].scores(query, &candidates).unwrap(),
        count,
        50,
    );
    emit(json!({"phase":"runtime","idle":idle,"single":single,"fanout":fanout}));
    handshake();
    state.manager.unload_all();
    assert!(
        providers
            .iter()
            .all(|provider| provider.scores("", &[]).is_err())
    );
    emit(json!({"phase":"unloaded"}));
    handshake();
}
