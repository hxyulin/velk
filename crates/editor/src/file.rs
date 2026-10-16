//! Streaming file I/O. Saving runs on RXUI's blocking pool, never the UI thread.
use ropey::Rope;
use std::{
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
};
use velk_core::Buffer;
use velk_editor_widget::Editor;

#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LineEnding {
    Lf,
    CrLf,
    Cr,
}
impl LineEnding {
    fn bytes(self) -> &'static [u8] {
        match self {
            Self::Lf => b"\n",
            Self::CrLf => b"\r\n",
            Self::Cr => b"\r",
        }
    }
}

struct TrackingReader<R> {
    reader: R,
    hash: blake3::Hasher,
    endings: [usize; 3],
    after_cr: bool,
}
impl<R: Read> Read for TrackingReader<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let count = self.reader.read(bytes)?;
        self.hash.update(&bytes[..count]);
        for &byte in &bytes[..count] {
            match byte {
                b'\r' => self.endings[2] += 1,
                b'\n' if self.after_cr => {
                    self.endings[2] -= 1;
                    self.endings[1] += 1;
                }
                b'\n' => self.endings[0] += 1,
                _ => {}
            }
            self.after_cr = byte == b'\r';
        }
        Ok(count)
    }
}

#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct FileState {
    /// Resolve the opened symlink once, so replacement updates its target.
    #[serde(with = "crate::session::path")]
    path: PathBuf,
    #[serde(with = "hash")]
    hash: blake3::Hash,
    ending: LineEnding,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiskStamp {
    length: u64,
    modified: Option<std::time::SystemTime>,
    #[cfg(unix)]
    identity: (u64, i64, i64),
}
impl DiskStamp {
    pub fn length(&self) -> u64 {
        self.length
    }
}
impl FileState {
    pub fn path(&self) -> &Path {
        &self.path
    }
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn open(path: &Path) -> io::Result<(Self, Editor)> {
        let (file, buffer) = Self::read(path)?;
        Ok((file, Editor::from_buffer(buffer)))
    }
    #[tracing::instrument(level = "debug", skip_all)]
    pub fn read(path: &Path) -> io::Result<(Self, Buffer)> {
        let path = fs::canonicalize(path)?;
        let file = File::open(&path)?;
        if !file.metadata()?.is_file() {
            return Err(io::Error::other("expected a regular file"));
        }
        let mut reader = TrackingReader {
            reader: BufReader::new(file),
            hash: blake3::Hasher::new(),
            endings: [0; 3],
            after_cr: false,
        };
        let editor = Buffer::from_reader(&mut reader)?;
        let max = reader.endings.iter().copied().max().unwrap();
        let index = reader.endings.iter().position(|&n| n == max).unwrap();
        let ending = [LineEnding::Lf, LineEnding::CrLf, LineEnding::Cr][index];
        Ok((
            Self {
                path,
                hash: reader.hash.finalize(),
                ending,
            },
            editor,
        ))
    }

