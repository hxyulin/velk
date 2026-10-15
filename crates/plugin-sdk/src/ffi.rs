#![allow(unsafe_code)]
use crate::{Plugin, api::*};
use std::{
    ffi::c_void,
    panic::{AssertUnwindSafe, catch_unwind},
};
fn guard(f: impl FnOnce() -> i32) -> i32 {
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
            id: fixed(P::ID),
            version: fixed(P::VERSION),
            create: Some(create::<P>),
            destroy: Some(destroy::<P>),
            describe: Some(describe::<P>),
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
