use super::{guard, registry};
use crate::{
    Plugin,
    api::*,
    host::{Request, Response},
};
use std::ffi::c_void;

/// A per-plugin host endpoint. Calls are synchronous and may wait for the UI;
/// use on plugin/background workers, never in pure interactive editing callbacks.
/// Clones retain host-owned storage; calls fail after unload or runtime shutdown.
pub struct Host(HostServices);
// SAFETY: the host contract requires thread-safe callbacks and retained context.
unsafe impl Send for Host {}
unsafe impl Sync for Host {}
impl Clone for Host {
    fn clone(&self) -> Self {
        unsafe { (self.0.retain)(self.0.context) };
        Self(self.0)
    }
}
impl Drop for Host {
    fn drop(&mut self) {
        unsafe { (self.0.release)(self.0.context) };
    }
}
impl Host {
    /// Invoke an exact declared dependency. Use only on an owned background
    /// worker; synchronous plugin-dispatch re-entry is rejected by the host.
    pub fn call_plugin(
        &self,
        provider: &str,
        hook: &str,
        input: Vec<u8>,
    ) -> Result<Vec<u8>, String> {
        match self.request(Request::CallPlugin {
            provider: provider.into(),
            hook: hook.into(),
            input,
        })? {
            Response::Plugin { output } => Ok(output),
            _ => Err("invalid plugin call reply".into()),
        }
    }
    pub fn request(&self, request: Request) -> Result<Response, String> {
        request.validate()?;
        let input = serde_json::to_vec(&request).map_err(|e| e.to_string())?;
        if input.len() > MAX_INPUT_BYTES {
            return Err("host request exceeds 1 MiB".into());
        }
        let mut output: Option<Result<Response, String>> = None;
        unsafe extern "C" fn collect(context: *mut c_void, bytes: Bytes, status: i32) -> i32 {
            guard(|| {
                if context.is_null()
                    || bytes.length > MAX_OUTPUT_BYTES
                    || (bytes.length != 0 && bytes.data.is_null())
                {
                    return INVALID;
                }
                let output = unsafe { &mut *context.cast::<Option<Result<Response, String>>>() };
                if output.is_some() {
                    return INVALID;
                }
                let bytes = if bytes.length == 0 {
                    &[]
                } else {
                    unsafe { std::slice::from_raw_parts(bytes.data, bytes.length) }
                };
                *output = Some(if status == OK {
                    serde_json::from_slice(bytes).map_err(|e| e.to_string())
                } else {
                    Err(String::from_utf8_lossy(bytes).into_owned())
                });
                OK
            })
        }
        let status = unsafe {
            (self.0.request)(
                self.0.context,
                Bytes {
                    data: input.as_ptr(),
                    length: input.len(),
                },
                collect,
                (&mut output as *mut Option<Result<Response, String>>).cast(),
            )
        };
        if status != OK {
            return Err(format!("host service failed ({status})"));
        }
        output.ok_or("host service returned no reply")?
    }
}
pub(super) fn register<P: Plugin>(reg: &mut registry::Registrar<P>) -> Result<(), String> {
    unsafe extern "C" fn attach<P: Plugin>(
        context: *mut c_void,
        services: *const HostServices,
    ) -> i32 {
        guard(|| {
            if context.is_null() || services.is_null() {
                return INVALID;
            }
            // SAFETY: attachment borrows a validated host table; acquire our own reference.
            let services = unsafe { *services };
            unsafe { (services.retain)(services.context) };
            let plugin = unsafe { &mut *context.cast::<P>() };
            plugin.attach_host(Host(services));
            OK
        })
    }
    // The instance owns both the boxed plugin and this registration until teardown.
    let binding = reg.binding(());
    let context = binding.plugin.cast();
    unsafe {
        reg.register_raw(
            HOST_HOOK,
            HOST_HOOK,
            1,
            SERVICE,
            HostApi {
                context,
                attach: attach::<P>,
            },
            binding,
        )
    }
}
