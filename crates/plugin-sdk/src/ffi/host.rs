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
    /// Call the selected implementation of a declared interface from a background worker.
    pub fn call_interface(&self, interface: &str, input: Vec<u8>) -> Result<Vec<u8>, String> {
        match self.request(Request::CallInterface {
            interface: interface.into(),
            input,
        })? {
            Response::Plugin { output } => Ok(output),
            _ => Err("invalid interface call reply".into()),
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

#[cfg(feature = "test-support")]
pub(crate) fn mock(
    handler: impl Fn(Request) -> Result<Response, String> + Send + Sync + 'static,
) -> Host {
    use std::sync::Arc;
    type Handler = Box<dyn Fn(Request) -> Result<Response, String> + Send + Sync>;
    unsafe extern "C" fn retain(context: *mut c_void) {
        // SAFETY: mock owns one Arc reference for every Host clone.
        unsafe { Arc::<Handler>::increment_strong_count(context.cast()) };
    }
    unsafe extern "C" fn release(context: *mut c_void) {
        // SAFETY: every release consumes a reference acquired by mock or retain.
        unsafe { Arc::<Handler>::decrement_strong_count(context.cast()) };
    }
    unsafe extern "C" fn request(
        context: *mut c_void,
        input: Bytes,
        sink: ByteSink,
        reply_context: *mut c_void,
    ) -> i32 {
        guard(|| {
            // SAFETY: Host pins the Arc and supplies a valid, bounded input view.
            let handler = unsafe { &*context.cast::<Handler>() };
            let input = unsafe { std::slice::from_raw_parts(input.data, input.length) };
            let response = serde_json::from_slice::<Request>(input)
                .map_err(|error| error.to_string())
                .and_then(handler);
            let (status, bytes) = match response.and_then(|response| {
                serde_json::to_vec(&response).map_err(|error| error.to_string())
            }) {
                Ok(bytes) => (OK, bytes),
                Err(error) => (INVALID, error.into_bytes()),
            };
            // SAFETY: the caller owns the synchronous sink/context; bytes live through it.
            unsafe {
                sink(
                    reply_context,
                    Bytes {
                        data: bytes.as_ptr(),
                        length: bytes.len(),
                    },
                    status,
                )
            }
        })
    }
    let handler: Arc<Handler> = Arc::new(Box::new(handler));
    Host(HostServices {
        context: Arc::into_raw(handler).cast_mut().cast(),
        request,
        retain,
        release,
    })
}
