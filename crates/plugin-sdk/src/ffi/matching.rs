use super::{guard, registry::Registrar};
use crate::commands::completion::matching::{self, Policy};
use crate::{Plugin, api::*};
impl<P: Plugin> Registrar<P> {
    /// Register a stateless local matcher, independent of the plugin instance.
    pub fn matching<M: Policy + Default + 'static>(&mut self) -> Result<(), String> {
        // SAFETY: the context-free callback borrows only caller-owned slices.
        unsafe {
            self.register_raw(
                matching::HOOK,
                matching::CONTRACT,
                1,
                SERVICE,
                MatchingApi {
                    scores: scores::<M>,
                },
                (),
            )
        }
    }
}
unsafe extern "C" fn scores<M: Policy + Default>(
    query: Bytes,
    candidates: *const Bytes,
    count: usize,
    output: *mut MatchScore,
) -> i32 {
    guard(|| {
        if count > 1000
            || query.length > 4096
            || query.data.is_null()
            || candidates.is_null()
            || output.is_null()
        {
            return INVALID;
        }
        let result: Result<(), String> = (|| {
            // SAFETY: the native service contract lends initialized storage for this call.
            let query = std::str::from_utf8(unsafe {
                std::slice::from_raw_parts(query.data, query.length)
            })
            .map_err(|e| e.to_string())?;
            let views = unsafe { std::slice::from_raw_parts(candidates, count) };
            let candidates = views
                .iter()
                .map(|view| {
                    if view.length > 4096 || view.data.is_null() {
                        return Err("invalid candidate".into());
                    }
                    std::str::from_utf8(unsafe {
                        std::slice::from_raw_parts(view.data, view.length)
                    })
                    .map_err(|e| e.to_string())
                })
                .collect::<Result<Vec<_>, String>>()?;
            let scores = M::default().scores(query, &candidates)?;
            if scores.len() != count {
                return Err("invalid score count".into());
            }
            for (index, score) in scores.into_iter().enumerate() {
                unsafe {
                    output.add(index).write(MatchScore {
                        matched: u32::from(score.is_some()),
                        score: score.unwrap_or_default(),
                    })
                };
            }
            Ok(())
        })();
        if result.is_ok() { OK } else { INVALID }
    })
}
