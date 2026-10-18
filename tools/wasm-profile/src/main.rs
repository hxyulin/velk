use serde_json::json;
use std::{
    hint::black_box,
    io::{self, Write},
    path::{Path, PathBuf},
    time::Instant,
};
use velk_plugin_host::{Manager, Manifest, PluginMetadata, PluginRuntime, PluginSpec};
mod scaling;
const QUERIES: [&str; 9] = [
    "print",
    "pritn",
    "pront",
    "compleiton",
    "Completion",
    "界接",
    "zzzz",
    "",
    "create_workspace",
];
fn labels(n: usize, long: bool) -> Vec<String> {
    let stems = [
        "print",
        "println",
        "PRINT",
        "Completion",
        "completion_item",
        "create_workspace",
        "project_integer",
        "世界接口",
        "世界编辑器",
        "東京タワー",
        "with_capacity",
        "Result",
        "buffer_revision",
        "apply_edit",
        "zz_unrelated",
        "rename_symbol",
        "close_document",
        "get_file_type",
        "commit_history",
        "completion_provider",
    ];
    (0..n)
        .map(|i| {
            let base = if i < stems.len() {
                stems[i].to_owned()
            } else {
                format!("{}_{i:04}", stems[i % stems.len()])
            };
            if long {
                format!("{base}_{}", "long_nested_identifier_segment_".repeat(100))
            } else {
                base
            }
        })
        .collect()
}
fn emit(value: serde_json::Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}
fn handshake() {
    let mut line = String::new();
    io::stdin().read_line(&mut line).unwrap();
}
struct Loaded {
    manager: Manager,
    _directory: tempfile::TempDir,
}
fn native_id(source: &Path) -> &'static str {
    if source
        .file_name()
        .unwrap()
        .to_string_lossy()
        .contains("completion")
    {
        "velk.completion"
    } else {
        "velk.matching.profile"
    }
}
fn setup(source: &Path, wasm: bool, count: usize) -> Loaded {
    let directory = tempfile::tempdir().unwrap();
    let name = source.file_name().unwrap();
    let bytes = std::fs::read(source).unwrap();
    std::fs::write(directory.path().join(name), &bytes).unwrap();
    let plugins = (0..count)
        .map(|i| PluginSpec {
            runtime: if wasm {
                PluginRuntime::WasmMatching {
                    metadata: PluginMetadata {
                        name: "Wasm matching".into(),
                        ..Default::default()
                    },
                    version: "0.0.0-dev".into(),
                }
            } else {
                PluginRuntime::Native
            },
            interfaces: Default::default(),
            id: if wasm {
                format!("velk.matching.wasm{i}")
            } else {
                native_id(source).into()
            },
            library: name.into(),
            digest: blake3::hash(&bytes).to_hex().to_string(),
            pack: false,
            dependencies: vec![],
            install_reason: Default::default(),
        })
        .collect();
    std::fs::write(
        directory.path().join("plugins.json"),
        serde_json::to_vec(&Manifest { plugins }).unwrap(),
    )
    .unwrap();
    let mut manager = Manager::new(directory.path().into());
    manager.set_wasm_cache_directory(None).unwrap();
    Loaded {
        manager,
        _directory: directory,
    }
}
fn load(state: &mut Loaded, id: &str) -> u64 {
    let start = Instant::now();
    let events = unsafe { state.manager.load_selected(&[id.into()]) }.unwrap();
    assert!(
        events
            .iter()
            .all(|e| matches!(e, velk_plugin_host::Event::Loaded(_))),
        "{events:?}"
    );
    start.elapsed().as_nanos() as u64
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let command = &args[1];
    if command == "scaling" {
        scaling::run(&args[2..]);
        return;
    }
    let native = PathBuf::from(&args[2]);
    let wasm = PathBuf::from(&args[3]);
    if command == "lifecycle" {
        let is_wasm = args[4] == "wasm";
        let mut states = Vec::new();
        let mut state = setup(
            if is_wasm { &wasm } else { &native },
            is_wasm,
            if is_wasm { 10 } else { 1 },
        );
        emit(json!({"phase":"baseline"}));
        handshake();
        for i in 0..10 {
            let id = if is_wasm {
                format!("velk.matching.wasm{i}")
            } else {
                native_id(&native).into()
            };
            if !is_wasm && i > 0 {
                states.push(state);
                state = setup(&native, false, 1);
            }
            let elapsed = load(&mut state, &id);
            let provider = state.manager.matching_provider(&id).unwrap();
            let input = labels(1000, false);
            let refs: Vec<_> = input.iter().map(String::as_str).collect();
            for _ in 0..10 {
                black_box(provider.scores("pritn", &refs).unwrap());
            }
            drop(provider);
            emit(json!({"phase":"loaded","count":i+1,"load_ns":elapsed}));
            handshake();
        }
        state.manager.unload_all();
        for other in &mut states {
            other.manager.unload_all();
        }
        emit(json!({"phase":"unloaded"}));
        handshake();
        for cycle in 0..10 {
            let id = if is_wasm {
                "velk.matching.wasm0"
            } else {
                native_id(&native)
            };
            load(&mut state, id);
            let lease = state.manager.matching_provider(id).unwrap();
            state.manager.unload_all();
            assert!(lease.scores("", &[]).is_err());
            emit(json!({"phase":"retired","cycle":cycle+1}));
            handshake();
        }
        return;
    }
    let mut n = setup(&native, false, 1);
    let mut w = setup(&wasm, true, 1);
    load(&mut n, native_id(&native));
    load(&mut w, "velk.matching.wasm0");
    let native = n.manager.matching_provider(native_id(&native)).unwrap();
    let wasm = w.manager.matching_provider("velk.matching.wasm0").unwrap();
    if command == "boundaries" {
        for count in [100, 1000] {
            let input = labels(count, true);
            let refs: Vec<_> = input.iter().map(String::as_str).collect();
            for query in QUERIES {
                unsafe { w.manager.restart(Some("velk.matching.wasm0")) }.unwrap();
                let provider = w.manager.matching_provider("velk.matching.wasm0").unwrap();
                let start = Instant::now();
                let expected = native.scores(query, &refs).unwrap();
                let native_ns = start.elapsed().as_nanos() as u64;
                let start = Instant::now();
                let result = provider.scores(query, &refs);
                let wasm_ns = start.elapsed().as_nanos() as u64;
                let (ok, error) = match result {
                    Ok(scores) => (scores == expected, None),
                    Err(error) => (false, Some(error)),
                };
                emit(
                    json!({"kind":"boundary", "count":count, "query":query, "native_ns":native_ns, "wasm_ns":wasm_ns, "ok":ok, "error":error}),
                );
            }
        }
        return;
    }
    let mut measurements = Vec::new();
    let corpus: Vec<String> = serde_json::from_slice(&std::fs::read(&args[4]).unwrap()).unwrap();
    let workloads = [
        ("synthetic", labels(10, false), 3000),
        ("synthetic", labels(100, false), 1000),
        ("synthetic", labels(1000, false), 400),
        ("repository", corpus, 400),
    ];
    for (dataset, input, samples) in workloads {
        let count = input.len();
        let queries: Vec<String> = if dataset == "repository" {
            [
                input[0].clone(),
                input[count / 2].clone(),
                input[count - 1].clone(),
            ]
            .into_iter()
            .flat_map(|word| {
                let prefix = word.chars().take(4).collect::<String>();
                let mut typo = word.chars().collect::<Vec<_>>();
                if typo.len() > 2 {
                    typo.swap(0, 1);
                }
                [word, prefix, typo.into_iter().collect()]
            })
            .collect()
        } else {
            QUERIES.into_iter().map(str::to_owned).collect()
        };
        let refs: Vec<_> = input.iter().map(String::as_str).collect();
        for query in &queries {
            let expected = velk_completion_matching::scores(query, &refs);
            assert_eq!(native.scores(query, &refs).unwrap(), expected);
            assert_eq!(wasm.scores(query, &refs).unwrap(), expected);
        }
        for trial in 0..3 {
            for round in 0..samples + 100 {
                for i in 0..3 {
                    let mode = (round + trial + i) % 3;
                    let query = queries[round % queries.len()].as_str();
                    let start = Instant::now();
                    let result = match mode {
                        0 => velk_completion_matching::scores(black_box(query), black_box(&refs)),
                        1 => native.scores(black_box(query), black_box(&refs)).unwrap(),
                        _ => wasm.scores(black_box(query), black_box(&refs)).unwrap(),
                    };
                    black_box(&result);
                    let elapsed = start.elapsed().as_nanos() as u64;
                    drop(result);
                    if round >= 100 {
                        measurements.push(json!({"kind":"call","dataset":dataset,"count":count,"trial":trial,"mode":(["linked","native","wasm"][mode]),"query":round%9,"ns":elapsed}));
                    }
                }
            }
        }
    }
    for sample in measurements {
        emit(sample);
    }
    let input = labels(100, true);
    let refs: Vec<_> = input.iter().map(String::as_str).collect();
    for query in QUERIES {
        unsafe { w.manager.restart(Some("velk.matching.wasm0")) }.unwrap();
        let provider = w.manager.matching_provider("velk.matching.wasm0").unwrap();
        let start = Instant::now();
        let result = provider.scores(query, &refs);
        let elapsed = start.elapsed().as_nanos() as u64;
        emit(match result {
            Ok(scores) => {
                json!({"kind":"stress","query":query,"ns":elapsed,"ok":scores==native.scores(query,&refs).unwrap()})
            }
            Err(error) => {
                json!({"kind":"stress","query":query,"ns":elapsed,"ok":false,"error":error})
            }
        });
    }
}
