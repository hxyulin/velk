//! Opt-in end-to-end walkthrough using the installed server and bundled plugins.
use super::*;
#[test]
#[ignore = "manual native-plugin GPU walkthrough"]
#[allow(unsafe_code)]
fn render_feature_walkthrough() {
    use rxui::{TestUi, astrelis::GraphicsContext};
    use std::time::{Duration, Instant};
    fn wait(
        capture: &mut TestUi<Document>,
        label: &str,
        condition: impl Fn(&Document, &rxui::AppContext<'_>) -> bool,
    ) {
        let deadline = Instant::now() + Duration::from_secs(15);
        let entity = capture.entity().clone();
        loop {
            let runtime = capture.runtime();
            runtime.poll_tasks();
            runtime.flush().unwrap();
            if runtime.update(|cx| condition(&entity.read(cx), cx)) {
                eprintln!("verified: {label}");
                return;
            }
            if Instant::now() >= deadline {
                runtime.update(|cx| {
                    let doc = entity.read(cx);
                    let buffer = doc.buffer.as_ref().map(|buffer| {
                        let editor = buffer.editor.read(cx);
                        (
                            buffer.id,
                            buffer.status.clone(),
                            editor.buffer().revision(),
                            editor.caret_position(),
                            editor.completion_active(),
                            buffer.semantic.as_ref().map(|state| {
                                (
                                    state.status.clone(),
                                    state
                                        .command
                                        .as_ref()
                                        .map(|pending| (pending.request, pending.cancelled)),
                                )
                            }),
                        )
                    });
                    eprintln!("Buffer at timeout: {buffer:?}");
                    panic!(
                        "Timed out: {label}; notice={:?}; semantic={:?}; plugins={:?}",
                        doc.window_status,
                        doc.buffer
                            .as_ref()
                            .and_then(|b| b.semantic.as_ref().map(|s| &s.status)),
                        doc.plugins.as_ref().map(|p| &p.status)
                    );
                });
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn wait_tree_text(capture: &mut TestUi<Document>, text: &str, present: bool) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            capture.runtime().poll_tasks();
            capture.runtime().flush().unwrap();
            capture.prepare().unwrap();
            let found = capture.ui().elements().any(|element| {
                element.kind == rxui::ElementType::Button
                    && element.bounds.x < 324.
                    && element.text.is_some_and(|label| label.contains(text))
            });
            if found == present {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "File tree did not reconcile {text}"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
    fn invoke(capture: &mut TestUi<Document>, command: &str, input: Option<&str>) {
        let entity = capture.entity().clone();
        capture.runtime().update(|cx| {
            entity.update(cx, |doc, cx| {
                let invocation = doc
                    .plugins
                    .as_ref()
                    .unwrap()
                    .loaded
                    .iter()
                    .find_map(|p| {
                        p.metadata
                            .commands
                            .iter()
                            .find(|c| c.id == command)
                            .map(|c| crate::document::plugin_commands::Invoke {
                                plugin: p.id.clone(),
                                generation: p.generation,
                                command: c.clone(),
                            })
                    })
                    .expect("declared command");
                doc.invoke_plugin_command_with_input(invocation, input.map(str::to_owned), cx);
            })
        });
    }
    fn shot(capture: &mut TestUi<Document>, output: &std::path::Path, name: &str) {
        let mut pixels = Vec::new();
        for _ in 0..4 {
            pixels = capture.render().unwrap();
            std::thread::sleep(Duration::from_millis(30));
            capture.runtime().poll_tasks();
            capture.runtime().flush().unwrap();
        }
        std::fs::write(output.join(format!("{name}.rgba")), pixels).unwrap();
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let temporary = tempfile::Builder::new()
        .prefix("feature-walkthrough-")
        .tempdir_in(root.join("target"))
        .unwrap();
    let project = temporary.path().canonicalize().unwrap();
    std::fs::create_dir(project.join("src")).unwrap();
    std::fs::write(project.join("Cargo.toml"), "[package]\nname = \"velk-feature-walkthrough\"\nversion = \"0.1.0\"\nedition = \"2024\"\n[workspace]\n").unwrap();
    let path = project.join("src/main.rs");
    let text = "fn greeting(name: &str) -> String {\n    format!(\"Hello, {name}!\")\n}\n\nfn main() {\n    let name = \"Velk\";\n    gre\n}\n";
    std::fs::write(&path, text).unwrap();
    let output = root.join("target/feature-demo/captures");
    std::fs::create_dir_all(&output).unwrap();
    // SAFETY: these are the locally built bundled plugins.
    let plugins =
        unsafe { velk_plugin_host::Runtime::start(root.join("target/release/plugins")) }.unwrap();
    let graphics = pollster::block_on(GraphicsContext::headless()).unwrap();
    let mut capture = TestUi::new(&graphics, [1280, 800], Document::empty(1)).unwrap();
    capture.painter_mut().fonts_mut().load_system_fonts();
    capture.painter_mut().map_family(
        rxui::FontFamily::Monospace,
        velk_editor_widget::MONOSPACE_FAMILY,
    );
    let entity = capture.entity().clone();
    {
        let (runtime, ui, _) = capture.parts();
        ui.set_theme(crate::theme::dark()).unwrap();
        runtime
            .configure_tasks(
                Arc::new(rxui::ThreadPoolExecutor::new(2, 2).unwrap()),
                || {},
            )
            .unwrap();
        runtime.update(|cx| {
            entity.update(cx, |doc, cx| {
                doc.shell.store.preferences = crate::settings::Preferences::default();
                doc.shell.store.preferences.set_layout("rail");
                doc.workspace.set_root(project.clone());
                let (file, mut editor) = crate::file::FileState::open(&path).unwrap();
                editor
                    .set_language(Some(velk_editor_widget::Language::Rust))
                    .unwrap();
                doc.add_buffer(file, editor, path.clone(), cx);
                doc.enable_selected_plugins(
                    plugins.client(),
                    Some(vec![
                        "velk.tree-sitter".into(),
                        "velk.lsp".into(),
                        "velk.completion".into(),
                        "velk.completion.words".into(),
                        "velk.language-server.rust-analyzer".into(),
                        "velk.files.backend".into(),
                        "velk.files".into(),
                    ]),
                    vec![],
                    cx,
                );
            })
        });
    }
    capture
        .runtime()
        .update(|cx| entity.update(cx, |doc, cx| doc.start_all_semantics(cx)));
    wait(&mut capture, "rust-analyzer ready", |doc, _| {
        doc.plugins.as_ref().is_some_and(|p| p.task.is_none())
            && doc
                .buffer
                .semantic
                .as_ref()
                .is_some_and(|s| matches!(s.status, velk_semantic::Status::Ready(_)))
    });
    wait(&mut capture, "Files panel available", |doc, _| {
        !doc.panel_titles(velk_panels::Slot::Left).is_empty()
    });
    capture.runtime().update(|cx| {
        entity.update(cx, |doc, cx| {
            doc.show_plugin_panel("velk.files", "files", cx)
        })
    });
    let caret = text.find("    gre\n").unwrap() + "    gre".len();
    capture.runtime().update(|cx| {
        entity.update(cx, |doc, cx| {
            doc.buffer
                .editor
                .update(cx, |editor, _| editor.select_range(caret..caret));
        })
    });
    capture.runtime().flush().unwrap();
    for _ in 0..3 {
        capture.render().unwrap();
        std::thread::sleep(Duration::from_millis(30));
        capture.runtime().poll_tasks();
        capture.runtime().flush().unwrap();
    }
    capture.prepare().unwrap();
    let directory_button = capture
        .ui()
        .elements()
        .find(|e| {
            e.kind == rxui::ElementType::Button
                && e.bounds.x < 324.
                && e.text.is_some_and(|text| text.contains("src"))
        })
        .map(|e| e.bounds);
    if let Some(bounds) = directory_button {
        let position = [
            bounds.x + bounds.width * 0.5,
            bounds.y + bounds.height * 0.5,
        ];
        capture
            .pointer(rxui::PointerEvent::Down {
                position,
                button: rxui::PointerButton::Primary,
                modifiers: Default::default(),
            })
            .unwrap();
        capture
            .pointer(rxui::PointerEvent::Up {
                position,
                button: rxui::PointerButton::Primary,
                modifiers: Default::default(),
            })
            .unwrap();
        for _ in 0..10 {
            capture.runtime().poll_tasks();
            capture.runtime().flush().unwrap();
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    let surface = capture.element("editor-surface").unwrap().unwrap().id;
    {
        let (_, ui, _) = capture.parts();
        ui.focus(surface);
    }
    capture
        .key(rxui::KeyEvent {
            key: rxui::KeyboardKey::Space,
            pressed: true,
            repeat: false,
            modifiers: rxui::Modifiers {
                control: true,
                ..Default::default()
            },
        })
        .unwrap();
    capture.runtime().flush().unwrap();
    for attempt in 0..16 {
        wait(&mut capture, "completion request finished", |doc, _| {
            doc.buffer
                .semantic
                .as_ref()
                .is_some_and(|s| s.command.is_none())
        });
        if capture.runtime().update(|cx| {
            entity
                .read(cx)
                .collection
                .as_ref()
                .is_some_and(|s| s.has_results_from("velk.lsp"))
        }) {
            break;
        }
        for _ in 0..50 {
            capture.runtime().poll_tasks();
            capture.runtime().flush().unwrap();
            std::thread::sleep(Duration::from_millis(10));
        }
        eprintln!("retry completion while rust-analyzer indexes: {attempt}");
        invoke(&mut capture, "velk.completion.complete", None);
    }
    wait(&mut capture, "real completion response", |doc, cx| {
        doc.buffer.editor.read(cx).completion_active()
    });
    capture
        .text_input(rxui::TextInputEvent::Insert("e".into()))
        .unwrap();
    wait(&mut capture, "typed prefix refresh", |doc, cx| {
        let editor = doc.buffer.editor.read(cx);
        editor.completion_active()
            && editor.completion_refresh().is_none()
            && editor.buffer().text().to_string().contains("    gree\n")
            && doc
                .buffer
                .semantic
                .as_ref()
                .is_some_and(|s| s.command.is_none())
    });
    wait(&mut capture, "both completion sources merged", |doc, _| {
        doc.collection.as_ref().is_some_and(|state| {
            state.settled()
                && state.has_results_from("velk.lsp")
                && state.has_results_from("velk.completion.words")
        })
    });
    let lsp_selected = |capture: &mut TestUi<Document>| {
        capture.runtime().update(|cx| {
        matches!(entity.read(cx).buffer.editor.read(cx).completion_choice().map(|choice| &choice.action),
            Some(velk_commands::ChoiceAction::Contributed { plugin, .. }) if plugin == "velk.lsp")
    })
    };
    for _ in 0..2 {
        if lsp_selected(&mut capture) {
            break;
        }
        capture
            .key(rxui::KeyEvent {
                key: rxui::KeyboardKey::ArrowDown,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            })
            .unwrap();
    }
    assert!(lsp_selected(&mut capture));
    shot(&mut capture, &output, "completion-popup");
    assert!(
        capture.element("editor-completions").unwrap().is_some(),
        "rust-analyzer returned no visible suggestions"
    );
    capture
        .key(rxui::KeyEvent {
            key: rxui::KeyboardKey::Enter,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        })
        .unwrap();
    wait(&mut capture, "completion accepted", |doc, cx| {
        doc.workspace_edit.is_none()
            && !doc.buffer.editor.read(cx).completion_active()
            && doc
                .buffer
                .editor
                .read(cx)
                .buffer()
                .text()
                .to_string()
                .contains("    greeting")
    });
    shot(&mut capture, &output, "completion-accepted");
    let id = capture.read(|doc| doc.buffer.id);
    capture.runtime().update(|cx| {
        entity.update(cx, |doc, cx| {
            doc.split_editor(velk_editor_model::views::Axis::Horizontal, cx)
        })
    });
    invoke(&mut capture, "velk.files.rename", Some("src/welcome.rs"));
    let renamed = project.join("src/welcome.rs");
    wait(&mut capture, "rename with split views", |doc, _| {
        doc.command_task.is_none() && doc.file_operation.is_none() && doc.buffer.path == renamed
    });
    capture.runtime().update(|cx| {
        let doc = entity.read(cx);
        assert_eq!(doc.buffer.id, id);
        assert_eq!(doc.buffer.views.len(), 2);
        assert!(doc.buffer.editor.read(cx).buffer().is_dirty());
        assert_eq!(doc.documents.find_path(&renamed), Some(id));
        assert_eq!(doc.documents.find_path(&path), None);
        doc.documents.validate_layout(&doc.views.layout).unwrap();
    });
    assert!(!path.exists());
    assert_eq!(std::fs::read_to_string(&renamed).unwrap(), text);
    std::thread::sleep(Duration::from_millis(250));
    capture.runtime().poll_tasks();
    capture.runtime().flush().unwrap();
    wait_tree_text(&mut capture, "welcome.rs", true);
    wait_tree_text(&mut capture, "main.rs", false);
    shot(&mut capture, &output, "renamed-shared-views");
    capture
        .runtime()
        .update(|cx| entity.update(cx, |doc, cx| doc.save(None, cx)));
    wait(&mut capture, "saved renamed document", |doc, cx| {
        !doc.any_saving() && !doc.buffer.editor.read(cx).buffer().is_dirty()
    });
    assert!(
        std::fs::read_to_string(&renamed)
            .unwrap()
            .contains("    greeting")
    );
    capture.runtime().update(|cx| {
        entity.update(cx, |doc, cx| {
            doc.buffer.editor.update(cx, |editor, _| {
                editor.execute(velk_editor_model::EditCommand::Undo);
            });
            assert!(doc.buffer.editor.read(cx).buffer().is_dirty());
            doc.buffer.editor.update(cx, |editor, _| {
                editor.execute(velk_editor_model::EditCommand::Redo);
            });
            assert!(!doc.buffer.editor.read(cx).buffer().is_dirty());
        })
    });
    invoke(&mut capture, "velk.files.create-directory", Some("notes"));
    wait(&mut capture, "created directory", |doc, _| {
        project.join("notes").is_dir() && doc.command_task.is_none() && doc.file_operation.is_none()
    });
    invoke(
        &mut capture,
        "velk.files.create-file",
        Some("notes/scratch.txt"),
    );
    wait(&mut capture, "created and opened scratch file", |doc, _| {
        doc.command_task.is_none()
            && doc.file_operation.is_none()
            && doc.opening.is_none()
            && doc.buffer.path == project.join("notes/scratch.txt")
    });
    let scratch_id = capture.read(|doc| doc.buffer.id);
    invoke(
        &mut capture,
        "velk.files.delete",
        Some("delete notes/scratch.txt"),
    );
    wait(&mut capture, "open-file deletion rejected", |doc, _| {
        doc.command_task.is_none()
            && doc.file_operation.is_none()
            && doc
                .buffer
                .status
                .as_deref()
                .is_some_and(|s| s.contains("close the affected documents"))
    });
    assert!(project.join("notes/scratch.txt").exists());
    shot(&mut capture, &output, "delete-open-file-guard");
    capture.runtime().update(|cx| {
        entity.update(cx, |doc, cx| {
            doc.host_request(
                velk_plugin_contracts::host::Request::CloseBuffer { id: scratch_id },
                None,
                cx,
            )
            .unwrap()
        })
    });
    invoke(
        &mut capture,
        "velk.files.delete",
        Some("delete notes/scratch.txt"),
    );
    wait(&mut capture, "closed-file deletion", |doc, _| {
        doc.command_task.is_none()
            && doc.file_operation.is_none()
            && !project.join("notes/scratch.txt").exists()
    });
    invoke(&mut capture, "velk.files.delete", Some("delete notes"));
    wait(&mut capture, "empty-directory deletion", |doc, _| {
        doc.command_task.is_none()
            && doc.file_operation.is_none()
            && !project.join("notes").exists()
    });
    capture.runtime().update(|cx| {
        entity.update(cx, |doc, cx| {
            doc.host_request(
                velk_plugin_contracts::host::Request::CloseBuffer { id },
                None,
                cx,
            )
            .unwrap()
        })
    });
    wait(&mut capture, "last buffer closed", |doc, _| {
        !doc.has_buffers()
    });
    capture.runtime().update(|cx| {
        let doc = entity.read(cx);
        assert!(doc.documents.is_empty());
        doc.documents.validate_layout(&doc.views.layout).unwrap();
    });
    wait_tree_text(&mut capture, "notes", false);
    shot(&mut capture, &output, "empty-workspace");
    capture.runtime().update(|cx| {
        entity.update(cx, |doc, cx| {
            doc.plugin_operation(crate::document::plugins::Operation::UnloadAll, cx);
        })
    });
    wait(&mut capture, "all plugins unloaded", |doc, _| {
        doc.plugins
            .as_ref()
            .is_some_and(|p| p.task.is_none() && p.loaded.is_empty())
    });
    capture.runtime().update(|cx| {
        entity.update(cx, |doc, cx| {
            doc.enable_selected_plugins(
                plugins.client(),
                Some(vec![
                    "velk.completion".into(),
                    "velk.completion.words".into(),
                ]),
                vec![],
                cx,
            );
        })
    });
    wait(&mut capture, "completion without LSP loaded", |doc, _| {
        doc.plugins
            .as_ref()
            .is_some_and(|p| p.task.is_none() && p.loaded.len() == 2)
    });
    capture.runtime().update(|cx| {
        entity.update(cx, |doc, cx| {
            doc.host_request(
                velk_plugin_contracts::host::Request::NewBuffer {
                    text: "print process\npr".into(),
                },
                None,
                cx,
            )
            .unwrap();
            doc.buffer
                .editor
                .update(cx, |editor, _| editor.select_range(16..16));
        })
    });
    capture.prepare().unwrap();
    let surface = capture.element("editor-surface").unwrap().unwrap().id;
    {
        let (_, ui, _) = capture.parts();
        ui.focus(surface);
    }
    invoke(&mut capture, "velk.completion.complete", None);
    wait(
        &mut capture,
        "word completion without semantic service",
        |doc, cx| {
            doc.buffer.semantic.is_none()
                && doc
                    .collection
                    .as_ref()
                    .is_some_and(|s| s.has_results_from("velk.completion.words"))
                && doc.buffer.editor.read(cx).completion_refresh().is_none()
        },
    );
    shot(&mut capture, &output, "completion-without-lsp");
    capture
        .key(rxui::KeyEvent {
            key: rxui::KeyboardKey::Enter,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        })
        .unwrap();
    wait(&mut capture, "untitled completion accepted", |doc, cx| {
        doc.buffer.editor.read(cx).buffer().text() == "print process\nprint"
    });
    capture.runtime().update(|cx| {
        entity.update(cx, |doc, cx| {
            doc.buffer.editor.update(cx, |editor, _| {
                editor.execute(velk_editor_model::EditCommand::Undo);
            });
            assert_eq!(
                doc.buffer.editor.read(cx).buffer().text().to_string(),
                "print process\npr"
            );
        })
    });
    std::fs::write(output.join("verified.txt"), "Real rust-analyzer; real bundled native plugins; Ctrl+Space completion popup; Enter acceptance; two shared views; rename preserves ID/unsaved text/disk baseline; save at new path; undo/redo saved checkpoint; create directory/file; reject deleting open file; delete closed file/empty directory; close final buffer and validate empty layout; unload LSP; complete words in untitled buffer; undo completion.\n").unwrap();
    drop(capture);
    drop(plugins);
}
