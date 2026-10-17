use super::*;
use velk_core_contracts::history::{
    Provider, Request, Response, Restore, Session, Snapshot, Status,
};
struct Factory {
    api: UndoApi,
    code: Arc<Code>,
    id: String,
}
impl std::fmt::Debug for Factory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("UndoProvider").field(&self.id).finish()
    }
}
struct NativeSession {
    pointer: NonNull<c_void>,
    api: UndoApi,
    code: Arc<Code>,
    status: Status,
}
// SAFETY: each session is independently allocated and accessed serially by Core.
unsafe impl Send for NativeSession {}
struct NativeSnapshot {
    pointer: NonNull<c_void>,
    api: UndoApi,
    _code: Arc<Code>,
}
// SAFETY: the contract requires immutable Send + Sync snapshots; code stays pinned.
unsafe impl Send for NativeSnapshot {}
unsafe impl Sync for NativeSnapshot {}
struct Reply {
    bytes: Option<Result<Vec<u8>, String>>,
    calls: usize,
}
unsafe extern "C" fn collect(context: *mut c_void, bytes: Bytes, status: i32) -> i32 {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        // SAFETY: caller-owned context and a borrowed initialized byte view.
        let reply = unsafe { &mut *context.cast::<Reply>() };
        reply.calls += 1;
        if reply.calls != 1
            || bytes.length > MAX_UNDO_BYTES
            || bytes.length > 0 && bytes.data.is_null()
        {
            return INVALID;
        }
        let value = if bytes.length == 0 {
            vec![]
        } else {
            unsafe { std::slice::from_raw_parts(bytes.data, bytes.length) }.to_vec()
        };
        reply.bytes = Some(if status == OK {
            Ok(value)
        } else {
            Err(String::from_utf8_lossy(&value).into_owned())
        });
        OK
    }))
    .unwrap_or(PANICKED)
}
fn result(status: i32, reply: Reply) -> Result<Vec<u8>, String> {
    if status != OK || reply.calls != 1 {
        return Err(format!("undo provider callback failed ({status})"));
    }
    reply.bytes.ok_or("missing undo reply")?
}
fn view(bytes: &[u8]) -> Bytes {
    Bytes {
        data: bytes.as_ptr(),
        length: bytes.len(),
    }
}
struct Failed(String);
impl Session for Failed {
    fn status(&self) -> Status {
        Status::default()
    }
    fn request(&mut self, _: Request) -> Result<Response, String> {
        Err(self.0.clone())
    }
    fn snapshot(&self) -> Result<Arc<dyn Snapshot>, String> {
        Err(self.0.clone())
    }
}
impl Factory {
    fn create(&self, root: u64, archive: &[u8]) -> Result<Box<dyn Session>, String> {
        let mut pointer = std::ptr::null_mut();
        // SAFETY: factory code is pinned; outputs are caller-owned and creation is reentrant.
        let status = unsafe { (self.api.create)(root, view(archive), &mut pointer) };
        if status != OK {
            return Err(format!("undo initialization failed ({status})"));
        }
        let mut session = NativeSession {
            pointer: NonNull::new(pointer).ok_or("missing undo session")?,
            api: self.api,
            code: self.code.clone(),
            status: Status::default(),
        };
        session.request(Request::Status)?;
        Ok(Box::new(session))
    }
}
impl Provider for Factory {
    fn id(&self) -> &str {
        &self.id
    }
    fn open(&self, root: u64) -> Box<dyn Session> {
        self.create(root, &[])
            .unwrap_or_else(|error| Box::new(Failed(error)))
    }
    fn restore(&self, archive: Restore) -> Result<Box<dyn Session>, String> {
        self.create(
            archive.state,
            &serde_json::to_vec(&archive).map_err(|e| e.to_string())?,
        )
    }
}
impl Session for NativeSession {
    fn status(&self) -> Status {
        self.status.clone()
    }
    fn request(&mut self, request: Request) -> Result<Response, String> {
        let bytes = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        if bytes.len() > MAX_UNDO_BYTES {
            return Err("undo input exceeds 512 MiB".into());
        }
        let mut reply = Reply {
            bytes: None,
            calls: 0,
        };
        // SAFETY: access is serialized, code is pinned, and inputs/sink last this call.
        let status = unsafe {
            (self.api.request)(
                self.pointer.as_ptr(),
                view(&bytes),
                collect,
                (&mut reply as *mut Reply).cast(),
            )
        };
        let response: Response =
            serde_json::from_slice(&result(status, reply)?).map_err(|e| e.to_string())?;
        self.status = response.status.clone();
        Ok(response)
    }
    fn snapshot(&self) -> Result<Arc<dyn Snapshot>, String> {
        let mut pointer = std::ptr::null_mut();
        // SAFETY: snapshots own immutable data independent of session/instance lifetime.
        let status = unsafe { (self.api.snapshot)(self.pointer.as_ptr(), &mut pointer) };
        if status != OK {
            return Err(format!("undo snapshot failed ({status})"));
        }
        Ok(Arc::new(NativeSnapshot {
            pointer: NonNull::new(pointer).ok_or("missing undo snapshot")?,
            api: self.api,
            _code: self.code.clone(),
        }))
    }
}
impl Snapshot for NativeSnapshot {
    fn encode(&self) -> Result<serde_json::Value, String> {
        let mut reply = Reply {
            bytes: None,
            calls: 0,
        };
        // SAFETY: snapshot and code remain pinned for encoding, including after unload.
        let status = unsafe {
            (self.api.encode)(
                self.pointer.as_ptr(),
                collect,
                (&mut reply as *mut Reply).cast(),
            )
        };
        serde_json::from_slice(&result(status, reply)?).map_err(|e| e.to_string())
    }
}
impl Drop for NativeSession {
    fn drop(&mut self) {
        unsafe {
            (self.api.destroy)(self.pointer.as_ptr());
        }
    }
}
impl Drop for NativeSnapshot {
    fn drop(&mut self) {
        unsafe {
            (self.api.destroy_snapshot)(self.pointer.as_ptr());
        }
    }
}
impl Manager {
    pub fn undo_provider(&self, id: &str) -> Result<Arc<dyn Provider>, String> {
        let generation = self
            .plugins
            .get(id)
            .filter(|p| !p.destroyed)
            .ok_or("undo plugin unavailable")?;
        Ok(Arc::new(Factory {
            api: generation.undo.ok_or("undo service unavailable")?,
            code: generation.library.clone().ok_or("undo code unavailable")?,
            id: id.into(),
        }))
    }
    pub(crate) fn undo_providers(&self) -> BTreeMap<String, Arc<dyn Provider>> {
        self.plugins
            .keys()
            .filter_map(|id| {
                self.undo_provider(id)
                    .ok()
                    .map(|provider| (id.clone(), provider))
            })
            .collect()
    }
}
