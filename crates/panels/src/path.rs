//! Native path transport, without lossy UTF-8 conversion or filesystem access.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "encoding", content = "units", rename_all = "snake_case")]
pub enum NativePath {
    Unix(Vec<u8>),
    Windows(Vec<u16>),
}
impl NativePath {
    pub fn new(path: &Path) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            Self::Unix(path.as_os_str().as_bytes().to_vec())
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStrExt;
            Self::Windows(path.as_os_str().encode_wide().collect())
        }
    }
    pub fn to_path(&self) -> Result<PathBuf, String> {
        #[cfg(unix)]
        if let Self::Unix(bytes) = self {
            use std::os::unix::ffi::OsStringExt;
            if bytes.len() <= 32_768 && !bytes.contains(&0) {
                let path = PathBuf::from(std::ffi::OsString::from_vec(bytes.clone()));
                if path.is_absolute() {
                    return Ok(path);
                }
            }
        }
        #[cfg(windows)]
        if let Self::Windows(units) = self {
            use std::os::windows::ffi::OsStringExt;
            if units.len() <= 32_768 && !units.contains(&0) {
                let path = PathBuf::from(std::ffi::OsString::from_wide(units));
                if path.is_absolute() {
                    return Ok(path);
                }
            }
        }
        Err("invalid absolute native path or incompatible platform encoding".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    #[test]
    fn native_paths_roundtrip_without_utf8_and_reject_invalid_inputs() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(
            b"/tmp/name-\xff\n.rs".to_vec(),
        ));
        let encoded = crate::encode(&NativePath::new(&path)).unwrap();
        assert_eq!(
            crate::decode::<NativePath>(&encoded)
                .unwrap()
                .to_path()
                .unwrap(),
            path
        );
        for invalid in [
            NativePath::Unix(b"relative".to_vec()),
            NativePath::Unix(b"/nul\0".to_vec()),
            NativePath::Unix(vec![b'/'; 32769]),
            NativePath::Windows(vec![47]),
        ] {
            assert!(invalid.to_path().is_err());
        }
    }
}
