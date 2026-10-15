//! Streaming file I/O. Saving runs on RXUI's blocking pool, never the UI thread.
use ropey::Rope;
use std::{
    fs::{self, File},
    io::{self, BufReader, BufWriter, Read, Seek, Write},
    path::{Path, PathBuf},
};
use velk_editor_widget::Editor;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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

#[derive(Clone)]
pub struct FileState {
    /// Resolve the opened symlink once, so replacement updates its target.
    path: PathBuf,
    hash: blake3::Hash,
    ending: LineEnding,
}
impl FileState {
    pub fn open(path: &Path) -> io::Result<(Self, Editor)> {
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
        let editor = Editor::from_reader(&mut reader)?;
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

    /// Write and sync a sibling temporary file, then replace the original.
    /// The content check detects ordinary external modifications. It is not an
    /// OS compare-and-swap: another process can still race the final rename.
    pub fn save(mut self, text: Rope) -> io::Result<Self> {
        let parent = self
            .path
            .parent()
            .ok_or_else(|| io::Error::other("file has no parent directory"))?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        {
            let mut writer = BufWriter::new(temporary.as_file_mut());
            for chunk in text.chunks() {
                for part in chunk.split_inclusive('\n') {
                    if let Some(content) = part.strip_suffix('\n') {
                        writer.write_all(content.as_bytes())?;
                        writer.write_all(self.ending.bytes())?;
                    } else {
                        writer.write_all(part.as_bytes())?;
                    }
                }
            }
            writer.flush()?;
        }
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

#[cfg(test)]
mod tests {
    use super::*;
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
