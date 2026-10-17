use velk_plugin_api::{BUFFER_TOO_SMALL, OK};
pub(super) fn bounded_output(
    maximum: usize,
    mut call: impl FnMut(&mut [u8], &mut usize) -> i32,
) -> Result<Vec<u8>, String> {
    let mut output = vec![0; maximum.min(4096)];
    for attempt in 0..2 {
        let mut length = 0;
        let status = call(&mut output, &mut length);
        if status == OK && length <= output.len() {
            output.truncate(length);
            return Ok(output);
        }
        if status != BUFFER_TOO_SMALL || attempt != 0 || length <= output.len() || length > maximum
        {
            return Err("invalid or oversized plugin response".into());
        }
        output.resize(length, 0);
    }
    unreachable!()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn output_is_bounded_and_retries_only_one_valid_capacity_request() {
        let mut calls = 0;
        let output = bounded_output(8192, |buffer, length| {
            calls += 1;
            *length = 6000;
            if buffer.len() < *length {
                return BUFFER_TOO_SMALL;
            }
            buffer[..*length].fill(42);
            OK
        })
        .unwrap();
        assert_eq!(calls, 2);
        assert_eq!(output, vec![42; 6000]);
        for length in [0, 4096, 8193, usize::MAX] {
            let mut calls = 0;
            assert!(
                bounded_output(8192, |_, size| {
                    calls += 1;
                    *size = length;
                    BUFFER_TOO_SMALL
                })
                .is_err()
            );
            assert_eq!(calls, 1);
        }
        assert!(
            bounded_output(8192, |_, length| {
                *length = 9000;
                OK
            })
            .is_err()
        );
    }
}