    /// Select a destination independently of a document's identity. Existing
    /// files get a fresh conflict baseline; a new file uses no-clobber creation.
    /// Another open document must never be overwritten by Save As.
    pub fn save_as(
        source: Option<Self>,
        destination: PathBuf,
        text: Rope,
        occupied: &[PathBuf],
    ) -> io::Result<Self> {
        let destination = if destination.exists() {
            fs::canonicalize(destination)?
        } else {
            let parent = destination
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or(Path::new("."));
            fs::canonicalize(parent)?.join(
                destination
                    .file_name()
                    .ok_or_else(|| io::Error::other("file needs a name"))?,
            )
        };
        if occupied.contains(&destination) {
            return Err(io::Error::other(
                "destination is already open in another document",
            ));
        }
        if let Some(source) = &source
            && source.path == destination
        {
            return source.clone().save(text);
        }
        let ending = source.map_or(LineEnding::Lf, |s| s.ending);
        if destination.exists() {
            let metadata = fs::metadata(&destination)?;
            if !metadata.is_file() {
                return Err(io::Error::other("expected a regular file"));
            }
            let mut hash = blake3::Hasher::new();
            io::copy(&mut File::open(&destination)?, &mut hash)?;
            return Self {
                path: destination,
                hash: hash.finalize(),
                ending,
            }
            .save(text);
        }
        Self::create_new(destination, text, ending)
    }
    fn create_new(destination: PathBuf, text: Rope, ending: LineEnding) -> io::Result<Self> {
        let mut builder = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            builder.permissions(fs::Permissions::from_mode(0o666));
        }
        let mut temporary = builder.tempfile_in(destination.parent().unwrap())?;
        write_text(temporary.as_file_mut(), &text, ending)?;
        temporary.as_file_mut().rewind()?;
        let mut hash = blake3::Hasher::new();
        io::copy(temporary.as_file_mut(), &mut hash)?;
        temporary.as_file().sync_all()?;
        temporary
            .persist_noclobber(&destination)
            .map_err(|e| e.error)?;
        sync_parent(&destination);
        Ok(Self {
            path: destination,
            hash: hash.finalize(),
            ending,
        })
    }
    pub fn disk_stamp(&self) -> io::Result<DiskStamp> {
        let metadata = fs::metadata(&self.path)?;
        Ok(DiskStamp {
            length: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            identity: {
                use std::os::unix::fs::MetadataExt;
                (metadata.ino(), metadata.ctime(), metadata.ctime_nsec())
            },
        })
    }
    pub fn overwrite(self, text: Rope) -> io::Result<Self> {
        match File::open(&self.path) {
            Ok(mut current) => {
                let mut hash = blake3::Hasher::new();
                io::copy(&mut current, &mut hash)?;
                Self {
                    hash: hash.finalize(),
                    ..self
                }
                .save(text)
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                Self::create_new(self.path, text, self.ending)
            }
            Err(error) => Err(error),
        }
    }
    pub fn changed_on_disk(&self) -> io::Result<bool> {
        let mut hash = blake3::Hasher::new();
        io::copy(&mut File::open(&self.path)?, &mut hash)?;
        Ok(hash.finalize() != self.hash)
    }
    /// Write and sync a sibling temporary file, then replace the original.
    /// The content check detects ordinary external modifications. It is not an
    /// OS compare-and-swap: another process can still race the final rename.
    #[tracing::instrument(level = "debug", skip_all, fields(bytes = text.len_bytes()))]
    pub fn save(mut self, text: Rope) -> io::Result<Self> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("file has no parent directory"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        write_text(temporary.as_file_mut(), &text, self.ending)?;
        let metadata = fs::symlink_metadata(&self.path)?;
        if !metadata.is_file() || metadata.permissions().readonly() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "file is read-only or no longer a regular file",
            ));
        }
        let mut current_hash = blake3::Hasher::new();
        io::copy(&mut File::open(&self.path)?, &mut current_hash)?;
        if current_hash.finalize() != self.hash {
            return Err(io::Error::other(
                "file changed on disk; save cancelled to preserve the external changes",
            ));
        }
        temporary
            .as_file()
            .set_permissions(metadata.permissions())?;
        temporary.as_file_mut().rewind()?;
        let mut saved_hash = blake3::Hasher::new();
        io::copy(temporary.as_file_mut(), &mut saved_hash)?;
        temporary.as_file().sync_all()?;
        temporary.persist(&self.path).map_err(|error| error.error)?;
        // The rename is committed. A directory-sync error cannot undo it; keep
        // the new baseline and report a successful write rather than a false
        // conflict on the next save. File contents were synchronized above.
        #[cfg(unix)]
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        self.hash = saved_hash.finalize();
        Ok(self)
    }
}

