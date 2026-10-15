mod document;
mod file;
mod plugins;
mod theme;

use rxui::{WeakEntity, prelude::*};
use std::{
    cell::RefCell,
    ffi::OsString,
    fmt, io,
    path::{Path, PathBuf},
    process::ExitCode,
    rc::Rc,
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

fn open(path: &Path) -> Result<(file::FileState, Editor), Error> {
    (|| {
        let (file, mut editor) = file::FileState::open(path)?;
        editor
            .set_language(Language::for_path(path))
            .map_err(io::Error::other)?;
        Ok((file, editor))
    })()
    .map_err(|source| Error::Open {
        path: path.to_path_buf(),
        source,
    })
}

fn run() -> Result<(), Error> {
    let path = file_argument(std::env::args_os().skip(1))?;
    let (file, editor) = open(&path)?;
    let title = format!("Velk — {}", path.display());
    let plugins = plugins::start();
    let plugin_client = plugins.as_ref().map(velk_plugin_host::Runtime::client);
    let document: Rc<RefCell<Option<WeakEntity<document::Document>>>> = Rc::default();
    let created = document.clone();
    let closing = document.clone();
    let quitting = document.clone();
    Application::new()
        .theme(theme::dark())
        .window_created(move |_, cx| {
            if let Some(document) = created.borrow().as_ref().and_then(WeakEntity::upgrade) {
                document.read(cx).title();
            }
        })
        .close_requested(move |_, cx| {
            if let Some(document) = closing.borrow().as_ref().and_then(WeakEntity::upgrade) {
                document.update(cx, |this, cx| this.intent(document::Intent::Close, cx));
                CloseResponse::KeepOpen
            } else {
                CloseResponse::Close
            }
        })
        .quit_requested(move |cx| {
            if let Some(document) = quitting.borrow().as_ref().and_then(WeakEntity::upgrade) {
                document.update(cx, |this, cx| this.intent(document::Intent::Quit, cx));
                CloseResponse::KeepOpen
            } else {
                CloseResponse::Close
            }
        })
        .run(move |cx| {
            let editor = cx.new(|_| editor);
            let rust = Language::for_path(&path) == Some(Language::Rust);
            let root = cx.new(|cx| document::Document::new(editor, file, path, cx));
            if let Some(client) = plugin_client {
                root.update(cx, |this, cx| this.enable_plugins(client, cx));
            }
            if rust {
                root.update(cx, |this, cx| {
                    match velk_semantic::Service::start(
                        velk_semantic::lsp::LspBackend::rust_analyzer(),
                    ) {
                        Ok(service) => this.enable_semantics(service, cx),
                        Err(error) => eprintln!("velk: cannot start semantic service: {error}"),
                    }
                });
            }
            *document.borrow_mut() = Some(root.downgrade());
            let window = cx.open_window(
                WindowOptions::new().title(title).size(1280., 800.),
                root.clone(),
            )?;
            root.update(cx, |this, _| this.window = Some(window));
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
        let (_, editor) = open(&path).unwrap();
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
        let (_, editor) = open(&path).unwrap();
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
