//! Shared internal marshaling and safe-author session adapter. No Rust value
//! allocated by one library is destroyed by another.
use super::guard;
use crate::{Plugin, api::*, semantic as model};
use std::{ffi::c_void, sync::Arc};

fn bytes(s: &str) -> Bytes {
    Bytes {
        data: s.as_ptr(),
        length: s.len(),
    }
}
fn request(r: model::HoverRequest) -> SemanticRequest {
    SemanticRequest {
        id: r.id,
        revision: r.revision,
        position: r.position,
    }
}
fn hover_request(r: SemanticRequest) -> model::HoverRequest {
    model::HoverRequest {
        id: r.id,
        revision: r.revision,
        position: r.position,
    }
}
/// # Safety
/// Nonempty views must reference readable storage for the returned borrow.
unsafe fn text<'a>(b: &Bytes, limit: usize) -> Result<&'a str, String> {
    if b.length > limit || (b.length != 0 && b.data.is_null()) {
        return Err("invalid semantic text buffer".into());
    }
    if b.length == 0 {
        return Ok("");
    }
    // SAFETY: the ABI caller guarantees the view's lifetime and storage.
    std::str::from_utf8(unsafe { std::slice::from_raw_parts(b.data, b.length) })
        .map_err(|e| e.to_string())
}
unsafe fn path(b: &Bytes) -> Result<std::path::PathBuf, String> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        if b.length > MAX_INPUT_BYTES || (b.length != 0 && b.data.is_null()) {
            return Err("invalid semantic path".into());
        }
        let bytes = if b.length == 0 {
            Vec::new()
        } else {
            unsafe { std::slice::from_raw_parts(b.data, b.length) }.to_vec()
        };
        Ok(std::ffi::OsString::from_vec(bytes).into())
    }
    #[cfg(not(unix))]
    {
        Ok(unsafe { text(b, MAX_INPUT_BYTES)? }.into())
    }
}
/// Borrow a neutral input as typed C records only for `call`.
pub fn with_input<R>(
    input: &model::Input,
    call: impl FnOnce(&SemanticInput) -> R,
) -> Result<R, String> {
    let mut raw = SemanticInput {
        kind: CANCEL_HOVER,
        document: std::ptr::null(),
        request: Default::default(),
    };
    match input {
        model::Input::Document(d) | model::Input::Saved(d) => {
            #[cfg(unix)]
            let path = {
                use std::os::unix::ffi::OsStrExt;
                d.path.as_os_str().as_bytes()
            };
            #[cfg(not(unix))]
            let path = d
                .path
                .to_str()
                .ok_or("semantic plugin path must be UTF-8 on this platform")?
                .as_bytes();
            if d.text.len_bytes() > MAX_DOCUMENT_BYTES
                || path.len() > MAX_INPUT_BYTES
                || d.language.len() > MAX_INPUT_BYTES
            {
                return Err("semantic document exceeds ABI limits".into());
            }
            let text = d.text.to_string();
            let document = SemanticDocument {
                path: Bytes {
                    data: path.as_ptr(),
                    length: path.len(),
                },
                language: bytes(&d.language),
                text: bytes(&text),
                revision: d.revision,
            };
            raw.kind = if matches!(input, model::Input::Saved(_)) {
                SAVED
            } else {
                DOCUMENT
            };
            raw.document = &document;
            Ok(call(&raw))
        }
        model::Input::Hover(r) => {
            raw.kind = HOVER;
            raw.request = request(*r);
            Ok(call(&raw))
        }
        model::Input::CancelHover => Ok(call(&raw)),
    }
}
unsafe fn decode_input(raw: &SemanticInput) -> Result<model::Input, String> {
    Ok(match raw.kind {
        DOCUMENT | SAVED => {
            // SAFETY: input document is live for this call and copied before return.
            let d = unsafe { raw.document.as_ref() }.ok_or("null semantic document")?;
            let document = model::DocumentSnapshot {
                path: unsafe { path(&d.path)? },
                language: unsafe { text(&d.language, MAX_INPUT_BYTES)? }.into(),
                text: ropey::Rope::from_str(unsafe { text(&d.text, MAX_DOCUMENT_BYTES)? }),
                revision: d.revision,
            };
            if raw.kind == DOCUMENT {
                model::Input::Document(document)
            } else {
                model::Input::Saved(document)
            }
        }
        HOVER => model::Input::Hover(hover_request(raw.request)),
        CANCEL_HOVER => model::Input::CancelHover,
        _ => return Err("invalid semantic input kind".into()),
    })
}
fn severity(s: model::Severity) -> u32 {
    match s {
        model::Severity::Error => 1,
        model::Severity::Warning => 2,
        model::Severity::Information => 3,
        model::Severity::Hint => 4,
    }
}
/// Borrow a neutral event as typed C records only for `call`.
pub fn with_event<R>(
    event: &model::Event,
    call: impl FnOnce(&SemanticEvent) -> R,
) -> Result<R, String> {
    let mut raw = SemanticEvent {
        kind: STATUS,
        status: 0,
        capabilities: 0,
        revision: 0,
        request: Default::default(),
        text: bytes(""),
        start: 0,
        end: 0,
        has_range: 0,
        has_hover: 0,
        markdown: 0,
        diagnostics: std::ptr::null(),
        diagnostic_count: 0,
    };
    match event {
        model::Event::Status(s) => match s {
            model::Status::Starting => raw.status = STARTING,
            model::Status::Ready(c) => {
                raw.status = READY;
                raw.capabilities = u32::from(c.diagnostics) | (u32::from(c.hover) << 1);
            }
            model::Status::Failed(message) => {
                raw.status = FAILED;
                raw.text = bytes(message);
            }
            model::Status::Stopped => raw.status = STOPPED,
        },
        model::Event::Diagnostics {
            revision,
            diagnostics,
        } => {
            if diagnostics.len() > MAX_RUNS {
                return Err("too many semantic diagnostics".into());
            }
            let total = diagnostics.iter().try_fold(0usize, |n, d| {
                n.checked_add(d.message.len())?
                    .checked_add(d.source.as_ref().map_or(0, String::len))?
                    .checked_add(d.code.as_ref().map_or(0, String::len))
            });
            if total.is_none_or(|n| n > MAX_OUTPUT_BYTES) {
                return Err("semantic diagnostics exceed ABI limits".into());
            }
            let views: Vec<_> = diagnostics
                .iter()
                .map(|d| SemanticDiagnostic {
                    start: d.range.start,
                    end: d.range.end,
                    severity: severity(d.severity),
                    message: bytes(&d.message),
                    source: bytes(d.source.as_deref().unwrap_or("")),
                    code: bytes(d.code.as_deref().unwrap_or("")),
                })
                .collect();
            raw.kind = DIAGNOSTICS;
            raw.revision = *revision;
            raw.diagnostics = views.as_ptr();
            raw.diagnostic_count = views.len();
            return Ok(call(&raw));
        }
        model::Event::Hover { request: r, hover } => {
            raw.kind = HOVER_RESULT;
            raw.request = request(*r);
            if let Some(hover) = hover {
                raw.has_hover = 1;
                raw.text = bytes(&hover.contents);
                raw.markdown = u32::from(hover.markdown);
                if let Some(range) = &hover.range {
                    raw.has_range = 1;
                    raw.start = range.start;
                    raw.end = range.end;
                }
            }
        }
        model::Event::RequestFailed {
            request: r,
            message,
        } => {
            raw.kind = REQUEST_FAILED;
            raw.request = request(*r);
            raw.text = bytes(message);
        }
    }
    if raw.text.length > MAX_OUTPUT_BYTES {
        return Err("semantic event exceeds ABI limits".into());
    }
    Ok(call(&raw))
}
/// Copy every plugin-owned byte before returning.
/// # Safety
/// `raw` and all its nonempty views must be valid for this call.
pub unsafe fn decode_event(raw: &SemanticEvent) -> Result<model::Event, String> {
    Ok(match raw.kind {
        STATUS => model::Event::Status(match raw.status {
            STARTING => model::Status::Starting,
            READY if raw.capabilities & !3 == 0 => model::Status::Ready(model::Capabilities {
                diagnostics: raw.capabilities & 1 != 0,
                hover: raw.capabilities & 2 != 0,
            }),
            FAILED => model::Status::Failed(unsafe { text(&raw.text, MAX_OUTPUT_BYTES)? }.into()),
            STOPPED => model::Status::Stopped,
            _ => return Err("invalid semantic status".into()),
        }),
        DIAGNOSTICS => {
            if raw.diagnostic_count > MAX_RUNS
                || (raw.diagnostic_count != 0 && raw.diagnostics.is_null())
            {
                return Err("invalid diagnostic array".into());
            }
            let views = if raw.diagnostic_count == 0 {
                &[]
            } else {
                unsafe { std::slice::from_raw_parts(raw.diagnostics, raw.diagnostic_count) }
            };
            let mut diagnostics = Vec::with_capacity(views.len());
            let mut size = 0usize;
            for d in views {
                size = size
                    .checked_add(d.message.length)
                    .and_then(|n| n.checked_add(d.source.length))
                    .and_then(|n| n.checked_add(d.code.length))
                    .filter(|n| *n <= MAX_OUTPUT_BYTES)
                    .ok_or("diagnostic output too large")?;
                if d.start > d.end {
                    return Err("invalid diagnostic range".into());
                }
                let source = unsafe { text(&d.source, MAX_OUTPUT_BYTES)? };
                let code = unsafe { text(&d.code, MAX_OUTPUT_BYTES)? };
                diagnostics.push(model::Diagnostic {
                    range: d.start..d.end,
                    severity: match d.severity {
                        1 => model::Severity::Error,
                        2 => model::Severity::Warning,
                        3 => model::Severity::Information,
                        4 => model::Severity::Hint,
                        _ => return Err("invalid diagnostic severity".into()),
                    },
                    message: unsafe { text(&d.message, MAX_OUTPUT_BYTES)? }.into(),
                    source: (!source.is_empty()).then(|| source.into()),
                    code: (!code.is_empty()).then(|| code.into()),
                });
            }
            model::Event::Diagnostics {
                revision: raw.revision,
                diagnostics,
            }
        }
        HOVER_RESULT => {
            if raw.has_hover > 1
                || raw.has_range > 1
                || raw.markdown > 1
                || (raw.has_range == 1 && raw.start > raw.end)
            {
                return Err("invalid semantic hover".into());
            }
            let request = hover_request(raw.request);
            model::Event::Hover {
                request,
                hover: if raw.has_hover == 1 {
                    Some(model::Hover {
                        request,
                        range: (raw.has_range == 1).then_some(raw.start..raw.end),
                        contents: unsafe { text(&raw.text, MAX_OUTPUT_BYTES)? }.into(),
                        markdown: raw.markdown == 1,
                    })
                } else {
                    None
                },
            }
        }
        REQUEST_FAILED => model::Event::RequestFailed {
            request: hover_request(raw.request),
            message: unsafe { text(&raw.text, MAX_OUTPUT_BYTES)? }.into(),
        },
        _ => return Err("invalid semantic event kind".into()),
    })
}
struct HostSink {
    callback: SemanticSink,
    context: *mut c_void,
}
// SAFETY: the ABI host explicitly supplies a thread-safe sink/context, retained
// until session destroy joins this producer. No Rust borrow crosses the ABI.
unsafe impl Send for HostSink {}
unsafe impl Sync for HostSink {}
impl HostSink {
    fn publish(&self, event: model::Event) {
        let result = with_event(&event, |raw| unsafe { (self.callback)(self.context, raw) });
        if result.is_err() {
            let error = model::Event::Status(model::Status::Failed(
                "semantic event exceeds ABI limits".into(),
            ));
            let _ = with_event(&error, |raw| unsafe { (self.callback)(self.context, raw) });
        }
    }
}
struct OwnedSession {
    service: Arc<model::Service>,
}
pub(super) unsafe extern "C" fn create<P: Plugin, F>(
    instance: *mut c_void,
    definition: *const LanguageServerDefinition,
    sink: SemanticSink,
    context: *mut c_void,
    output: *mut *mut c_void,
) -> i32
where
    F: FnMut(&mut P, crate::LanguageServer) -> Box<dyn model::Backend> + 'static,
{
    if instance.is_null() || definition.is_null() || output.is_null() {
        return INVALID;
    }
    guard(|| {
        // SAFETY: serialized lifecycle call on a live plugin instance.
        let Ok(definition) = (unsafe { super::language_server::decode(&*definition) }) else {
            return INVALID;
        };
        let binding = unsafe { &mut *instance.cast::<super::registry::Binding<P, F>>() };
        let backend = (binding.handler)(unsafe { &mut *binding.plugin }, definition);
        let host = HostSink {
            callback: sink,
            context,
        };
        let Ok(service) = model::Service::start_observed(backend, move |event| host.publish(event))
        else {
            return INVALID;
        };
        let session = Box::new(OwnedSession { service });
        unsafe {
            output.write(Box::into_raw(session).cast());
        }
        OK
    })
}
pub(super) unsafe extern "C" fn input(instance: *mut c_void, raw: *const SemanticInput) -> i32 {
    if instance.is_null() || raw.is_null() {
        return INVALID;
    }
    guard(|| {
        let Ok(input) = (unsafe { decode_input(&*raw) }) else {
            return INVALID;
        };
        let service = unsafe { &(*instance.cast::<OwnedSession>()).service };
        match input {
            model::Input::Document(d) => service.update(d),
            model::Input::Saved(d) => service.saved(d),
            model::Input::Hover(r) => service.hover(r),
            model::Input::CancelHover => service.cancel_hover(),
        };
        OK
    })
}
pub(super) unsafe extern "C" fn destroy(instance: *mut c_void) -> i32 {
    if instance.is_null() {
        return INVALID;
    }
    guard(|| {
        // Keep ownership intact on failure: the host retains library + sink.
        let session = unsafe { &*instance.cast::<OwnedSession>() };
        if session.service.shutdown_and_join().is_err() {
            return PANICKED;
        }
        unsafe {
            drop(Box::from_raw(instance.cast::<OwnedSession>()));
        }
        OK
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_inputs_and_events_copy_unicode_ranges_and_owned_strings() {
        let d = model::DocumentSnapshot {
            path: "/tmp/file 世界.rs".into(),
            language: "rust".into(),
            revision: 73,
            text: ropey::Rope::from_str("fn café() { /* 👩‍💻 */ }"),
        };
        let r = model::HoverRequest {
            id: 12,
            revision: 73,
            position: 4,
        };
        for input in [
            model::Input::Document(d.clone()),
            model::Input::Saved(d),
            model::Input::Hover(r),
            model::Input::CancelHover,
        ] {
            let decoded = with_input(&input, |raw| unsafe { decode_input(raw) })
                .unwrap()
                .unwrap();
            match (&input, decoded) {
                (model::Input::Document(a), model::Input::Document(b))
                | (model::Input::Saved(a), model::Input::Saved(b)) => {
                    assert_eq!(a.path, b.path);
                    assert_eq!(a.text, b.text);
                    assert_eq!(a.revision, b.revision);
                }
                (model::Input::Hover(a), model::Input::Hover(b)) => assert_eq!(*a, b),
                (model::Input::CancelHover, model::Input::CancelHover) => {}
                _ => panic!("input kind changed"),
            }
        }
        let diagnostic = model::Diagnostic {
            range: 3..7,
            severity: model::Severity::Warning,
            message: "message 世界".into(),
            source: Some("server".into()),
            code: Some("E1".into()),
        };
        let events = [
            model::Event::Status(model::Status::Starting),
            model::Event::Status(model::Status::Ready(model::Capabilities {
                diagnostics: true,
                hover: true,
            })),
            model::Event::Status(model::Status::Failed("failed 世界".into())),
            model::Event::Status(model::Status::Stopped),
            model::Event::Diagnostics {
                revision: 73,
                diagnostics: vec![diagnostic.clone()],
            },
            model::Event::Hover {
                request: r,
                hover: Some(model::Hover {
                    request: r,
                    range: Some(3..7),
                    contents: "**世界**".into(),
                    markdown: true,
                }),
            },
            model::Event::Hover {
                request: r,
                hover: None,
            },
            model::Event::RequestFailed {
                request: r,
                message: "cancelled".into(),
            },
        ];
        let copied: Vec<_> = events
            .iter()
            .map(|event| {
                with_event(event, |raw| unsafe { decode_event(raw) })
                    .unwrap()
                    .unwrap()
            })
            .collect();
        for (a, b) in events.iter().zip(&copied) {
            assert_eq!(format!("{a:?}"), format!("{b:?}"));
        }
        drop(events);
        assert!(
            matches!(&copied[4],model::Event::Diagnostics{diagnostics,..} if diagnostics==&[diagnostic])
        );
    }
    #[test]
    fn rejects_bad_semantic_records_before_dereferencing_payloads() {
        with_event(&model::Event::Status(model::Status::Starting), |event| {
            let mut raw = SemanticEvent {
                kind: DIAGNOSTICS,
                diagnostics: std::ptr::null(),
                diagnostic_count: MAX_RUNS + 1,
                ..*event
            };
            assert!(unsafe { decode_event(&raw) }.is_err());
            raw.diagnostic_count = 1;
            assert!(unsafe { decode_event(&raw) }.is_err());
            raw.kind = STATUS;
            raw.status = READY;
            raw.capabilities = 8;
            assert!(unsafe { decode_event(&raw) }.is_err());
            raw.kind = HOVER_RESULT;
            raw.has_hover = 1;
            raw.has_range = 1;
            raw.start = 10;
            raw.end = 1;
            assert!(unsafe { decode_event(&raw) }.is_err());
        })
        .unwrap();
        let input = model::Input::Document(model::DocumentSnapshot {
            path: "/file".into(),
            language: "rust".into(),
            revision: 0,
            text: ropey::Rope::new(),
        });
        with_input(&input, |raw| {
            let raw = SemanticInput {
                kind: DOCUMENT,
                document: std::ptr::null(),
                ..*raw
            };
            assert!(unsafe { decode_input(&raw) }.is_err());
        })
        .unwrap();
    }
    #[cfg(unix)]
    #[test]
    fn semantic_paths_preserve_non_utf8_unix_names() {
        use std::os::unix::ffi::OsStringExt;
        let path: std::path::PathBuf =
            std::ffi::OsString::from_vec(b"/tmp/file-\xff.rs".to_vec()).into();
        let input = model::Input::Document(model::DocumentSnapshot {
            path: path.clone(),
            language: "rust".into(),
            revision: 0,
            text: ropey::Rope::new(),
        });
        let model::Input::Document(decoded) =
            with_input(&input, |raw| unsafe { decode_input(raw) })
                .unwrap()
                .unwrap()
        else {
            panic!("wrong input kind")
        };
        assert_eq!(decoded.path, path);
    }
}
