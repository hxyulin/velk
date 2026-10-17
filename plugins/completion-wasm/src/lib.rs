//! Bundled matching guest. Its only allocation belongs to the Wasm instance.
#[cfg(target_arch = "wasm32")]
#[allow(unsafe_code)]
mod exports {
    use velk_plugin_wasm_api as api;
    #[unsafe(no_mangle)]
    pub extern "C" fn velk_abi_version() -> u32 {
        api::VERSION
    }
    #[unsafe(no_mangle)]
    pub extern "C" fn velk_allocate(length: u32) -> u32 {
        if length as usize != api::BUFFER_BYTES {
            return 0;
        }
        Box::into_raw(vec![0u8; length as usize].into_boxed_slice()) as *mut u8 as u32
    }
    #[unsafe(no_mangle)]
    pub unsafe extern "C" fn velk_matching_v1(
        input: u32,
        length: u32,
        output: u32,
        capacity: u32,
    ) -> i32 {
        if length as usize > api::MAX_INPUT || capacity as usize > api::MAX_OUTPUT {
            return api::INVALID;
        }
        // SAFETY: the host supplies live, disjoint regions from velk_allocate.
        // Neither borrowed region escapes the call; the instance owns its allocation.
        let input = unsafe { std::slice::from_raw_parts(input as *const u8, length as usize) };
        let output =
            unsafe { std::slice::from_raw_parts_mut(output as *mut u8, capacity as usize) };
        let Some(request) = api::Request::read(input) else {
            return api::INVALID;
        };
        if output.len() < request.count * api::SCORE_BYTES {
            return api::INVALID;
        }
        let mut matcher = velk_completion_matching::Matcher::new(request.query);
        for (candidate, output) in request
            .candidates()
            .zip(output.as_chunks_mut::<{ api::SCORE_BYTES }>().0.iter_mut())
        {
            api::write_score(output, matcher.score(candidate).map(|score| score.fields()));
        }
        request.count as i32
    }
}
