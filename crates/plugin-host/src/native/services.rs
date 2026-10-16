use super::*;
use crate::services::Bridge;
use std::{
    io::Read,
    sync::atomic::{AtomicBool, Ordering},
};
use velk_plugin_contracts::host::{self, Request, Response};

pub(super) struct Endpoint {
    bridge: Arc<Bridge>,
    plugin: String,
    generation: u64,
    capabilities: BTreeSet<String>,
    pub active: Arc<AtomicBool>,
}
impl Endpoint {
    fn request(&self, request: Request) -> Result<Response, String> {
        if !self.active.load(Ordering::Acquire) || self.bridge.closed.load(Ordering::Acquire) {
            return Err("host service retired".into());
        }
        request.validate()?;
        if !self.capabilities.contains(request.capability()) {
            return Err(format!("plugin did not declare {}", request.capability()));
        }
        let after = match &request {
            Request::ReadDirectory { after, .. } => after.clone(),
            _ => None,
        };
        let directory = matches!(request, Request::ReadDirectory { .. });
        match request {
            Request::PublishDiagnostics {
                client,
                publication,
            } => {
                self.bridge.publish_diagnostics(
                    &self.plugin,
                    self.generation,
                    client,
                    self.active.clone(),
                    publication,
                )?;
                Ok(Response::Accepted)
            }
            Request::ClearDiagnostics { client } => {
                self.bridge
                    .clear_diagnostics(&self.plugin, self.generation, client);
                Ok(Response::Accepted)
            }
            Request::Workspace => self
                .workspace()
                .map(|workspace| Response::Workspace { workspace }),
            Request::ReadFile { path } | Request::ReadDirectory { path, .. } => {
                let workspace = self.workspace()?;
                let root = workspace
                    .project_root
                    .ok_or("no project folder is selected")?;
                read_project_page(&root, &path, directory, after.as_deref())
            }
            _ => self.dispatch(request),
        }
    }
    fn workspace(&self) -> Result<host::Workspace, String> {
        if let Some(workspace) = self.bridge.workspace.lock().unwrap().clone() {
            return Ok(workspace);
        }
        match self.dispatch(Request::Workspace)? {
            Response::Workspace { workspace } => Ok(workspace),
            _ => Err("invalid workspace reply".into()),
        }
    }
    fn dispatch(&self, request: Request) -> Result<Response, String> {
        self.bridge
            .request(&self.plugin, self.generation, self.active.clone(), request)
    }
    fn table(self: &Arc<Self>) -> HostServices {
        HostServices {
            context: Arc::as_ptr(self).cast_mut().cast(),
            retain,
            release,
            request,
        }
    }
}
unsafe extern "C" fn retain(context: *mut c_void) {
    // SAFETY: every table owns or borrows an existing strong host reference.
    unsafe { Arc::increment_strong_count(context.cast::<Endpoint>()) };
}
unsafe extern "C" fn release(context: *mut c_void) {
    unsafe { Arc::decrement_strong_count(context.cast::<Endpoint>()) };
}
unsafe extern "C" fn request(
    context: *mut c_void,
    input: Bytes,
    sink: ByteSink,
    reply: *mut c_void,
) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if context.is_null() || input.length > MAX_INPUT_BYTES || input.data.is_null() {
            return INVALID;
        }
        let endpoint = unsafe { &*context.cast::<Endpoint>() };
        let input = unsafe { std::slice::from_raw_parts(input.data, input.length) };
        let result = serde_json::from_slice(input)
            .map_err(|e| e.to_string())
            .and_then(|request| endpoint.request(request))
            .and_then(|response| serde_json::to_vec(&response).map_err(|e| e.to_string()))
            .and_then(|bytes| {
                if bytes.len() <= MAX_OUTPUT_BYTES {
                    Ok(bytes)
                } else {
                    Err("host response exceeds 4 MiB".into())
                }
            });
        let (bytes, status) = match result {
            Ok(bytes) => (bytes, OK),
            Err(error) => (error.into_bytes(), INVALID),
        };
        unsafe {
            sink(
                reply,
                Bytes {
                    data: bytes.as_ptr(),
                    length: bytes.len(),
                },
                status,
            )
        }
    }))
    .unwrap_or(PANICKED)
}
impl Generation {
    pub(super) fn attach_host(&mut self, bridge: Arc<Bridge>) -> Result<(), String> {
        let Some(index) = self.hooks.iter().position(|h| h.info.name == HOST_HOOK) else {
            return Ok(());
        };
        let api: HostApi = unsafe { self.table(index, HOST_HOOK, 1)? };
        let endpoint = Arc::new(Endpoint {
            bridge,
            plugin: self.info.id.clone(),
            generation: self.info.generation,
            capabilities: self
                .info
                .metadata
                .capabilities
                .iter()
                .map(|c| c.id.clone())
                .collect(),
            active: Arc::new(AtomicBool::new(true)),
        });
        self.host = Some(endpoint.clone());
        let status = unsafe { (api.attach)(api.context, &endpoint.table()) };
        if status != OK {
            return Err(format!("host service attachment failed ({status})"));
        }
        Ok(())
    }
}
#[cfg(test)]
fn read_project(
    root: &std::path::Path,
    path: &std::path::Path,
    directory: bool,
) -> Result<Response, String> {
    read_project_page(root, path, directory, None)
}
fn read_project_page(
    root: &std::path::Path,
    path: &std::path::Path,
    directory: bool,
    after: Option<&std::path::Path>,
) -> Result<Response, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let target = root.join(path).canonicalize().map_err(|e| e.to_string())?;
    if !target.starts_with(&root) {
        return Err("path is outside the project root".into());
    }
    if directory {
        let mut paths = std::fs::read_dir(&target)
            .map_err(|e| e.to_string())?
            .map(|entry| entry.map(|entry| entry.path()).map_err(|e| e.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        paths.sort();
        let entries = paths
            .into_iter()
            .filter(|path| {
                after.is_none_or(|after| path.strip_prefix(&root).is_ok_and(|path| path > after))
            })
            .take(host::MAX_DIRECTORY_ENTRIES)
            .map(|path| {
                let ty = std::fs::symlink_metadata(&path)
                    .map_err(|e| e.to_string())?
                    .file_type();
                Ok(host::DirectoryEntry {
                    path: path
                        .strip_prefix(&root)
                        .map_err(|e| e.to_string())?
                        .to_path_buf(),
                    kind: if ty.is_symlink() {
                        host::FileKind::Symlink
                    } else if ty.is_dir() {
                        host::FileKind::Directory
                    } else if ty.is_file() {
                        host::FileKind::File
                    } else {
                        host::FileKind::Other
                    },
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Response::Directory { entries })
    } else {
        if !std::fs::metadata(&target)
            .map_err(|e| e.to_string())?
            .is_file()
        {
            return Err("project read requires a regular file".into());
        }
        let file = std::fs::File::open(target).map_err(|e| e.to_string())?;
        if !file.metadata().map_err(|e| e.to_string())?.is_file() {
            return Err("project read requires a regular file".into());
        }
        let mut bytes = Vec::new();
        file.take(host::MAX_FILE_BYTES as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > host::MAX_FILE_BYTES {
            return Err("project file exceeds 1 MiB".into());
        }
        Ok(Response::File { bytes })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn project_reads_are_bounded_and_resolve_only_inside_the_project() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(root.path().join("hello.txt"), "世界").unwrap();
        let Response::File { bytes } =
            read_project(root.path(), std::path::Path::new("hello.txt"), false).unwrap()
        else {
            panic!()
        };
        assert_eq!(bytes, "世界".as_bytes());
        assert!(
            read_project(root.path(), outside.path(), false)
                .unwrap_err()
                .contains("outside")
        );
        assert!(
            read_project(root.path(), std::path::Path::new(".."), true)
                .unwrap_err()
                .contains("outside")
        );
        let Response::Directory { entries } =
            read_project(root.path(), std::path::Path::new("."), true).unwrap()
        else {
            panic!()
        };
        assert_eq!(entries[0].path, std::path::Path::new("hello.txt"));
        assert_eq!(entries[0].kind, host::FileKind::File);
        std::fs::write(root.path().join("large"), vec![0; host::MAX_FILE_BYTES + 1]).unwrap();
        assert!(
            read_project(root.path(), std::path::Path::new("large"), false)
                .unwrap_err()
                .contains("1 MiB")
        );
        assert!(read_project(root.path(), std::path::Path::new("."), false).is_err());
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(outside.path(), root.path().join("escape")).unwrap();
            assert!(
                read_project(root.path(), std::path::Path::new("escape"), false)
                    .unwrap_err()
                    .contains("outside")
            );
            std::os::unix::fs::symlink(root.path().join("hello.txt"), root.path().join("internal"))
                .unwrap();
            assert!(read_project(root.path(), std::path::Path::new("internal"), false).is_ok());
        }
    }
    #[test]
    fn directory_pages_cover_every_entry_without_duplicates() {
        let root = tempfile::tempdir().unwrap();
        for index in 0..host::MAX_DIRECTORY_ENTRIES + 3 {
            std::fs::write(root.path().join(format!("{index:05}")), "").unwrap();
        }
        let mut after = None;
        let mut paths = Vec::new();
        loop {
            let Response::Directory { entries } =
                read_project_page(root.path(), root.path(), true, after.as_deref()).unwrap()
            else {
                panic!()
            };
            if entries.is_empty() {
                break;
            }
            assert!(entries.len() <= host::MAX_DIRECTORY_ENTRIES);
            after = entries.last().map(|entry| entry.path.clone());
            paths.extend(entries.into_iter().map(|entry| entry.path));
        }
        assert_eq!(paths.len(), host::MAX_DIRECTORY_ENTRIES + 3);
        assert!(paths.windows(2).all(|pair| pair[0] < pair[1]));
    }
    #[test]
    fn missing_capabilities_and_retired_endpoints_fail_without_ui_dispatch() {
        let bridge = Bridge::new();
        let endpoint = Endpoint {
            bridge: bridge.clone(),
            plugin: "test.host".into(),
            generation: 1,
            capabilities: BTreeSet::new(),
            active: Arc::new(AtomicBool::new(true)),
        };
        assert!(
            endpoint
                .request(Request::Workspace)
                .unwrap_err()
                .contains(host::WORKSPACE_READ)
        );
        assert!(bridge.receiver.is_empty());
        endpoint.active.store(false, Ordering::Release);
        assert!(
            endpoint
                .request(Request::Buffers)
                .unwrap_err()
                .contains("retired")
        );
    }
    #[test]
    fn project_capability_can_query_its_root_without_extra_workspace_capability() {
        let bridge = Bridge::new();
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("a"), "data").unwrap();
        let endpoint = Arc::new(Endpoint {
            bridge: bridge.clone(),
            plugin: "test.host".into(),
            generation: 2,
            capabilities: [host::PROJECT_READ.into()].into(),
            active: Arc::new(AtomicBool::new(true)),
        });
        let worker =
            std::thread::spawn(move || endpoint.request(Request::ReadFile { path: "a".into() }));
        let request = bridge.receiver.recv_blocking().unwrap();
        assert_eq!(request.request, Request::Workspace);
        request.respond(Ok(Response::Workspace {
            workspace: host::Workspace {
                current_directory: "/".into(),
                project_root: Some(root.path().into()),
            },
        }));
        assert_eq!(
            worker.join().unwrap().unwrap(),
            Response::File {
                bytes: b"data".to_vec()
            }
        );
    }
}

#[cfg(test)]
mod retained_endpoint_tests {
    use super::*;
    #[test]
    fn retained_c_context_survives_retirement_and_rejects_calls_until_released() {
        let endpoint = Arc::new(Endpoint {
            bridge: Bridge::new(),
            plugin: "test.host".into(),
            generation: 9,
            capabilities: [host::WORKSPACE_READ.into()].into(),
            active: Arc::new(AtomicBool::new(true)),
        });
        let weak = Arc::downgrade(&endpoint);
        let table = endpoint.table();
        unsafe { (table.retain)(table.context) };
        endpoint.active.store(false, Ordering::Release);
        drop(endpoint);
        assert!(weak.upgrade().is_some());
        let input = serde_json::to_vec(&Request::Workspace).unwrap();
        let mut output = hooks::Reply {
            result: None,
            calls: 0,
        };
        assert_eq!(
            unsafe {
                (table.request)(
                    table.context,
                    Bytes {
                        data: input.as_ptr(),
                        length: input.len(),
                    },
                    hooks::collect,
                    (&mut output as *mut hooks::Reply).cast(),
                )
            },
            OK
        );
        assert!(output.result.unwrap().unwrap_err().contains("retired"));
        assert_eq!(output.calls, 1);
        unsafe { (table.release)(table.context) };
        assert!(weak.upgrade().is_none());
    }
}

#[cfg(test)]
pub(super) fn test_endpoint() -> Arc<Endpoint> {
    Arc::new(Endpoint {
        bridge: Bridge::new(),
        plugin: "test.host".into(),
        generation: 1,
        capabilities: BTreeSet::new(),
        active: Arc::new(AtomicBool::new(true)),
    })
}
