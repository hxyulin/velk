use super::*;
use velk_syntax::{Highlight, Language, Provider, Session};

struct Lease {
    api: SyntaxApi,
    active: Arc<std::sync::RwLock<bool>>,
    code: Arc<Code>,
    id: String,
}
impl std::fmt::Debug for Lease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SyntaxProvider")
            .field("plugin", &self.id)
            .finish()
    }
}
struct NativeSession {
    pointer: NonNull<c_void>,
    api: SyntaxApi,
    active: Arc<std::sync::RwLock<bool>>,
    _code: Arc<Code>,
}
// SAFETY: the session is uniquely owned and used serially on one syntax worker.
// Its context is independent of the plugin instance; code stays pinned through drop.
unsafe impl Send for NativeSession {}
impl Provider for Lease {
    fn open(&self, language: Language) -> Result<Box<dyn Session>, String> {
        let active = self.active.read().map_err(|_| "syntax provider poisoned")?;
        if !*active {
            return Err("syntax provider retired".into());
        }
        let language = language.as_str();
        let mut pointer = std::ptr::null_mut();
        // SAFETY: code is pinned; create is reentrant and owns its returned session.
        let status = unsafe {
            (self.api.create)(
                Bytes {
                    data: language.as_ptr(),
                    length: language.len(),
                },
                &mut pointer,
            )
        };
        if status != OK {
            return Err(format!("cannot open syntax session ({status})"));
        }
        Ok(Box::new(NativeSession {
            pointer: NonNull::new(pointer).ok_or("missing syntax session")?,
            api: self.api,
            active: self.active.clone(),
            _code: self.code.clone(),
        }))
    }
}
unsafe extern "C" fn cancelled(context: *mut c_void) -> bool {
    // SAFETY: the session borrows this closure only during the synchronous call.
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        (*context.cast::<&dyn Fn() -> bool>())()
    }))
    .unwrap_or(true)
}
struct Captures {
    value: Option<Vec<Highlight>>,
    calls: usize,
}
unsafe extern "C" fn collect(
    context: *mut c_void,
    captures: *const SyntaxHighlight,
    count: usize,
) -> i32 {
    // SAFETY: trusted native callbacks lend their array and names for this call.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let output = unsafe { &mut *context.cast::<Captures>() };
        output.calls += 1;
        if output.calls != 1
            || count > velk_syntax::MAX_HIGHLIGHTS
            || count > 0 && captures.is_null()
        {
            return INVALID;
        }
        let captures = if count == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(captures, count) }
        };
        let mut value = Vec::with_capacity(count);
        for h in captures {
            if h.capture.data.is_null() || h.capture.length > 128 {
                return INVALID;
            }
            let Ok(capture) = std::str::from_utf8(unsafe {
                std::slice::from_raw_parts(h.capture.data, h.capture.length)
            }) else {
                return INVALID;
            };
            value.push(Highlight {
                range: h.start..h.end,
                capture: capture.into(),
            });
        }
        output.value = Some(value);
        OK
    }));
    result.unwrap_or(PANICKED)
}
impl Session for NativeSession {
    fn update(
        &mut self,
        revision: u64,
        text: ropey::Rope,
        edits: &[velk_syntax::Edit],
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<bool, String> {
        let active = self.active.read().map_err(|_| "syntax provider poisoned")?;
        if !*active {
            return Err("syntax provider retired".into());
        }
        if text.len_bytes() > MAX_DOCUMENT_BYTES || edits.len() > 4096 {
            return Err("syntax document exceeds limits".into());
        }
        if is_cancelled() {
            return Ok(false);
        }
        let text = text.to_string();
        let point = |p: velk_syntax::TextPoint| SyntaxPoint {
            byte: p.byte,
            line: p.line,
            column: p.column,
        };
        let edits: Vec<_> = edits
            .iter()
            .map(|e| SyntaxEdit {
                revision: e.revision,
                replaced_start: e.replaced.start,
                replaced_end: e.replaced.end,
                inserted: e.inserted,
                start: point(e.start),
                old_end: point(e.old_end),
                new_end: point(e.new_end),
            })
            .collect();
        let document = SyntaxDocument {
            revision,
            text: Bytes {
                data: text.as_ptr(),
                length: text.len(),
            },
            edits: edits.as_ptr(),
            edit_count: edits.len(),
        };
        let mut cancellation = is_cancelled;
        // SAFETY: all views and the cancellation closure last this serialized call.
        let status = unsafe {
            (self.api.update)(
                self.pointer.as_ptr(),
                &document,
                cancelled,
                (&mut cancellation as *mut &dyn Fn() -> bool).cast(),
            )
        };
        match status {
            OK => Ok(true),
            SYNTAX_CANCELLED => Ok(false),
            _ => Err(format!("syntax update failed ({status})")),
        }
    }
    fn highlights(
        &mut self,
        range: std::ops::Range<usize>,
        is_cancelled: &dyn Fn() -> bool,
    ) -> Result<Option<Vec<Highlight>>, String> {
        let active = self.active.read().map_err(|_| "syntax provider poisoned")?;
        if !*active {
            return Err("syntax provider retired".into());
        }
        let mut cancellation = is_cancelled;
        let mut output = Captures {
            value: None,
            calls: 0,
        };
        // SAFETY: the unique session and both borrowed callback contexts outlive this call.
        let status = unsafe {
            (self.api.highlights)(
                self.pointer.as_ptr(),
                range.start,
                range.end,
                cancelled,
                (&mut cancellation as *mut &dyn Fn() -> bool).cast(),
                collect,
                (&mut output as *mut Captures).cast(),
            )
        };
        if status == SYNTAX_CANCELLED {
            return Ok(None);
        }
        if status != OK || output.calls != 1 {
            return Err(format!("syntax query failed ({status})"));
        }
        Ok(Some(output.value.ok_or("missing syntax captures")?))
    }
}
impl Drop for NativeSession {
    fn drop(&mut self) {
        // SAFETY: session destruction never touches the retired plugin instance;
        // this object pins the code and uniquely owns the allocation.
        let status = unsafe { (self.api.destroy)(self.pointer.as_ptr()) };
        if status != OK {
            tracing::error!(
                status,
                "Syntax session teardown failed; retaining library until process exit"
            );
            // Failed teardown does not prove that plugin-owned activity stopped.
            std::mem::forget(self._code.clone());
        }
    }
}
impl Manager {
    pub fn syntax_provider(&self, id: &str) -> Result<Arc<dyn Provider>, String> {
        let generation = self
            .plugins
            .get(id)
            .filter(|p| !p.destroyed)
            .ok_or("syntax plugin unavailable")?;
        Ok(Arc::new(Lease {
            api: generation.syntax.ok_or("syntax service unavailable")?,
            active: generation.services_active.clone(),
            code: generation
                .library
                .clone()
                .ok_or("syntax code unavailable")?,
            id: id.into(),
        }))
    }
    pub(crate) fn syntax_providers(&self) -> BTreeMap<String, Arc<dyn Provider>> {
        self.plugins
            .keys()
            .filter_map(|id| self.syntax_provider(id).ok().map(|p| (id.clone(), p)))
            .collect()
    }
}
