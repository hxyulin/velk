//! Exercise the real search DLL, including upload boundaries and worker unload.
use super::*;
use velk_search::{self as model, Query, Reply, Request};
#[allow(unsafe_code)]
pub(super) fn run(output: &Path) -> Result<(), String> {
    let temp = tempfile::tempdir().map_err(|e| e.to_string())?;
    let spec = stage_plugin(
        output,
        temp.path(),
        model::DEFAULT_BACKEND,
        "velk_plugin_search",
    )?;
    publish(temp.path(), vec![spec])?;
    let mut manager = velk_plugin_host::Manager::new(temp.path().into());
    // SAFETY: only the bundled locally compiled SDK provider is in the manifest.
    unsafe {
        manager.load()?;
    }
    let handle = manager.bind_hook(model::DEFAULT_BACKEND, model::HOOK)?;
    let call =
        |manager: &mut velk_plugin_host::Manager, request: Request| -> Result<Reply, String> {
            model::decode(&manager.call_hook(&handle, &model::encode(&request)?)?)
        };
    call(
        &mut manager,
        Request::Begin {
            id: 1,
            revision: 3,
            query: Query {
                text: "café".into(),
                ..Default::default()
            },
        },
    )?;
    for text in ["🦀 ca", "fé CAFÉ"] {
        call(
            &mut manager,
            Request::Append {
                id: 1,
                text: text.into(),
            },
        )?;
    }
    call(&mut manager, Request::Start { id: 1 })?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let result = loop {
        match call(&mut manager, Request::Poll { id: 1 })? {
            Reply::Ready(r) => break r,
            Reply::Pending if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(1))
            }
            other => return Err(format!("unexpected search response: {other:?}")),
        }
    };
    result.validate(1, 3, 11)?;
    assert_eq!(
        result.matches,
        vec![
            model::Match { start: 2, end: 6 },
            model::Match { start: 7, end: 11 }
        ]
    );
    call(&mut manager, Request::Cancel { id: 1 })?;
    assert!(call(&mut manager, Request::Poll { id: 1 }).is_err());
    call(
        &mut manager,
        Request::Begin {
            id: 2,
            revision: 4,
            query: Query {
                text: "missing".into(),
                ..Default::default()
            },
        },
    )?;
    for _ in 0..64 {
        call(
            &mut manager,
            Request::Append {
                id: 2,
                text: "a".repeat(model::CHUNK_BYTES),
            },
        )?;
    }
    call(&mut manager, Request::Start { id: 2 })?;
    let copy = manager.plugins().next().unwrap().loaded_path.clone();
    assert!(matches!(
        manager.unload_all().as_slice(),
        [velk_plugin_host::Event::Removed(_)]
    ));
    assert!(!copy.exists());
    assert_eq!(
        result.matches.len(),
        2,
        "results own their storage after unload"
    );
    unsafe {
        manager.load()?;
    }
    assert!(
        manager
            .call_hook(&handle, &model::encode(&Request::Poll { id: 2 })?)
            .is_err()
    );
    manager.unload_all();
    println!(
        "Search plugin smoke passed: Unicode/chunked upload, cancellation, owned results, worker unload, retired handles"
    );
    Ok(())
}
