//! Synchronous typed language-server definitions. All string/array views are
//! borrowed for one call and copied into the receiving library's allocator.
use super::guard;
use crate::{LanguageServer, Plugin, api::*};
use std::ffi::c_void;
fn bytes(s: &str) -> Bytes {
    Bytes {
        data: s.as_ptr(),
        length: s.len(),
    }
}
unsafe fn text(raw: &Bytes) -> Result<String, String> {
    if raw.length > MAX_INPUT_BYTES || (raw.length != 0 && raw.data.is_null()) {
        return Err("invalid language-server string".into());
    }
    if raw.length == 0 {
        return Ok(String::new());
    }
    Ok(
        std::str::from_utf8(unsafe { std::slice::from_raw_parts(raw.data, raw.length) })
            .map_err(|e| e.to_string())?
            .into(),
    )
}
fn validate(d: &LanguageServer) -> Result<(), String> {
    if parse_plugin_id(&d.transport).is_none()
        || d.language.is_empty()
        || d.language.len() > 64
        || d.executable.is_empty()
        || d.arguments.len() > 256
    {
        return Err("invalid language-server definition".into());
    }
    let mut size = 0usize;
    for s in [
        &d.transport,
        &d.language,
        &d.executable,
        d.root.as_deref().unwrap_or(""),
        &d.initialization_options,
        &d.configuration,
    ]
    .into_iter()
    .chain(d.arguments.iter().map(String::as_str))
    {
        if s.contains('\0') {
            return Err("language-server definition contains NUL".into());
        }
        size = size
            .checked_add(s.len())
            .filter(|n| *n <= MAX_INPUT_BYTES)
            .ok_or("language-server definition exceeds 1 MiB")?;
    }
    Ok(())
}
/// # Safety
/// All views must remain valid for this call. Output is fully owned.
pub unsafe fn decode(raw: &LanguageServerDefinition) -> Result<LanguageServer, String> {
    let error = unsafe { text(&raw.error)? };
    if !error.is_empty() {
        return Err(error);
    }
    if raw.argument_count > 256 || (raw.argument_count != 0 && raw.arguments.is_null()) {
        return Err("invalid language-server arguments".into());
    }
    let args = if raw.argument_count == 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(raw.arguments, raw.argument_count) }
    };
    let views = [
        &raw.transport,
        &raw.language,
        &raw.executable,
        &raw.root,
        &raw.initialization_options,
        &raw.configuration,
    ];
    let size = views
        .into_iter()
        .chain(args.iter())
        .try_fold(0usize, |n, s| n.checked_add(s.length));
    if size.is_none_or(|n| n > MAX_INPUT_BYTES) {
        return Err("language-server definition exceeds 1 MiB".into());
    }
    let root = unsafe { text(&raw.root)? };
    let definition = LanguageServer {
        transport: unsafe { text(&raw.transport)? },
        language: unsafe { text(&raw.language)? },
        executable: unsafe { text(&raw.executable)? },
        arguments: args
            .iter()
            .map(|s| unsafe { text(s) })
            .collect::<Result<_, _>>()?,
        root: (!root.is_empty()).then_some(root),
        initialization_options: unsafe { text(&raw.initialization_options)? },
        configuration: unsafe { text(&raw.configuration)? },
    };
    validate(&definition)?;
    Ok(definition)
}
/// Only the callback borrows the typed record; the caller retains all ownership.
pub fn with_definition<R>(
    d: &LanguageServer,
    call: impl FnOnce(&LanguageServerDefinition) -> R,
) -> Result<R, String> {
    validate(d)?;
    let args: Vec<_> = d.arguments.iter().map(|s| bytes(s)).collect();
    Ok(call(&LanguageServerDefinition {
        transport: bytes(&d.transport),
        language: bytes(&d.language),
        executable: bytes(&d.executable),
        arguments: args.as_ptr(),
        argument_count: args.len(),
        root: bytes(d.root.as_deref().unwrap_or("")),
        initialization_options: bytes(&d.initialization_options),
        configuration: bytes(&d.configuration),
        error: bytes(""),
    }))
}
fn with_error<R>(error: &str, call: impl FnOnce(&LanguageServerDefinition) -> R) -> R {
    call(&LanguageServerDefinition {
        transport: bytes(""),
        language: bytes(""),
        executable: bytes(""),
        arguments: std::ptr::null(),
        argument_count: 0,
        root: bytes(""),
        initialization_options: bytes(""),
        configuration: bytes(""),
        error: bytes(error),
    })
}
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
        let Ok(language) = (unsafe { text(&raw) }) else {
            return INVALID;
        };
        // SAFETY: serialized call on a live instance; only explicit sink borrows.
        let binding = unsafe { &mut *instance.cast::<super::registry::Binding<P, F>>() };
        match (binding.handler)(unsafe { &mut *binding.plugin }, &language) {
            Ok(None) => NOT_FOUND,
            Ok(Some(d)) => match with_definition(&d, |raw| unsafe { sink(context, raw) }) {
                Ok(status) => status,
                Err(error) => with_error(&error, |raw| unsafe { sink(context, raw) }),
            },
            Err(error) => with_error(&error, |raw| unsafe { sink(context, raw) }),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> LanguageServer {
        LanguageServer {
            transport: "velk.lsp".into(),
            language: "rust".into(),
            executable: "/tools 世界/rust-analyzer".into(),
            arguments: vec!["--log-file".into(), "/tmp/a b.log".into()],
            root: Some("/tmp/project 世界".into()),
            initialization_options: "{\"checkOnSave\":false}".into(),
            configuration: "{}".into(),
        }
    }
    #[test]
    fn definition_round_trip_owns_strings_and_checks_limits() {
        let source = sample();
        let copied = with_definition(&source, |raw| unsafe { decode(raw) })
            .unwrap()
            .unwrap();
        assert_eq!(source, copied);
        drop(source);
        assert_eq!(copied.arguments[1], "/tmp/a b.log");
        with_definition(&copied, |raw| {
            let mut invalid = LanguageServerDefinition {
                argument_count: 257,
                ..*raw
            };
            assert!(unsafe { decode(&invalid) }.is_err());
            invalid.argument_count = 1;
            invalid.arguments = std::ptr::null();
            assert!(unsafe { decode(&invalid) }.is_err());
        })
        .unwrap();
        let mut invalid = sample();
        invalid.transport = "unscoped".into();
        assert!(with_definition(&invalid, |_| ()).is_err());
        invalid = sample();
        invalid.arguments = vec!["a".repeat(MAX_INPUT_BYTES)];
        assert!(with_definition(&invalid, |_| ()).is_err());
        with_error("invalid profile configuration", |raw| {
            assert_eq!(
                unsafe { decode(raw) }.unwrap_err(),
                "invalid profile configuration"
            )
        });
    }
}
