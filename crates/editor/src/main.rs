use rxui::prelude::*;
use std::{
    ffi::OsString,
    fmt,
    fs::File,
    io,
    path::{Path, PathBuf},
    process::ExitCode,
};
use velk_editor_widget::{Editor, Language};

#[derive(Debug)]
enum Error {
    Usage,
    Open { path: PathBuf, source: io::Error },
    Application(ApplicationError),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage => write!(f, "expected exactly one file path\nUsage: velk <file>"),
            Self::Open { path, source } => write!(f, "cannot open '{}': {source}", path.display()),
            Self::Application(error) => write!(f, "{error}"),
        }
    }
}

fn file_argument(mut arguments: impl Iterator<Item = OsString>) -> Result<PathBuf, Error> {
    let path = arguments.next().ok_or(Error::Usage)?;
    if arguments.next().is_some() {
        return Err(Error::Usage);
    }
    Ok(PathBuf::from(path))
}

fn open(path: &Path) -> Result<Editor, Error> {
    File::open(path)
        .and_then(Editor::from_reader)
        .map_err(|source| Error::Open {
            path: path.to_path_buf(),
            source,
        })
        .and_then(|mut editor| {
            editor
                .set_language(Language::for_path(path))
                .map_err(|error| Error::Open {
                    path: path.to_path_buf(),
                    source: io::Error::other(error),
                })?;
            Ok(editor)
        })
}

fn run() -> Result<(), Error> {
    let path = file_argument(std::env::args_os().skip(1))?;
    let editor = open(&path)?;
    let title = format!("Velk — {}", path.display());
    Application::new()
        .run(move |cx| {
            let editor = cx.new(|_| editor);
            cx.open_window(WindowOptions::new().title(title).size(1280., 800.), editor)?;
            Ok(())
        })
        .map_err(Error::Application)
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("velk: {error}");
            if matches!(error, Error::Usage) {
                ExitCode::from(2)
            } else {
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requires_exactly_one_path_and_keeps_spaces_and_flag_like_names() {
        assert!(matches!(
            file_argument(std::iter::empty()),
            Err(Error::Usage)
        ));
        assert!(matches!(
            file_argument(["one".into(), "two".into()].into_iter()),
            Err(Error::Usage)
        ));
        for path in ["folder/a file.txt", "世界.rs", "--showcase"] {
            assert_eq!(
                file_argument([path.into()].into_iter()).unwrap(),
                PathBuf::from(path)
            );
        }
    }

    #[test]
    fn rust_extension_enables_in_process_parser_and_other_files_stay_plain() {
        assert_eq!(
            Language::for_path(Path::new("main.rs")),
            Some(Language::Rust)
        );
        assert_eq!(
            Language::for_path(Path::new("MAIN.RS")),
            Some(Language::Rust)
        );
        assert_eq!(Language::for_path(Path::new("notes.txt")), None);
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("real.rs");
        std::fs::write(&path, include_str!("main.rs")).unwrap();
        let editor = open(&path).unwrap();
        assert_eq!(editor.buffer().text(), include_str!("main.rs"));
    }

    #[cfg(unix)]
    #[test]
    fn accepts_non_utf8_file_names() {
        use std::os::unix::ffi::OsStringExt;
        let name = OsString::from_vec(b"file-\xff.txt".to_vec());
        assert_eq!(
            file_argument([name.clone()].into_iter())
                .unwrap()
                .into_os_string(),
            name
        );
    }

    #[test]
    fn opens_unicode_text_with_normalized_newlines_and_no_undo_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("a file 世界.txt");
        std::fs::write(&path, "Hello 世界\r\né 👩‍💻\r").unwrap();
        let editor = open(&path).unwrap();
        assert_eq!(editor.buffer().text().to_string(), "Hello 世界\né 👩‍💻\n");
        assert!(!editor.buffer().can_undo());
        assert_eq!(editor.buffer().revision(), 0);
        assert_eq!(editor.buffer().selection().head, 0);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Hello 世界\r\né 👩‍💻\r"
        );
    }
}
