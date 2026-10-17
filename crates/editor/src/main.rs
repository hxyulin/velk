mod document;
mod file;
mod plugins;
mod recovery;
mod session;
mod settings;
mod theme;
mod undo;
mod workspace;

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
            Self::Usage => write!(f, "expected at most one file path\nUsage: velk [file]"),
            Self::Open { path, source } => write!(f, "cannot open '{}': {source}", path.display()),
            Self::Application(error) => write!(f, "{error}"),
        }
    }
}

fn file_argument(mut arguments: impl Iterator<Item = OsString>) -> Result<Option<PathBuf>, Error> {
    let path = arguments.next();
    if arguments.next().is_some() {
        return Err(Error::Usage);
    }
    Ok(path.map(PathBuf::from))
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
    let argument = file_argument(std::env::args_os().skip(1))?;
    let settings = settings::Store::load();
    undo::configure(&settings.preferences, None);
    let plugins = plugins::start();
    let plugin_client = plugins.as_ref().map(velk_plugin_host::Runtime::client);
    if let Some(client) = &plugin_client
        && let Err(error) = client.set_settings(settings.preferences.plugins.clone())
    {
        tracing::warn!(%error, "cannot configure plugin bootstrap");
    }
    undo::set_client(plugin_client.clone());
    let mut path = argument
        .clone()
        .unwrap_or_else(|| PathBuf::from("Untitled 1"));
    let restored = std::env::var_os(session::SNAPSHOT_ENV)
        .map(|snapshot| session::Session::read(snapshot.into(), &path))
        .transpose()
        .map_err(|message| Error::Open {
            path: path.clone(),
            source: io::Error::other(message),
        })?;
    let (mut file, mut editor) = if let Some(session) = &restored {
        (
            session.file.clone(),
            if session.is_empty() {
                undo::editor("")
            } else {
                session.editor().map_err(|message| Error::Open {
                    path: path.clone(),
                    source: io::Error::other(message),
                })?
            },
        )
    } else {
        if argument.is_some() {
            let (file, editor) = open(&path)?;
            (Some(file), editor)
        } else {
            (None, undo::editor(""))
        }
    };
    if restored.is_some() {
        editor
            .set_language(Language::for_path(&path))
            .map_err(|error| Error::Open {
                path: path.clone(),
                source: io::Error::other(error.to_string()),
            })?;
    }
    let (recovery_store, recovered, mut recovery_error) =
        match recovery::Store::open(restored.is_none()) {
            Ok((store, recovered)) => {
                let warning = store.warning.clone();
                (Some(store), recovered, warning)
            }
            Err(error) => (None, None, Some(format!("Recovery unavailable: {error}"))),
        };
    let recovery_handle = recovery_store.as_ref().map(|store| store.handle.clone());
    let mut empty = restored.as_ref().is_some_and(|s| s.is_empty());
    let mut document_id = restored
        .as_ref()
        .map_or(1, |s| if s.is_empty() { 0 } else { s.id.max(1) });
    let mut next_document_id = restored
        .as_ref()
        .map_or(2, |s| s.next_id.max(document_id + 1));
    let mut recovered_order = Vec::new();
    let mut recovered_documents = Vec::new();
    let mut recovered_views = velk_editor_model::views::Snapshot::default();
    let mut recovered_workspace = workspace::Workspace::default();
    let mut recovered_notice = false;
    if let Some(mut recovered) = recovered {
        recovered_notice = true;
        next_document_id = recovered.next_id;
        recovered_order = recovered.documents.iter().map(|d| d.id).collect();
        if recovered.documents.is_empty() && argument.is_none() {
            empty = true;
            document_id = 0;
            path = PathBuf::new();
        }
        let chosen = if argument.is_none() {
            recovered
                .documents
                .iter()
                .position(|d| d.id == recovered.active)
        } else {
            recovered.documents.iter().position(|d| {
                d.file
                    .as_ref()
                    .zip(file.as_ref())
                    .is_some_and(|(a, b)| a.path() == b.path())
            })
        };
        if let Some(index) = chosen {
            let primary = recovered.documents.remove(index);
            document_id = primary.id;
            path = primary.path;
            file = primary.file;
            editor = primary.editor;
        } else if !empty {
            if recovered.documents.len() >= 256 {
                return Err(Error::Open {
                    path,
                    source: io::Error::other(
                        "recovered workspace has 256 documents; launch without a file and close a document first",
                    ),
                });
            }
            document_id = recovered.next_id;
            next_document_id = document_id + 1;
        }
        recovered_documents = recovered.documents;
        recovered_workspace = recovered.workspace;
        recovered_views = recovered.views;
    }
    let reload_buffers = restored
        .as_ref()
        .map(session::Session::extra_editors)
        .transpose()
        .map_err(|message| Error::Open {
            path: path.clone(),
            source: io::Error::other(message),
        })?
        .unwrap_or_default();
    next_document_id = next_document_id.max(
        reload_buffers
            .iter()
            .enumerate()
            .map(|(index, d)| {
                if d.id == 0 {
                    document_id + index as u64 + 2
                } else {
                    d.id + 1
                }
            })
            .max()
            .unwrap_or(document_id + 1),
    );
    if let Some(handle) = &recovery_handle {
        let mut documents = vec![recovery::PendingDocument {
            id: document_id,
            path: path.clone(),
            file: file.clone(),
            buffer: editor.buffer().snapshot(),
            scroll: editor.scroll_offset(),
        }];
        documents.extend(
            recovered_documents
                .iter()
                .map(|d| recovery::PendingDocument {
                    id: d.id,
                    path: d.path.clone(),
                    file: d.file.clone(),
                    buffer: d.editor.buffer().snapshot(),
                    scroll: d.editor.scroll_offset(),
                }),
        );
        documents.extend(reload_buffers.iter().enumerate().map(|(index, d)| {
            recovery::PendingDocument {
                id: if d.id == 0 {
                    document_id + index as u64 + 1
                } else {
                    d.id
                },
                path: d.path.clone(),
                file: d.file.clone(),
                buffer: d.editor.buffer().snapshot(),
                scroll: d.editor.scroll_offset(),
            }
        }));
        if empty {
            documents.clear();
        }
        if let Some(session) = &restored
            && !empty
        {
            let primary = documents.remove(0);
            documents.insert(session.active_index.min(documents.len()), primary);
        } else {
            documents.sort_by_key(|d| {
                recovered_order
                    .iter()
                    .position(|id| *id == d.id)
                    .unwrap_or(recovered_order.len())
            });
        }
        handle.update(recovery::Pending {
            views: restored.as_ref().map_or_else(
                || recovered_views.clone(),
                |session| session.ui.views.clone(),
            ),
            next_id: next_document_id,
            documents,
            active: document_id,
            workspace: restored
                .as_ref()
                .map_or_else(|| recovered_workspace.clone(), |s| s.ui.workspace.clone()),
        });
        if let Err(error) = handle.flush() {
            if restored.is_some() {
                return Err(Error::Open {
                    path: path.clone(),
                    source: io::Error::other(error),
                });
            }
            recovery_error = Some(format!("Recovery unavailable: {error}"));
        }
    } else if restored.is_some() {
        return Err(Error::Open {
            path: path.clone(),
            source: io::Error::other("replacement cannot establish durable recovery"),
        });
    }
    undo::finish_bootstrap();
    let size = restored
        .as_ref()
        .map_or([1280., 800.], |session| session.size);
    let title = format!("Velk — {}", path.display());
    let document: Rc<RefCell<Option<WeakEntity<document::Document>>>> = Rc::default();
    let created = document.clone();
    let closing = document.clone();
    let quitting = document.clone();
    let focus_restoring = document.clone();
    let mut frame_id = 0u64;
    let mut reload_frames = if std::env::var_os(session::READY_ENV).is_some() {
        2
    } else {
        0
    };
    let application = Application::new();
    #[cfg(any(target_os = "macos", target_os = "windows"))]
    let application = application.menu_bar(document::Document::menus());
    let result = application
        // Join plugin workers before native event-loop teardown.
        .exiting(move |_| drop(plugins))
        .theme(theme::dark())
        // Unmapped, RXUI's generic monospace is Courier New; match the editor.
        .map_font_family(
            rxui::FontFamily::Monospace,
            velk_editor_widget::MONOSPACE_FAMILY,
        )
        .window_created(move |_, cx| {
            if let Some(document) = created.borrow().as_ref().and_then(WeakEntity::upgrade) {
                document.read(cx).title();
            }
        })
        .prepare_graphics(move |_, cx| {
            if let Some(document) = focus_restoring
                .borrow()
                .as_ref()
                .and_then(WeakEntity::upgrade)
            {
                let action = document.read(cx).focus_restoration();
                if let Some(action) = action {
                    let _ = action.invoke(cx);
                }
            }
            Ok(())
        })
        .render_graphics(move |window, _, _| {
            frame_id += 1;
            tracing::debug!(frame_id, "frame recording");
            if reload_frames > 0 {
                reload_frames -= 1;
                if reload_frames == 0 {
                    session::acknowledge()
                        .map_err(|error| ApplicationError::Native(Box::new(error)))?;
                } else if let Some(native) = window.native_window() {
                    native.request_redraw();
                }
            }
            Ok(())
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
            let root = if empty {
                cx.new(|_| document::Document::empty(next_document_id))
            } else {
                let editor = cx.new(|_| editor);
                cx.new(|cx| document::Document::new_with_id(document_id, editor, file, path, cx))
            };
            root.update(cx, |this, _| this.configure_shell(settings));
            root.update(cx, |this, cx| {
                this.restore_recovered(
                    recovered_documents,
                    recovered_workspace,
                    recovered_order,
                    next_document_id,
                    cx,
                );
            });
            if let Some(client) = plugin_client.clone() {
                if let Some(session) = &restored {
                    root.update(cx, |this, cx| {
                        this.enable_selected_plugins(
                            client,
                            Some(session.ui.plugins.clone()),
                            session.plugin_states.clone(),
                            cx,
                        )
                    });
                } else {
                    root.update(cx, |this, cx| this.enable_plugins(client, cx));
                }
            }
            if let Some(session) = &restored {
                root.update(cx, |this, cx| {
                    this.restore_buffers(reload_buffers, session.active_index, cx)
                });
            }
            let views = restored.as_ref().map_or_else(
                || recovered_views.clone(),
                |session| session.ui.views.clone(),
            );
            root.update(cx, |this, cx| this.restore_editor_views(views, cx));
            root.update(cx, |this, cx| this.start_all_semantics(cx));
            root.update(cx, |this, cx| {
                if let Some(handle) = recovery_handle {
                    this.enable_recovery(handle, cx);
                }
                this.start_document_maintenance(cx);
                if recovered_notice {
                    this.recovery_notice(
                        "Recovered your previous workspace. Source files were not changed.".into(),
                    );
                }
                if let Some(error) = recovery_error {
                    this.recovery_notice(error);
                }
            });
            let options = if let Some(session) = &restored {
                WindowOptions::new()
                    .title(title)
                    .geometry(rxui::WindowGeometry {
                        inner_size: size,
                        outer_position: session.position,
                        maximized: session.maximized,
                    })
            } else {
                WindowOptions::new().title(title).size(size[0], size[1])
            };
            // Elsewhere the native title bar stays: it has no traffic lights to
            // share, and drawing window buttons is not worth it yet.
            #[cfg(target_os = "macos")]
            let options = options.custom_title_bar();
            if let Some(session) = restored {
                root.update(cx, |this, _| this.restore_ui(session.ui));
            }
            *document.borrow_mut() = Some(root.downgrade());
            let window = cx.open_window(options, root.clone())?;
            root.update(cx, |this, _| this.window = Some(window));
            Ok(())
        })
        .map_err(Error::Application);
    if let Some(store) = recovery_store {
        store.shutdown();
    }
    result
}

fn main() -> ExitCode {
    let _diagnostics = match velk_diagnostics::init() {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("velk: diagnostics initialization failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "application failed");
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
    fn accepts_optional_path_and_keeps_spaces_and_flag_like_names() {
        assert_eq!(file_argument(std::iter::empty()).unwrap(), None);
        assert!(matches!(
            file_argument(["one".into(), "two".into()].into_iter()),
            Err(Error::Usage)
        ));
        for path in ["folder/a file.txt", "世界.rs", "--showcase"] {
            assert_eq!(
                file_argument([path.into()].into_iter()).unwrap().unwrap(),
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
