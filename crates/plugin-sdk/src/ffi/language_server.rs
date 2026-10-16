//! Author-side language-definition callback over the native ABI.
use super::guard;
use crate::api::wire::language_server::with_error;
pub use crate::api::wire::language_server::{decode, with_definition};
use crate::{LanguageServer, Plugin, api::*};
use std::ffi::c_void;

pub(super) unsafe extern "C" fn query<P: Plugin, F>(
    instance: *mut c_void,
    language: *const u8,
    length: usize,
    sink: LanguageServerSink,
    context: *mut c_void,
) -> i32
where
    F: FnMut(&mut P, &str) -> Result<Option<LanguageServer>, String> + 'static,
{
    if instance.is_null() || length > 64 || (length != 0 && language.is_null()) {
        return INVALID;
    }
    guard(|| {
        let raw = Bytes {
            data: language,
            length,
        };
        let input = if raw.length == 0 {
            &[][..]
        } else {
            // SAFETY: the checked request buffer is borrowed for this call.
            unsafe { std::slice::from_raw_parts(raw.data, raw.length) }
        };
        let Ok(language) = std::str::from_utf8(input) else {
            return INVALID;
        };
        // SAFETY: serialized call on a live instance; only explicit sink borrows.
        let binding = unsafe { &mut *instance.cast::<super::registry::Binding<P, F>>() };
        match (binding.handler)(unsafe { &mut *binding.plugin }, language) {
            Ok(None) => NOT_FOUND,
            Ok(Some(d)) => match with_definition(&d, |raw| unsafe { sink(context, raw) }) {
                Ok(status) => status,
                Err(error) => with_error(&error, |raw| unsafe { sink(context, raw) }),
            },
            Err(error) => with_error(&error, |raw| unsafe { sink(context, raw) }),
        }
    })
}
