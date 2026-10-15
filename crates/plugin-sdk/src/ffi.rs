#![allow(unsafe_code)]
use crate::{Plugin, api::*};
pub mod language_server;
pub mod registry;
pub mod semantic;
pub(super) struct Instance<P: Plugin> {
    registry: registry::Registrar<P>,
    plugin: Box<P>,
}
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};
/// # Safety
/// Same requirements as `entry`; this wrapper supplies a C ABI function pointer.
pub unsafe extern "C" fn member<P: Plugin>(abi: u32, output: *mut PluginApi, size: usize) -> i32 {
    unsafe { entry::<P>(abi, output, size) }
}
/// # Safety
/// Output must be aligned/writable for size bytes. Member must obey PackMember.
pub unsafe fn pack_entry(
    abi: u32,
    output: *mut PluginPackApi,
    size: usize,
    count: usize,
    member: PackMember,
) -> i32 {
    if abi != ABI_VERSION || size != std::mem::size_of::<PluginPackApi>() {
        return INCOMPATIBLE;
    }
    if output.is_null() || count == 0 || count > MAX_PACK_MEMBERS {
        return INVALID;
    }
    unsafe {
        output.write(PluginPackApi {
            abi_version: ABI_VERSION,
            struct_size: size as u32,
            member_count: count,
            member: Some(member),
        });
    }
    OK
}
pub(super) fn guard(f: impl FnOnce() -> i32) -> i32 {
    catch_unwind(AssertUnwindSafe(f)).unwrap_or(PANICKED)
}
fn fixed<const N: usize>(s: &str) -> [u8; N] {
    assert!(
        s.len() < N && !s.contains('\0'),
        "plugin metadata exceeds ABI limits"
    );
    let mut out = [0; N];
    out[..s.len()].copy_from_slice(s.as_bytes());
    out
}
/// # Safety
/// `output` must point to writable, aligned storage of `size` bytes. After success,
/// calls must obey PluginApi's lifetime and serial execution contract.
pub unsafe fn entry<P: Plugin>(abi: u32, output: *mut PluginApi, size: usize) -> i32 {
    if abi != ABI_VERSION || size != std::mem::size_of::<PluginApi>() {
        return INCOMPATIBLE;
    }
    if output.is_null() {
        return INVALID;
    }
    guard(|| {
        if parse_plugin_id(P::ID).is_none() {
            return INVALID;
        }
        let api = PluginApi {
            abi_version: ABI_VERSION,
            struct_size: size as u32,
            flags: if P::RESIDENT_CODE { RESIDENT_CODE } else { 0 },
            id: fixed(P::ID),
            version: fixed(P::VERSION),
            create: Some(create::<P>),
            destroy: Some(destroy::<P>),
            describe: Some(describe::<P>),
            hook_count: Some(registry::count::<P>),
            hook_info: Some(registry::info::<P>),
            get_hook: Some(registry::get::<P>),
        };
        // SAFETY: required writable storage was supplied by the caller.
        unsafe {
            output.write(api);
        }
        OK
    })
}
unsafe extern "C" fn create<P: Plugin>(output: *mut *mut c_void) -> i32 {
    if output.is_null() {
        return INVALID;
    }
    guard(|| {
        let mut plugin = Box::new(P::default());
        let mut registry = registry::Registrar::new(&mut *plugin);
        if register_metadata(&mut registry).is_err()
            || P::register(&mut registry).is_err()
            || (registry.entries.len() < MAX_HOOKS && register_diagnostics(&mut registry).is_err())
        {
            return INVALID;
        }
        let instance = Box::into_raw(Box::new(Instance { registry, plugin })).cast();
        // SAFETY: ABI requires writable output storage.
        unsafe {
            output.write(instance);
        }
        OK
    })
}
unsafe extern "C" fn destroy<P: Plugin>(instance: *mut c_void) -> i32 {
    if instance.is_null() {
        return INVALID;
    }
    guard(|| {
        // SAFETY: the instance came from create<P>, and is destroyed exactly once.
        unsafe {
            drop(Box::from_raw(instance.cast::<Instance<P>>()));
        }
        OK
    })
}
unsafe extern "C" fn describe<P: Plugin>(
    instance: *mut c_void,
    output: *mut u8,
    capacity: usize,
    length: *mut usize,
) -> i32 {
    if instance.is_null() || length.is_null() || (capacity > 0 && output.is_null()) {
        return INVALID;
    }
    guard(|| {
        // SAFETY: caller exclusively owns this live instance for this call.
        let text = unsafe { (&mut *instance.cast::<Instance<P>>()).plugin.describe() };
        // SAFETY: ABI requires writable length storage and an output of capacity bytes.
        unsafe {
            length.write(text.len());
            if text.len() > capacity {
                return BUFFER_TOO_SMALL;
            }
            if !text.is_empty() {
                std::ptr::copy_nonoverlapping(text.as_ptr(), output, text.len());
            }
        }
        OK
    })
}
unsafe extern "C" fn render_markdown<P: Plugin, F>(
    instance: *mut c_void,
    source: *const u8,
    length: usize,
    sink: RunSink,
    context: *mut c_void,
) -> i32
where
    F: FnMut(&mut P, &str) -> crate::markup::Document + 'static,
{
    if instance.is_null() || source.is_null() || length > MAX_INPUT_BYTES {
        return INVALID;
    }
    guard(|| {
        // SAFETY: the host supplies readable input for this synchronous call.
        let source = unsafe { std::slice::from_raw_parts(source, length) };
        let Ok(source) = std::str::from_utf8(source) else {
            return INVALID;
        };
        // SAFETY: the host exclusively owns this live instance for the call.
        let binding = unsafe { &mut *instance.cast::<registry::Binding<P, F>>() };
        let document = (binding.handler)(unsafe { &mut *binding.plugin }, source);
        if document.runs.len() > MAX_RUNS {
            return INVALID;
        }
        let size = document.runs.iter().try_fold(0usize, |n, r| {
            n.checked_add(r.text.len())?
                .checked_add(r.style.link.as_ref().map_or(0, String::len))
        });
        if size.is_none_or(|n| n > MAX_OUTPUT_BYTES) {
            return INVALID;
        }
        let runs: Vec<_> = document
            .runs
            .iter()
            .map(|run| {
                let style = &run.style;
                let link = style.link.as_deref().unwrap_or("");
                StyledRun {
                    text: Bytes {
                        data: run.text.as_ptr(),
                        length: run.text.len(),
                    },
                    link: Bytes {
                        data: link.as_ptr(),
                        length: link.len(),
                    },
                    flags: (u32::from(style.bold) * BOLD)
                        | (u32::from(style.italic) * ITALIC)
                        | (u32::from(style.code) * CODE)
                        | (u32::from(style.strikethrough) * STRIKETHROUGH)
                        | (u32::from(style.link.is_some()) * LINK),
                    heading: style.heading,
                }
            })
            .collect();
        // SAFETY: the host callback/context is live, and all views into document
        // and runs stay valid until the callback returns. Nothing escapes.
        unsafe { sink(context, runs.as_ptr(), runs.len()) }
    })
}
fn register_diagnostics<P: Plugin>(reg: &mut registry::Registrar<P>) -> Result<(), String> {
    unsafe extern "C" fn attach(sink: *const TraceSink) -> i32 {
        guard(|| {
            if sink.is_null() {
                return INVALID;
            }
            // SAFETY: the host lends a valid table and owns its callbacks until
            // process exit. SDK copies the table, retaining no borrowed pointer.
            unsafe {
                velk_diagnostics::attach(*sink);
            }
            OK
        })
    }
    unsafe {
        reg.register_raw(
            DIAGNOSTICS_HOOK,
            DIAGNOSTICS_HOOK,
            1,
            SERVICE,
            DiagnosticsApi { attach },
            (),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pack_discovery_checks_abi_layout_and_count_without_calling_factories() {
        unsafe extern "C" fn member(_: usize, _: u32, _: *mut PluginApi, _: usize) -> i32 {
            NOT_FOUND
        }
        let size = std::mem::size_of::<PluginPackApi>();
        // SAFETY: rejected queries never touch output; success uses aligned storage.
        unsafe {
            for (abi, bytes, count, expected) in [
                (99, size, 1, INCOMPATIBLE),
                (1, 0, 1, INCOMPATIBLE),
                (1, size, 0, INVALID),
                (1, size, MAX_PACK_MEMBERS + 1, INVALID),
            ] {
                assert_eq!(
                    pack_entry(abi, std::ptr::null_mut(), bytes, count, member),
                    expected
                );
            }
            let mut output = std::mem::MaybeUninit::uninit();
            assert_eq!(
                pack_entry(ABI_VERSION, output.as_mut_ptr(), size, 2, member),
                OK
            );
            let pack = output.assume_init();
            assert_eq!(pack.member_count, 2);
            assert_eq!(
                pack.member.unwrap()(2, ABI_VERSION, std::ptr::null_mut(), 0),
                NOT_FOUND
            );
        }
    }
    #[derive(Default)]
    struct Sample;
    impl Plugin for Sample {
        const ID: &'static str = "test.sample";
        const VERSION: &'static str = "1.0.0";
        fn describe(&mut self) -> String {
            "Hello 世界".into()
        }
    }
    #[test]
    fn rejects_incompatible_queries_before_touching_output_and_copies_owned_results() {
        // SAFETY: mismatch paths never touch output; successful calls use live storage.
        unsafe {
            assert_eq!(entry::<Sample>(999, std::ptr::null_mut(), 0), INCOMPATIBLE);
            let mut api = std::mem::MaybeUninit::uninit();
            assert_eq!(
                entry::<Sample>(
                    ABI_VERSION,
                    api.as_mut_ptr(),
                    std::mem::size_of::<PluginApi>()
                ),
                OK
            );
            let api = api.assume_init();
            let mut instance = std::ptr::null_mut();
            assert_eq!(api.create.unwrap()(&mut instance), OK);
            let mut length = 0;
            assert_eq!(
                api.describe.unwrap()(instance, std::ptr::null_mut(), 0, &mut length),
                BUFFER_TOO_SMALL
            );
            let mut bytes = vec![0; length];
            assert_eq!(
                api.describe.unwrap()(instance, bytes.as_mut_ptr(), bytes.len(), &mut length),
                OK
            );
            assert_eq!(api.destroy.unwrap()(instance), OK);
            assert_eq!(String::from_utf8(bytes).unwrap(), "Hello 世界");
        }
    }
    #[derive(Default)]
    struct Panicking;
    impl Plugin for Panicking {
        const ID: &'static str = "test.panic";
        const VERSION: &'static str = "1";
        fn describe(&mut self) -> String {
            panic!("plugin panic must stay inside the SDK")
        }
    }
    #[test]
    fn plugin_panics_return_a_status_instead_of_unwinding_across_c() {
        // SAFETY: the instance and buffers are valid and calls are serialized.
        unsafe {
            let mut instance = std::ptr::null_mut();
            assert_eq!(create::<Panicking>(&mut instance), OK);
            let mut length = 0;
            assert_eq!(
                describe::<Panicking>(instance, std::ptr::null_mut(), 0, &mut length),
                PANICKED
            );
            assert_eq!(destroy::<Panicking>(instance), OK);
        }
    }
}

fn register_metadata<P: Plugin>(registry: &mut registry::Registrar<P>) -> Result<(), String> {
    let metadata = crate::PluginMetadata::of::<P>();
    metadata.validate()?;
    let bytes = serde_json::to_vec(&metadata).map_err(|e| e.to_string())?;
    registry.provide(
        registry::Request(METADATA_HOOK),
        move |_: &mut P, input: &[u8]| {
            if !input.is_empty() {
                return Err("metadata request must be empty".into());
            }
            Ok(bytes.clone())
        },
    )
}
