use super::{guard, registry::Registrar};
use crate::commands::snippet::{self, Provider, Request};
use crate::{Plugin, api::*};
impl<P: Plugin> Registrar<P> {
    pub fn snippets<F: Provider + Default + 'static>(&mut self) -> Result<(), String> {
        // SAFETY: the stateless callback borrows caller storage only.
        unsafe {
            self.register_raw(
                snippet::HOOK,
                snippet::CONTRACT,
                1,
                SERVICE,
                SnippetApi {
                    evaluate: evaluate::<F>,
                },
                (),
            )
        }
    }
}
unsafe extern "C" fn evaluate<F: Provider + Default>(
    input: Bytes,
    output: *mut u8,
    capacity: usize,
    length: *mut usize,
) -> i32 {
    guard(|| {
        if input.data.is_null()
            || output.is_null()
            || length.is_null()
            || input.length > snippet::MAX_BYTES
        {
            return INVALID;
        }
        // SAFETY: the service contract lends initialized input for this call.
        let bytes = unsafe { std::slice::from_raw_parts(input.data, input.length) };
        let response = serde_json::from_slice::<Request>(bytes)
            .map_err(|error| error.to_string())
            .and_then(|request| snippet::evaluate(&F::default(), request));
        let Ok(bytes) = serde_json::to_vec(&response) else {
            return INVALID;
        };
        if bytes.len() > snippet::MAX_BYTES {
            return INVALID;
        }
        unsafe {
            length.write(bytes.len());
        }
        if bytes.len() > capacity {
            return BUFFER_TOO_SMALL;
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len());
        }
        OK
    })
}
