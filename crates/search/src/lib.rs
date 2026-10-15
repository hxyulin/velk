//! UI-independent current-buffer search contract. Owned Rust values stay local;
//! the named request carries bounded JSON bytes across the plugin ABI.
use serde::{Deserialize, Serialize};
pub const HOOK: &str = "velk.search.buffer.v1";
pub const DEFAULT_BACKEND: &str = "velk.search.literal";
pub const MAX_DOCUMENT_BYTES: usize = 64 * 1024 * 1024;
pub const CHUNK_BYTES: usize = 64 * 1024;
pub const MAX_QUERY_BYTES: usize = 1024;
pub const MAX_MATCHES: usize = 10_000;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub case_sensitive: bool,
    pub whole_word: bool,
}
impl Query {
    pub fn validate(&self) -> Result<(), String> {
        if self.text.is_empty() || self.text.len() > MAX_QUERY_BYTES {
            return Err("Search text must contain 1–1024 UTF-8 bytes".into());
        }
        Ok(())
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Match {
    /// Unicode scalar offsets in the submitted snapshot, not UTF-8 bytes.
    pub start: usize,
    pub end: usize,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Results {
    pub id: u64,
    pub revision: u64,
    pub matches: Vec<Match>,
    /// More matches exist beyond the bounded returned list.
    pub truncated: bool,
}
impl Results {
    pub fn validate(&self, id: u64, revision: u64, chars: usize) -> Result<(), String> {
        if self.id != id || self.revision != revision || self.matches.len() > MAX_MATCHES {
            return Err("Search result identity or size is invalid".into());
        }
        let mut end = 0;
        for m in &self.matches {
            if m.start < end || m.start >= m.end || m.end > chars {
                return Err("Search returned invalid or overlapping ranges".into());
            }
            end = m.end;
        }
        Ok(())
    }
}
/// One bounded upload/search at a time per plugin instance. Begin supersedes
/// prior work; chunks are UTF-8 and may split lines but not Unicode scalars.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    Begin {
        id: u64,
        revision: u64,
        query: Query,
    },
    Append {
        id: u64,
        text: String,
    },
    Start {
        id: u64,
    },
    Poll {
        id: u64,
    },
    Cancel {
        id: u64,
    },
}
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "state", content = "data", rename_all = "snake_case")]
pub enum Reply {
    Accepted,
    Pending,
    Ready(Results),
}
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, String> {
    serde_json::to_vec(value).map_err(|e| e.to_string())
}
pub fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    serde_json::from_slice(bytes).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_stale_unsorted_empty_overlapping_and_out_of_bounds_ranges() {
        let mut r = Results {
            id: 4,
            revision: 8,
            matches: vec![Match { start: 0, end: 2 }],
            truncated: false,
        };
        assert!(r.validate(4, 8, 4).is_ok());
        assert!(r.validate(3, 8, 4).is_err());
        assert!(r.validate(4, 7, 4).is_err());
        assert!(r.validate(4, 8, 1).is_err());
        r.matches.push(Match { start: 1, end: 3 });
        assert!(r.validate(4, 8, 4).is_err());
        r.matches = vec![Match { start: 1, end: 1 }];
        assert!(r.validate(4, 8, 4).is_err());
    }
}
