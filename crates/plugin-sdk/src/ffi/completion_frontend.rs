use super::{guard, registry::Registrar};
use crate::commands::completion::frontend::{self, Provider, Snapshot};
use crate::{Plugin, api::*};
impl<P: Plugin> Registrar<P> {
    pub fn completion_frontend<F: Provider + Default + 'static>(&mut self) -> Result<(), String> {
        // SAFETY: the stateless callback borrows caller storage only.
        unsafe {
            self.register_raw(
                frontend::HOOK,
                frontend::CONTRACT,
                1,
                SERVICE,
                CompletionFrontendApi {
                    present: present::<F>,
                },
                (),
            )
        }
    }
}
unsafe extern "C" fn present<F: Provider + Default>(
    input: Bytes,
    output: *mut u8,
    capacity: usize,
    length: *mut usize,
) -> i32 {
    guard(|| {
        if input.data.is_null()
            || output.is_null()
            || length.is_null()
            || input.length > frontend::MAX_BYTES
        {
            return INVALID;
        }
        let result: Result<Vec<u8>, String> = (|| {
            // SAFETY: all views are borrowed for this call by the service contract.
            let bytes = unsafe { std::slice::from_raw_parts(input.data, input.length) };
            let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(|e| e.to_string())?;
            snapshot.validate()?;
            let view = F::default().present(&snapshot)?;
            view.validate(&snapshot)?;
            let bytes = serde_json::to_vec(&view).map_err(|e| e.to_string())?;
            if bytes.len() > frontend::MAX_BYTES {
                return Err("frontend response too large".into());
            }
            Ok(bytes)
        })();
        let Ok(bytes) = result else {
            return INVALID;
        };
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
