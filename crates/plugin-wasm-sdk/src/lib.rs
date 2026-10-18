//! Safe authoring helpers for Velk's core-Wasm matching transport.
//! Plugin code implements [`Matching`] and invokes [`export_matching!`].

use velk_plugin_wasm_api as api;
pub use velk_plugin_wasm_api::Request;

/// A matching provider emits exactly one score per candidate, in input order.
/// Request strings borrow the guest input and must not escape this call.
pub trait Matching {
    fn rank(request: Request<'_>, scores: &mut Scores<'_>) -> Result<(), InvalidScores>;
}

/// The provider emitted too many or too few scores for its request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InvalidScores;

impl std::fmt::Display for InvalidScores {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("matching provider must emit one score per candidate")
    }
}

impl std::error::Error for InvalidScores {}

/// Checked output writer. `None` denotes an unmatched candidate.
pub struct Scores<'a> {
    remaining: &'a mut [u8],
}

impl Scores<'_> {
    pub fn push(&mut self, score: Option<[u64; 4]>) -> Result<(), InvalidScores> {
        if self.remaining.len() < api::SCORE_BYTES {
            return Err(InvalidScores);
        }
        let (record, remaining) =
            std::mem::take(&mut self.remaining).split_at_mut(api::SCORE_BYTES);
        api::write_score(record, score);
        self.remaining = remaining;
        Ok(())
    }
}

/// Runs the same checked adapter used by Wasm exports on ordinary byte slices.
/// This lets plugins test their complete request/response behavior natively.
pub fn dispatch<P: Matching>(input: &[u8], output: &mut [u8]) -> i32 {
    let Some(request) = Request::read(input) else {
        return api::INVALID;
    };
    let count = request.count;
    let Some(output) = output.get_mut(..count * api::SCORE_BYTES) else {
        return api::INVALID;
    };
    let mut scores = Scores { remaining: output };
    if P::rank(request, &mut scores).is_err() || !scores.remaining.is_empty() {
        return api::INVALID;
    }
    count as i32
}

