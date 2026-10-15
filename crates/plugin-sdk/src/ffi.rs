#![allow(unsafe_code)]
use crate::{Plugin, api::*};
pub mod semantic;
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};
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
        let api = PluginApi {
            abi_version: ABI_VERSION,
            struct_size: size as u32,
            flags: if P::RESIDENT_CODE { RESIDENT_CODE } else { 0 },
            id: fixed(P::ID),
            version: fixed(P::VERSION),
            create: Some(create::<P>),
            destroy: Some(destroy::<P>),
            describe: Some(describe::<P>),
            semantic_create: P::SEMANTIC.then_some(semantic::create::<P>),
            semantic_input: P::SEMANTIC.then_some(semantic::input),
            semantic_destroy: P::SEMANTIC.then_some(semantic::destroy),
            render_markdown: if P::MARKDOWN {
                Some(render_markdown::<P>)
            } else {
                None
            },
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
        let instance = Box::into_raw(Box::new(P::default())).cast();
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
            drop(Box::from_raw(instance.cast::<P>()));
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
        let text = unsafe { (&mut *instance.cast::<P>()).describe() };
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
unsafe extern "C" fn render_markdown<P: Plugin>(
    instance: *mut c_void,
    source: *const u8,
    length: usize,
    sink: RunSink,
    context: *mut c_void,
) -> i32 {
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
        let document = unsafe { (&mut *instance.cast::<P>()).render_markdown(source) };
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
#[cfg(test)]
mod tests {
    use super::*;
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
