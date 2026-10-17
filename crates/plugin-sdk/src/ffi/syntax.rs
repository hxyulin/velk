//! Worker-owned syntax sessions, independent of the plugin instance.
use super::{guard, registry::Registrar};
use crate::{Plugin, api::*};
use std::ffi::c_void;
use velk_syntax::{Provider, Session};

struct OwnedSession {
    session: Box<dyn Session>,
    length: usize,
}
impl<P: Plugin> Registrar<P> {
    pub fn syntax<S: Provider + Default>(&mut self) -> Result<(), String> {
        // SAFETY: context-free creation owns every session separately from P.
        unsafe {
            self.register_raw(
                velk_syntax::HOOK,
                velk_syntax::CONTRACT,
                1,
                SERVICE,
                SyntaxApi {
                    create: create::<S>,
                    update,
                    highlights,
                    destroy,
                },
                (),
            )
        }
    }
}
unsafe extern "C" fn create<S: Provider + Default>(
    language: Bytes,
    output: *mut *mut c_void,
) -> i32 {
    guard(|| {
        if output.is_null() || language.data.is_null() || language.length != 4 {
            return INVALID;
        }
        // SAFETY: the caller lends initialized language bytes and writable output.
        let language = unsafe { std::slice::from_raw_parts(language.data, language.length) };
        if language != b"rust" {
            return NOT_FOUND;
        }
        let Ok(session) = S::default().open(velk_syntax::Language::Rust) else {
            return INVALID;
        };
        unsafe {
            output.write(Box::into_raw(Box::new(OwnedSession { session, length: 0 })).cast());
        }
        OK
    })
}
unsafe extern "C" fn update(
    session: *mut c_void,
    document: *const SyntaxDocument,
    cancelled: SyntaxCancelled,
    context: *mut c_void,
) -> i32 {
    guard(|| {
        if session.is_null() || document.is_null() {
            return INVALID;
        }
        // SAFETY: this session is uniquely owned by the serialized caller.
        let (session, document) = unsafe { (&mut *session.cast::<OwnedSession>(), &*document) };
        if document.text.data.is_null()
            || document.text.length > MAX_DOCUMENT_BYTES
            || document.edit_count > 4096
            || document.edit_count > 0 && document.edits.is_null()
        {
            return INVALID;
        }
        let text = unsafe { std::slice::from_raw_parts(document.text.data, document.text.length) };
        let Ok(text) = std::str::from_utf8(text) else {
            return INVALID;
        };
        if unsafe { cancelled(context) } {
            return SYNTAX_CANCELLED;
        }
        let text = ropey::Rope::from_str(text);
        let changes = if document.edit_count == 0 {
            &[][..]
        } else {
            unsafe { std::slice::from_raw_parts(document.edits, document.edit_count) }
        };
        let point = |p: SyntaxPoint| text_point(p);
        let changes: Vec<_> = changes
            .iter()
            .map(|e| velk_syntax::Edit {
                revision: e.revision,
                replaced: e.replaced_start..e.replaced_end,
                inserted: e.inserted,
                start: point(e.start),
                old_end: point(e.old_end),
                new_end: point(e.new_end),
            })
            .collect();
        let length = text.len_chars();
        match session
            .session
            .update(document.revision, text, &changes, &|| unsafe {
                cancelled(context)
            }) {
            Ok(true) => {
                session.length = length;
                OK
            }
            Ok(false) => SYNTAX_CANCELLED,
            Err(_) => INVALID,
        }
    })
}
fn text_point(p: SyntaxPoint) -> velk_syntax::TextPoint {
    velk_syntax::TextPoint {
        byte: p.byte,
        line: p.line,
        column: p.column,
    }
}
unsafe extern "C" fn highlights(
    session: *mut c_void,
    start: usize,
    end: usize,
    cancelled: SyntaxCancelled,
    cancellation: *mut c_void,
    sink: SyntaxSink,
    context: *mut c_void,
) -> i32 {
    guard(|| {
        if session.is_null() {
            return INVALID;
        }
        let session = unsafe { &mut *session.cast::<OwnedSession>() };
        if start > end || end > session.length {
            return INVALID;
        }
        let captures = match session
            .session
            .highlights(start..end, &|| unsafe { cancelled(cancellation) })
        {
            Ok(Some(captures)) => captures,
            Ok(None) => return SYNTAX_CANCELLED,
            Err(_) => return INVALID,
        };
        if velk_syntax::validate_highlights(&captures, &(start..end), session.length).is_err() {
            return INVALID;
        }
        let wire: Vec<_> = captures
            .iter()
            .map(|h| SyntaxHighlight {
                start: h.range.start,
                end: h.range.end,
                capture: Bytes {
                    data: h.capture.as_ptr(),
                    length: h.capture.len(),
                },
            })
            .collect();
        // SAFETY: the sink copies these borrowed captures before returning.
        unsafe { sink(context, wire.as_ptr(), wire.len()) }
    })
}
unsafe extern "C" fn destroy(session: *mut c_void) -> i32 {
    guard(|| {
        if session.is_null() {
            return INVALID;
        }
        // SAFETY: exactly one owner releases the pointer produced by create.
        unsafe {
            drop(Box::from_raw(session.cast::<OwnedSession>()));
        }
        OK
    })
}