/// Exports the matching ABI for a [`Matching`] implementation.
/// Invoke once at crate root in a `cdylib` targeting `wasm32-unknown-unknown`.
/// The SDK owns the reusable buffer; plugin code never handles pointers.
#[macro_export]
macro_rules! export_matching {
    ($provider:ty) => {
        #[cfg(target_arch = "wasm32")]
        #[allow(unsafe_code)]
        mod __velk_matching_exports {
            static BUFFER: $crate::__private::Buffer = $crate::__private::Buffer::new();

            #[unsafe(no_mangle)]
            pub extern "C" fn velk_abi_version() -> u32 {
                $crate::__private::VERSION
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn velk_allocate(length: u32) -> u32 {
                BUFFER.allocate(length)
            }

            #[unsafe(no_mangle)]
            pub extern "C" fn velk_matching_v1(
                input: u32,
                length: u32,
                output: u32,
                capacity: u32,
            ) -> i32 {
                BUFFER.rank::<$provider>(input, length, output, capacity)
            }
        }
    };
}

#[doc(hidden)]
pub mod __private {
    use super::*;
    use std::sync::Mutex;

    pub use api::VERSION;

    pub struct Buffer(Mutex<Option<Box<[u8]>>>);

    impl Default for Buffer {
        fn default() -> Self {
            Self::new()
        }
    }

    impl Buffer {
        pub const fn new() -> Self {
            Self(Mutex::new(None))
        }

        pub fn allocate(&self, length: u32) -> u32 {
            if length as usize != api::BUFFER_BYTES {
                return 0;
            }
            let Ok(mut allocation) = self.0.try_lock() else {
                return 0;
            };
            let buffer = allocation.get_or_insert_with(|| vec![0; api::BUFFER_BYTES].into());
            buffer.as_mut_ptr() as usize as u32
        }

        pub fn rank<P: Matching>(
            &self,
            input: u32,
            length: u32,
            output: u32,
            capacity: u32,
        ) -> i32 {
            let Ok(mut allocation) = self.0.try_lock() else {
                return api::INVALID;
            };
            let Some(buffer) = allocation.as_mut() else {
                return api::INVALID;
            };
            let base = buffer.as_ptr() as usize as u32;
            if input != base
                || base.checked_add(api::MAX_INPUT as u32) != Some(output)
                || length as usize > api::MAX_INPUT
                || capacity as usize > api::MAX_OUTPUT
            {
                return api::INVALID;
            }
            let (input, output) = buffer.split_at_mut(api::MAX_INPUT);
            dispatch::<P>(&input[..length as usize], &mut output[..capacity as usize])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Lengths;
    impl Matching for Lengths {
        fn rank(request: Request<'_>, scores: &mut Scores<'_>) -> Result<(), InvalidScores> {
            for candidate in request.candidates() {
                scores.push(Some([
                    candidate.len() as u64,
                    request.query.len() as u64,
                    0,
                    0,
                ]))?;
            }
            Ok(())
        }
    }

    fn request() -> Vec<u8> {
        let mut input = vec![0; 64];
        let length = api::write_request(&mut input, "界", &["a\0b", "世界", ""]).unwrap();
        input.truncate(length);
        input
    }

    #[test]
    fn checked_dispatch_preserves_order_unicode_and_borrowed_input() {
        let input = request();
        let mut output = vec![0; api::SCORE_BYTES * 3];
        assert_eq!(dispatch::<Lengths>(&input, &mut output), 3);
        assert_eq!(
            api::read_scores(&output, 3).unwrap(),
            [Some([3, 3, 0, 0]), Some([6, 3, 0, 0]), Some([0, 3, 0, 0])]
        );
    }

    #[test]
    fn malformed_input_and_small_output_fail_before_calling_provider() {
        struct Never;
        impl Matching for Never {
            fn rank(_: Request<'_>, _: &mut Scores<'_>) -> Result<(), InvalidScores> {
                panic!("invalid input must not reach a plugin")
            }
        }
        for input in [&[255; 4][..], &[0; 9], &[1, 0, 0, 0, 255, 0, 0, 0, 0]] {
            assert_eq!(dispatch::<Never>(input, &mut [0; 120]), api::INVALID);
        }
        assert_eq!(dispatch::<Never>(&request(), &mut [0; 119]), api::INVALID);
    }

    #[test]
    fn incomplete_or_excessive_scores_are_rejected() {
        struct Incomplete;
        impl Matching for Incomplete {
            fn rank(_: Request<'_>, _: &mut Scores<'_>) -> Result<(), InvalidScores> {
                Ok(())
            }
        }
        struct Excessive;
        impl Matching for Excessive {
            fn rank(_: Request<'_>, scores: &mut Scores<'_>) -> Result<(), InvalidScores> {
                loop {
                    scores.push(None)?;
                }
            }
        }
        assert_eq!(
            dispatch::<Incomplete>(&request(), &mut [0; 120]),
            api::INVALID
        );
        assert_eq!(
            dispatch::<Excessive>(&request(), &mut [0; 120]),
            api::INVALID
        );
    }

    #[test]
    fn owned_buffer_rejects_unallocated_overlapping_and_foreign_regions() {
        let buffer = __private::Buffer::new();
        assert_eq!(buffer.rank::<Lengths>(0, 0, 0, 0), api::INVALID);
        assert_eq!(buffer.allocate(1), 0);
        let base = buffer.allocate(api::BUFFER_BYTES as u32);
        assert_eq!(buffer.allocate(api::BUFFER_BYTES as u32), base);
        assert_eq!(buffer.rank::<Lengths>(base, 8, base, 40), api::INVALID);
        assert_eq!(buffer.rank::<Lengths>(base + 1, 8, base, 40), api::INVALID);
        assert_eq!(
            buffer.rank::<Lengths>(base, u32::MAX, base, 40),
            api::INVALID
        );
    }
}
