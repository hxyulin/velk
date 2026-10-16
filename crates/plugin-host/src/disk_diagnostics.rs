use std::{fs, io::Read, path::Path, time::SystemTime};
use velk_plugin_contracts::host::MAX_FILE_BYTES;

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    length: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, i64, i64),
}
impl Stamp {
    pub fn read(path: &Path) -> Option<Self> {
        let metadata = fs::metadata(path).ok()?;
        if !metadata.is_file() {
            return None;
        }
        Some(Self {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (metadata.ino(), metadata.ctime(), metadata.ctime_nsec())
            },
        })
    }
}
pub(crate) fn validate(path: &Path, expected: &str) -> Option<Stamp> {
    let before = Stamp::read(path)?;
    if before.length > MAX_FILE_BYTES as u64 {
        return None;
    }
    let file = fs::File::open(path).ok()?;
    if !file.metadata().ok()?.is_file() {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_FILE_BYTES as u64 + 1)
        .read_to_string(&mut text)
        .ok()?;
    if text.len() > MAX_FILE_BYTES || text.replace("\r\n", "\n").replace('\r', "\n") != expected {
        return None;
    }
    (Stamp::read(path).as_ref() == Some(&before)).then_some(before)
}
