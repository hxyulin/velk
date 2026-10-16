use super::{guard, registry::Registrar};
use crate::{Plugin, api::*};
use std::ffi::c_void;
use velk_editing::{Decision, Request, Role};

/// Stateless, reentrant editing policy. Settings arrive in each request; no
/// plugin instance or UI object is shared with interactive evaluation.
pub trait Behavior: 'static {
    const ROLE: Role;
    fn evaluate(request: &Request) -> Result<Decision, String>;
}
impl<P: Plugin> Registrar<P> {
    pub fn editing<B: Behavior>(&mut self) -> Result<(), String> {
        unsafe {
            self.register_raw(
                B::ROLE.hook(),
                velk_editing::CONTRACT,
                1,
                SERVICE,
                EditingApi {
                    evaluate: evaluate::<B>,
                },
                (),
            )
        }
    }
}
unsafe extern "C" fn evaluate<B: Behavior>(
    input: Bytes,
    sink: ByteSink,
    context: *mut c_void,
) -> i32 {
    guard(|| {
        if input.length > velk_editing::MAX_WIRE || input.data.is_null() {
            return INVALID;
        }
        // SAFETY: the host lends initialized bytes for this call only.
        let input = unsafe { std::slice::from_raw_parts(input.data, input.length) };
        let result = (|| {
            let request: Request = serde_json::from_slice(input).map_err(|e| e.to_string())?;
            request.validate()?;
            if request.intent.role() != B::ROLE {
                return Err("wrong editing role".into());
            }
            let decision = B::evaluate(&request)?;
            decision.validate(&request)?;
            serde_json::to_vec(&decision).map_err(|e| e.to_string())
        })();
        let (bytes, status) = match result {
            Ok(bytes) => (bytes, OK),
            Err(error) => (error.into_bytes(), INVALID),
        };
        if bytes.len() > velk_editing::MAX_WIRE {
            return INVALID;
        }
        unsafe {
            sink(
                context,
                Bytes {
                    data: bytes.as_ptr(),
                    length: bytes.len(),
                },
                status,
            )
        }
    })
}