fn write_text(file: &mut File, text: &Rope, ending: LineEnding) -> io::Result<()> {
    let mut writer = BufWriter::new(file);
    for chunk in text.chunks() {
        for part in chunk.split_inclusive('\n') {
            if let Some(content) = part.strip_suffix('\n') {
                writer.write_all(content.as_bytes())?;
                writer.write_all(ending.bytes())?;
            } else {
                writer.write_all(part.as_bytes())?;
            }
        }
    }
    writer.flush()
}
fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(directory) = File::open(parent)
    {
        let _ = directory.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn save_as_creates_new_files_preserves_source_and_endings_and_rejects_open_destinations() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("source.txt");
        let destination = directory.path().join("copy.txt");
        fs::write(&source, "original\r\n").unwrap();
        let (file, _) = FileState::open(&source).unwrap();
        let saved = FileState::save_as(
            Some(file.clone()),
            destination.clone(),
            Rope::from_str("new\ntext\n"),
            &[],
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&source).unwrap(), "original\r\n");
        assert_eq!(fs::read_to_string(&destination).unwrap(), "new\r\ntext\r\n");
        assert!(!saved.changed_on_disk().unwrap());
        let occupied = fs::canonicalize(&destination).unwrap();
        assert!(
            FileState::save_as(
                Some(file),
                destination.clone(),
                Rope::from_str("overwrite"),
                &[occupied]
            )
            .is_err()
        );
        assert_eq!(fs::read_to_string(&destination).unwrap(), "new\r\ntext\r\n");
        let blank = directory.path().join("blank.txt");
        FileState::save_as(None, blank.clone(), Rope::new(), &[]).unwrap();
        assert_eq!(fs::read(blank).unwrap(), b"");
    }
    #[test]
    fn approved_overwrite_rechecks_existing_files_and_recreates_deleted_files_without_clobbering() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        fs::write(&path, "original\r\n").unwrap();
        let (file, _) = FileState::open(&path).unwrap();
        fs::write(&path, "external").unwrap();
        assert!(file.clone().save(Rope::from_str("local\n")).is_err());
        let file = file.overwrite(Rope::from_str("local\n")).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "local\r\n");
        fs::remove_file(&path).unwrap();
        let file = file.overwrite(Rope::from_str("recreated\n")).unwrap();
        assert_eq!(fs::read_to_string(path).unwrap(), "recreated\r\n");
        assert!(!file.changed_on_disk().unwrap());
    }
    #[cfg(unix)]
    #[test]
    fn save_as_resolves_symlink_destinations_and_rejects_an_open_alias() {
        use std::os::unix::fs::symlink;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        let link = directory.path().join("link.txt");
        fs::write(&path, "original").unwrap();
        symlink(&path, &link).unwrap();
        let occupied = fs::canonicalize(&path).unwrap();
        assert!(
            FileState::save_as(None, link.clone(), Rope::from_str("new"), &[occupied]).is_err()
        );
        FileState::save_as(None, link.clone(), Rope::from_str("new"), &[]).unwrap();
        assert!(fs::symlink_metadata(link).unwrap().file_type().is_symlink());
        assert_eq!(fs::read_to_string(path).unwrap(), "new");
    }
    #[test]
    fn unicode_and_line_endings_round_trip_without_flattening_or_losing_final_newline() {
        for ending in ["\n", "\r\n", "\r"] {
            for trailing in ["", ending] {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("世界.txt");
                let source = format!("hello{ending}é 👩‍💻{trailing}");
                fs::write(&path, &source).unwrap();
                let (state, mut editor) = FileState::open(&path).unwrap();
                editor.insert("é");
                let state = state.save(editor.buffer().text().clone()).unwrap();
                assert_eq!(fs::read_to_string(&path).unwrap(), format!("é{source}"));
                state.save(editor.buffer().text().clone()).unwrap();
            }
        }
    }
    #[test]
    fn external_changes_or_deletion_fail_without_overwriting_or_leaking_temporary_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        fs::write(&path, "original").unwrap();
        let (state, _) = FileState::open(&path).unwrap();
        fs::write(&path, "external").unwrap();
        assert!(state.clone().save(Rope::from_str("edit")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "external");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 1);
        fs::remove_file(&path).unwrap();
        assert!(state.save(Rope::from_str("edit")).is_err());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 0);
    }
    #[cfg(unix)]
    #[test]
    fn symlink_target_and_executable_permissions_are_preserved_and_read_only_save_fails() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("file");
        let link = dir.path().join("link");
        fs::write(&path, "old").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o751)).unwrap();
        symlink(&path, &link).unwrap();
        let (state, _) = FileState::open(&link).unwrap();
        let state = state.save(Rope::from_str("new")).unwrap();
        assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(fs::read_to_string(&link).unwrap(), "new");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o751
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
        assert!(state.save(Rope::from_str("no")).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    }
    #[test]
    fn tracking_reader_handles_crlf_split_across_reads_and_mixed_endings_use_dominant_style() {
        let mut reader = TrackingReader {
            reader: &b"a\r\nb\rc\nd\r\n"[..],
            hash: blake3::Hasher::new(),
            endings: [0; 3],
            after_cr: false,
        };
        let mut byte = [0];
        while reader.read(&mut byte).unwrap() != 0 {}
        assert_eq!(reader.endings, [1, 2, 1]);
        assert_eq!(reader.hash.finalize(), blake3::hash(b"a\r\nb\rc\nd\r\n"));
    }
}

mod hash {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    pub fn serialize<S: Serializer>(hash: &blake3::Hash, serializer: S) -> Result<S::Ok, S::Error> {
        hash.as_bytes().serialize(serializer)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<blake3::Hash, D::Error> {
        <[u8; 32]>::deserialize(deserializer).map(blake3::Hash::from_bytes)
    }
}
