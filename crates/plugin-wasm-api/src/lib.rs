//! Core-Wasm matching ABI. All integers are little endian; no Rust layout is shared.
//! Input: query length + UTF-8, candidate count, then length + UTF-8 per candidate.
//! Output: one 40-byte record per candidate: u64 match flag followed by four scores.
pub const VERSION: u32 = 1;
pub const MAX_TEXT: usize = 4096;
pub const MAX_CANDIDATES: usize = 1000;
pub const MAX_INPUT: usize = 8 + MAX_TEXT + MAX_CANDIDATES * (4 + MAX_TEXT);
pub const SCORE_BYTES: usize = 40;
pub const MAX_OUTPUT: usize = MAX_CANDIDATES * SCORE_BYTES;
pub const BUFFER_BYTES: usize = MAX_INPUT + MAX_OUTPUT;
pub const INVALID: i32 = -1;

pub fn request_size(query: &str, candidates: &[&str]) -> Result<usize, &'static str> {
    if query.len() > MAX_TEXT
        || candidates.len() > MAX_CANDIDATES
        || candidates
            .iter()
            .any(|candidate| candidate.len() > MAX_TEXT)
    {
        return Err("matching request exceeds limits");
    }
    Ok(8 + query.len() + candidates.iter().map(|s| 4 + s.len()).sum::<usize>())
}
/// Writes directly into caller-provided linear memory, without a staging buffer.
pub fn write_request(
    output: &mut [u8],
    query: &str,
    candidates: &[&str],
) -> Result<usize, &'static str> {
    let length = request_size(query, candidates)?;
    let mut output = output.get_mut(..length).ok_or("input buffer too small")?;
    fn bytes(output: &mut &mut [u8], value: &[u8]) {
        let (head, tail) = std::mem::take(output).split_at_mut(value.len());
        head.copy_from_slice(value);
        *output = tail;
    }
    bytes(&mut output, &(query.len() as u32).to_le_bytes());
    bytes(&mut output, query.as_bytes());
    bytes(&mut output, &(candidates.len() as u32).to_le_bytes());
    for candidate in candidates {
        bytes(&mut output, &(candidate.len() as u32).to_le_bytes());
        bytes(&mut output, candidate.as_bytes());
    }
    Ok(length)
}
fn integer(input: &mut &[u8]) -> Option<usize> {
    let (head, tail) = input.split_at_checked(4)?;
    *input = tail;
    Some(u32::from_le_bytes(head.try_into().ok()?) as usize)
}
fn string<'a>(input: &mut &'a [u8]) -> Option<&'a str> {
    let length = integer(input)?;
    if length > MAX_TEXT {
        return None;
    }
    let (head, tail) = input.split_at_checked(length)?;
    *input = tail;
    std::str::from_utf8(head).ok()
}
/// Validated borrowed request. Candidates borrow the backing linear memory.
pub struct Request<'a> {
    pub query: &'a str,
    pub count: usize,
    candidates: &'a [u8],
}
impl<'a> Request<'a> {
    pub fn read(mut input: &'a [u8]) -> Option<Self> {
        if input.len() > MAX_INPUT {
            return None;
        }
        let query = string(&mut input)?;
        let count = integer(&mut input)?;
        if count > MAX_CANDIDATES {
            return None;
        }
        let candidates = input;
        for _ in 0..count {
            string(&mut input)?;
        }
        input.is_empty().then_some(Self {
            query,
            count,
            candidates,
        })
    }
    pub fn candidates(&self) -> impl Iterator<Item = &'a str> {
        let mut input = self.candidates;
        (0..self.count).map(move |_| string(&mut input).expect("validated candidate"))
    }
}
pub fn write_score(output: &mut [u8], score: Option<[u64; 4]>) {
    let values = score.map_or([0; 5], |s| [1, s[0], s[1], s[2], s[3]]);
    for (bytes, value) in output.as_chunks_mut::<8>().0.iter_mut().zip(values) {
        bytes.copy_from_slice(&value.to_le_bytes());
    }
}
pub fn read_scores(input: &[u8], count: usize) -> Result<Vec<Option<[u64; 4]>>, &'static str> {
    if count > MAX_CANDIDATES || input.len() != count * SCORE_BYTES {
        return Err("invalid matching output length");
    }
    input
        .as_chunks::<SCORE_BYTES>()
        .0
        .iter()
        .map(|record| {
            let values: [u64; 5] = std::array::from_fn(|i| {
                u64::from_le_bytes(record[i * 8..i * 8 + 8].try_into().unwrap())
            });
            match values[0] {
                0 if values[1..].iter().all(|value| *value == 0) => Ok(None),
                1 => Ok(Some([values[1], values[2], values[3], values[4]])),
                _ => Err("invalid matching flag or unmatched record"),
            }
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn borrowed_requests_and_fixed_scores_round_trip() {
        let labels = ["print", "世界\0接口", ""];
        let mut bytes = vec![0; request_size("pritn", &labels).unwrap()];
        write_request(&mut bytes, "pritn", &labels).unwrap();
        let request = Request::read(&bytes).unwrap();
        assert_eq!(request.query, "pritn");
        assert_eq!(request.candidates().collect::<Vec<_>>(), labels);
        assert!(request.query.as_ptr() >= bytes.as_ptr());
        let mut output = vec![0; SCORE_BYTES * 2];
        write_score(&mut output[..SCORE_BYTES], Some([u64::MAX, 2, 3, 4]));
        write_score(&mut output[SCORE_BYTES..], None);
        assert_eq!(
            read_scores(&output, 2).unwrap(),
            [Some([u64::MAX, 2, 3, 4]), None]
        );
    }
    #[test]
    fn malformed_lengths_utf8_trailing_data_and_flags_are_rejected() {
        assert!(Request::read(&[255; 4]).is_none());
        assert!(Request::read(&[1, 0, 0, 0, 255, 0, 0, 0, 0]).is_none());
        assert!(Request::read(&[0; 9]).is_none());
        assert!(write_request(&mut [], "", &[]).is_err());
        assert!(request_size(&"x".repeat(MAX_TEXT + 1), &[]).is_err());
        assert!(request_size("", &vec![""; MAX_CANDIDATES + 1]).is_err());
        let mut record = [0; SCORE_BYTES];
        record[0] = 2;
        assert!(read_scores(&record, 1).is_err());
        assert!(read_scores(&record[..39], 1).is_err());
        record[0] = 0;
        record[8] = 1;
        assert!(read_scores(&record, 1).is_err());
    }
}
