use super::{
    Document,
    buffers::{BufferState, EditorView},
    palette,
};
use rxui::{Element, prelude::*};
use std::collections::BTreeMap;
use velk_editor_model::views::{Axis, Layout, Node};
use velk_editor_widget::Editor;

pub(super) struct SplitRight;
impl Command for SplitRight {}
pub(super) struct SplitDown;
impl Command for SplitDown {}
pub(super) struct CloseView;
impl Command for CloseView {}
pub(super) struct NextView;
impl Command for NextView {}
struct Pane {
    editor: Entity<Editor>,
    focus: FocusHandle,
}
#[derive(Default)]
pub(super) struct State {
    pub layout: Layout,
    panes: BTreeMap<u64, Pane>,
}
impl State {
    pub fn new(buffer: Option<&BufferState>, focus: FocusHandle) -> Self {
        let mut state = Self::default();
        if let Some(buffer) = buffer {
            let id = state.layout.show(buffer.id);
            state.panes.insert(
                id,
                Pane {
                    editor: buffer.editor.clone(),
                    focus,
                },
            );
        }
        state
    }
}
impl Document {
    pub(super) fn synchronize_document_views(
        &mut self,
        id: u64,
        source: &Entity<Editor>,
        cx: &mut Context<'_, Self>,
    ) {
        let Ok(buffer) = self.host_buffer(id) else {
            return;
        };
        let source_id = source.id();
        let annotations = source.read(cx).shared_annotations();
        let hover = source.read(cx).semantic_hover_enabled();
        let editors: Vec<_> = buffer
            .views
            .iter()
            .filter(|view| view.editor.id() != source_id)
            .map(|view| view.editor.clone())
            .collect();
        for editor in editors {
            editor.update(cx, |editor, cx| {
                if !editor.synchronize_document(annotations.clone(), hover) {
                    cx.unchanged();
                }
            });
        }
    }
    fn prune_editor_views(&mut self) {
        let shown: Vec<_> = self
            .views
            .panes
            .values()
            .map(|pane| pane.editor.id())
            .collect();
        for buffer in self
            .buffer
            .as_mut()
            .into_iter()
            .chain(self.inactive.values_mut())
        {
            let selected = buffer.editor.id();
            buffer
                .views
                .retain(|view| view.editor.id() == selected || shown.contains(&view.editor.id()));
        }
    }
    pub(super) fn show_active_document(&mut self, cx: &mut Context<'_, Self>) {
        let Some(buffer) = self.buffer.as_ref() else {
            return;
        };
        let document = buffer.id;
        let active = self.views.layout.show(document);
        let already = self
            .views
            .panes
            .get(&active)
            .filter(|pane| {
                pane.editor
                    .read(cx)
                    .buffer()
                    .document()
                    .same_document(&buffer.editor.read(cx).buffer().document())
            })
            .map(|pane| pane.editor.clone());
        let editor = if let Some(editor) = already {
            editor
        } else {
            let used: Vec<_> = self
                .views
                .panes
                .iter()
                .filter(|(id, _)| **id != active)
                .map(|(_, pane)| pane.editor.id())
                .collect();
            if !used.contains(&buffer.editor.id()) {
                buffer.editor.clone()
            } else {
                let created = buffer.editor.read(cx).new_view();
                let new = match created {
                    Ok(editor) => editor,
                    Err(error) => {
                        self.notice(Some(format!("Cannot create editor view: {error}")));
                        return;
                    }
                };
                let editor = cx.new(|_| new);
                let view = EditorView::new(document, editor.clone(), cx);
                self.buffer.views.push(view);
                editor
            }
        };
        let pane = self.views.panes.entry(active).or_insert_with(|| Pane {
            editor: editor.clone(),
            focus: FocusHandle::new(),
        });
        pane.editor = editor.clone();
        self.editor_focus = pane.focus.clone();
        self.buffer.editor = editor;
        self.prune_editor_views();
        self.sync_buffer_keybindings(cx);
    }
    fn activate_view(&mut self, id: u64, cx: &mut Context<'_, Self>) {
        if self.busy() || self.lifecycle.close_intent().is_some() {
            return;
        }
        let Some((_, document)) = self
            .views
            .layout
            .views()
            .into_iter()
            .find(|(view, _)| *view == id)
        else {
            return;
        };
        if self.views.layout.active == id && self.buffer.as_ref().is_some_and(|b| b.id == document)
        {
            return;
        }
        if let Some(buffer) = self.buffer.as_ref() {
            buffer.editor.update(cx, |editor, _| {
                editor.break_undo_group();
                editor.cancel_composition();
            });
        }
        self.cancel_key_chord();
        self.views.layout.active = id;
        self.select_buffer(document, cx);
        self.show_active_document(cx);
        self.title();
        self.restore_focus_pending = true;
        self.schedule_recovery(cx);
    }
    pub(super) fn split_editor(&mut self, axis: Axis, cx: &mut Context<'_, Self>) {
        if self.busy() || !self.has_buffers() || self.buffer.editor.read(cx).has_composition() {
            return;
        }
        let created = self.buffer.editor.read(cx).new_view();
        let new = match created {
            Ok(editor) => editor,
            Err(error) => {
                self.notice(Some(format!("Cannot split editor: {error}")));
                return;
            }
        };
        let Some(id) = self.views.layout.split(axis) else {
            self.notice(Some("A window supports at most 16 editor views.".into()));
            return;
        };
        let editor = cx.new(|_| new);
        let view = EditorView::new(self.buffer.id, editor.clone(), cx);
        self.buffer.views.push(view);
        let focus = FocusHandle::new();
        self.editor_focus = focus.clone();
        self.views.panes.insert(
            id,
            Pane {
                editor: editor.clone(),
                focus,
            },
        );
        self.buffer.editor = editor;
        self.sync_buffer_keybindings(cx);
        self.restore_focus_pending = true;
        self.schedule_recovery(cx);
    }
    pub(super) fn close_editor_view(&mut self, cx: &mut Context<'_, Self>) {
        if self.busy() || self.views.panes.len() < 2 {
            return;
        }
        let id = self.views.layout.active;
        self.views.layout.remove(id);
        self.views.panes.remove(&id);
        let active = self.views.layout.active;
        // Force activation even when the remaining pane shows the same document.
        self.views.layout.active = 0;
        self.activate_view(active, cx);
        self.prune_editor_views();
    }
    pub(super) fn remove_document_views(&mut self, document: u64) {
        let ids: Vec<_> = self
            .views
            .layout
            .views()
            .into_iter()
            .filter(|(_, id)| *id == document)
            .map(|(id, _)| id)
            .collect();
        for id in ids {
            self.views.layout.remove(id);
            self.views.panes.remove(&id);
        }
    }
    fn next_editor_view(&mut self, cx: &mut Context<'_, Self>) {
        let views = self.views.layout.views();
        if views.is_empty() {
            return;
        }
        let index = views
            .iter()
            .position(|(id, _)| *id == self.views.layout.active)
            .unwrap_or(0);
        self.activate_view(views[(index + 1) % views.len()].0, cx);
    }
    pub(super) fn editor_views(&self, cx: &mut ViewContext<'_, Self>) -> Element {
        if let Some(root) = &self.views.layout.root {
            self.editor_view_node(root, cx)
        } else {
            self.buffer
                .editor
                .clone()
                .into_element()
                .focus_handle(self.editor_focus.clone())
                .fill_width()
                .fill_height()
        }
    }
    fn editor_view_node(&self, node: &Node, cx: &mut ViewContext<'_, Self>) -> Element {
        match node {
            Node::View { id, document } => {
                let Some(pane) = self.views.panes.get(id) else {
                    return column().into_element();
                };
                let id = *id;
                let active = self.views.layout.active == id;
                let title = self
                    .host_buffer(*document)
                    .map(|buffer| {
                        buffer
                            .path
                            .file_name()
                            .unwrap_or(buffer.path.as_os_str())
                            .to_string_lossy()
                            .into_owned()
                    })
                    .unwrap_or_default();
                let editor = pane
                    .editor
                    .clone()
                    .into_element()
                    .focus_handle(pane.focus.clone())
                    .on_key_down_capture(cx.listener(move |this, event: &KeyInput, cx| {
                        this.activate_view(id, cx);
                        this.bound_key(event, velk_commands::keybindings::Scope::Editor, cx);
                    }))
                    .fill_width()
                    .fill_height()
                    .flex_shrink(1.)
                    .min_width(0.)
                    .min_height(0.);
                let header =
                    (self.views.panes.len() > 1).then(|| {
                        row()
                            .fill_width()
                            .height(25.)
                            .padding_x(8.)
                            .background(ThemeColor::Surface)
                            .border_bottom(1.)
                            .border_color(if active {
                                ThemeColor::Accent
                            } else {
                                ThemeColor::Divider
                            })
                            .child(button(title).variant(ButtonVariant::Quiet).on_click(
                                cx.listener(move |this, _, cx| {
                                    this.activate_view(id, cx);
                                    this.restore_focus_pending = true;
                                }),
                            ))
                            .child(row().flex_grow(1.))
                            .child(button("Close view").variant(ButtonVariant::Quiet).on_click(
                                cx.listener(move |this, _, cx| {
                                    this.activate_view(id, cx);
                                    this.close_editor_view(cx);
                                }),
                            ))
                    });
                column()
                    .key(format!("editor-view-{id}"))
                    .fill_width()
                    .fill_height()
                    .min_width(0.)
                    .min_height(0.)
                    .flex_shrink(1.)
                    .on_pointer_down_capture(cx.listener(move |this, _: &PointerInput, cx| {
                        this.activate_view(id, cx);
                    }))
                    .children(header)
                    .child(editor)
                    .into_element()
            }
            Node::Split {
                id,
                axis,
                fraction,
                first,
                second,
            } => {
                let first = self.editor_view_node(first, cx);
                let second = self.editor_view_node(second, cx);
                let id = *id;
                match axis {
                    Axis::Horizontal => split_row(first, second),
                    Axis::Vertical => split_column(first, second),
                }
                .position(SplitPosition::Fraction(*fraction))
                .min_first(100.)
                .min_second(100.)
                .divider_size(4.)
                .on_resize(cx.listener(move |this, event: &ResizeEvent, cx| {
                    if let SplitPosition::Fraction(fraction) = event.position {
                        this.views.layout.resize(id, fraction);
                        this.schedule_recovery(cx);
                    }
                }))
                .into_element()
                .key(format!("editor-split-{id}"))
            }
        }
    }
    pub(super) fn view_commands(
        &self,
        cx: &mut ViewContext<'_, Self>,
    ) -> (Vec<palette::Entry>, impl FnOnce(Element) -> Element + use<>) {
        let enabled = self.has_buffers() && !self.busy() && self.views.panes.len() < 16;
        let right = cx
            .command(SplitRight, |this, _, cx| {
                this.split_editor(Axis::Horizontal, cx)
            })
            .label("Split editor right")
            .enabled(enabled)
            .shortcut(Shortcut::primary("\\"));
        let down = cx
            .command(SplitDown, |this, _, cx| {
                this.split_editor(Axis::Vertical, cx)
            })
            .label("Split editor down")
            .enabled(enabled);
        let close = cx
            .command(CloseView, |this, _, cx| this.close_editor_view(cx))
            .label("Close editor view")
            .enabled(self.views.panes.len() > 1 && !self.busy());
        let next = cx
            .command(NextView, |this, _, cx| this.next_editor_view(cx))
            .label("Focus next editor view")
            .enabled(self.views.panes.len() > 1 && !self.busy());
        let entries = vec![
            palette::Entry::new(
                "velk.view.split-right",
                right.clone(),
                palette::primary("\\"),
            ),
            palette::Entry::new("velk.view.split-down", down.clone(), ""),
            palette::Entry::new("velk.view.close", close.clone(), ""),
            palette::Entry::new("velk.view.next", next.clone(), ""),
        ];
        (entries, move |element| {
            element
                .on_command(right)
                .on_command(down)
                .on_command(close)
                .on_command(next)
        })
    }
}

impl Document {
    pub(super) fn capture_editor_views(
        &self,
        cx: &impl rxui::ReadContext,
    ) -> velk_editor_model::views::Snapshot {
        velk_editor_model::views::Snapshot {
            layout: self.views.layout.clone(),
            states: self
                .views
                .panes
                .iter()
                .map(|(id, pane)| {
                    let editor = pane.editor.read(cx);
                    (
                        *id,
                        velk_editor_model::views::ViewState {
                            selection: editor.buffer().selection(),
                            scroll: editor.scroll_offset(),
                        },
                    )
                })
                .collect(),
        }
    }
    pub fn restore_editor_views(
        &mut self,
        snapshot: velk_editor_model::views::Snapshot,
        cx: &mut Context<'_, Self>,
    ) {
        if snapshot.layout.root.is_none() {
            return;
        }
        if let Err(error) = self.try_restore_editor_views(snapshot, cx) {
            self.prune_editor_views();
            self.notice(Some(format!("Editor views could not be restored: {error}")));
        }
    }
    fn try_restore_editor_views(
        &mut self,
        snapshot: velk_editor_model::views::Snapshot,
        cx: &mut Context<'_, Self>,
    ) -> Result<(), String> {
        snapshot.validate(&self.ids())?;
        let selected = self.buffer.as_ref().map(|buffer| buffer.id);
        let mut panes = BTreeMap::new();
        for (id, document) in snapshot.layout.views() {
            let buffer = self.host_buffer(document)?;
            let saved = &snapshot.states[&id];
            let created = buffer
                .editor
                .read(cx)
                .new_view()
                .map_err(|error| error.to_string());
            let mut editor = created?;
            let text = editor.buffer().text();
            if [saved.selection.anchor, saved.selection.head]
                .iter()
                .any(|position| {
                    *position > text.len_chars() || !editor.buffer().is_grapheme_boundary(*position)
                })
            {
                return Err("view selection is outside its recovered document".into());
            }
            editor.restore_view(saved.selection, saved.scroll)?;
            let editor = cx.new(|_| editor);
            let view = EditorView::new(document, editor.clone(), cx);
            self.with_buffer(document, cx, |this, _| {
                this.buffer.views.push(view);
                this.buffer.editor = editor.clone();
            });
            panes.insert(
                id,
                Pane {
                    editor,
                    focus: FocusHandle::new(),
                },
            );
        }
        self.views = State {
            layout: snapshot.layout,
            panes,
        };
        let active = self.views.layout.active;
        self.views.layout.active = 0;
        self.activate_view(active, cx);
        if let Some(selected) = selected
            && self
                .buffer
                .as_ref()
                .is_some_and(|buffer| buffer.id != selected)
        {
            self.select_buffer(selected, cx);
        }
        self.prune_editor_views();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Measure;
    impl rxui::TextMeasure for Measure {
        fn measure(
            &mut self,
            _: rxui::ElementId,
            request: rxui::TextRequest<'_>,
        ) -> Result<[f32; 2], rxui::UiError> {
            Ok([request.text.len() as f32 * 7., 18.])
        }
    }
    fn document() -> (Runtime, Entity<Document>, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file.txt");
        std::fs::write(&path, "alpha\nbeta\n").unwrap();
        let (file, editor) = crate::file::FileState::open(&path).unwrap();
        let mut runtime = Runtime::new();
        let document = runtime.update(|cx| {
            let editor = cx.new(|_| editor);
            cx.new(|cx| Document::new(editor, file, path, cx))
        });
        (runtime, document, directory)
    }
    #[test]
    fn split_views_share_edits_and_save_state_without_duplicating_documents() {
        let (mut runtime, document, _directory) = document();
        runtime
            .update(|cx| document.update(cx, |this, cx| this.split_editor(Axis::Horizontal, cx)));
        let (first, second) = runtime.update(|cx| {
            let doc = document.read(cx);
            let views = doc.views.layout.views();
            (
                doc.views.panes[&views[0].0].editor.clone(),
                doc.views.panes[&views[1].0].editor.clone(),
            )
        });
        runtime.update(|cx| {
            first.update(cx, |editor, _| {
                editor.select_range(6..10);
            });
            second.update(cx, |editor, _| {
                editor.select_range(0..0);
                editor.insert("new ");
            });
        });
        let mut ui = rxui::Ui::new(&mut runtime, document.clone()).unwrap();
        ui.prepare(&mut runtime, [1000., 700.], &mut Measure)
            .unwrap();
        runtime.update(|cx| {
            assert_eq!(
                first.read(cx).buffer().text().to_string(),
                "new alpha\nbeta\n"
            );
            assert_eq!(first.read(cx).buffer().selection().range(), 10..14);
            assert_eq!(second.read(cx).buffer().selection().head, 4);
            let doc = document.read(cx);
            assert_eq!(doc.ids().len(), 1);
            assert_eq!(doc.lifecycle_documents(cx).len(), 1);
            assert_eq!(doc.views.layout.views().len(), 2);
        });
        runtime.update(|cx| {
            first.update(cx, |editor, _| {
                editor.execute(velk_editor_model::EditCommand::Undo);
            })
        });
        runtime
            .update(|cx| assert_eq!(second.read(cx).buffer().text().to_string(), "alpha\nbeta\n"));
        runtime.update(|cx| {
            second.update(cx, |editor, _| {
                editor.insert("x");
            });
            let checkpoint = first.read(cx).buffer().checkpoint();
            first.update(cx, |editor, _| {
                assert!(editor.mark_saved(checkpoint));
            });
            assert!(!second.read(cx).buffer().is_dirty());
        });
    }
    #[test]
    fn opening_another_document_and_returning_ends_the_original_typing_group() {
        let (mut runtime, document, _directory) = document();
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.buffer
                    .editor
                    .update(cx, |editor, _| editor.insert("a"));
                this.add_buffer(None, Editor::new("other"), "other".into(), cx);
                this.select_buffer(1, cx);
                this.buffer.editor.update(cx, |editor, _| {
                    editor.insert("b");
                    editor.execute(velk_editor_model::EditCommand::Undo);
                });
                assert_eq!(
                    this.buffer.editor.read(cx).buffer().text().to_string(),
                    "aalpha\nbeta\n"
                );
            })
        });
    }
    #[test]
    fn tab_switches_target_only_the_active_pane_and_async_guards_include_view_identity() {
        let (mut runtime, document, _directory) = document();
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                let first = this.views.layout.active;
                let context = this.command_context(cx);
                let guard = this.edit_guard(cx);
                this.split_editor(Axis::Horizontal, cx);
                assert_eq!(this.command_context(cx), context);
                assert!(
                    !this.guard_valid(&guard, cx),
                    "another view at the same caret must reject the old result"
                );
                let split = this.views.layout.active;
                this.add_buffer(None, Editor::new("other"), "other".into(), cx);
                assert_eq!(this.views.layout.views(), [(first, 1), (split, 2)]);
                this.select_buffer(1, cx);
                let a = this.views.panes[&first].editor.id();
                let b = this.views.panes[&split].editor.id();
                assert_ne!(a, b, "each pane needs its own widget geometry");
                this.close_editor_view(cx);
                assert_eq!(this.views.layout.views(), [(first, 1)]);
                assert_eq!(
                    this.ids().len(),
                    2,
                    "closing a view does not close its document"
                );
                assert!(this.lifecycle.prompted_document().is_none());
            })
        });
    }
    #[test]
    fn view_layout_round_trips_with_selections_scroll_and_shared_history() {
        let (mut runtime, document, _directory) = document();
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.split_editor(Axis::Horizontal, cx);
                this.split_editor(Axis::Vertical, cx);
                let views = this.views.layout.views();
                for (index, (id, _)) in views.iter().enumerate() {
                    this.views.panes[id].editor.update(cx, |editor, _| {
                        editor
                            .restore_view(
                                velk_core::Selection {
                                    anchor: index,
                                    head: index + 1,
                                },
                                [index as f32 * 20., index as f32 * 100.],
                            )
                            .unwrap()
                    });
                }
                let saved = this.capture_editor_views(cx);
                let saved = serde_json::from_slice(&serde_json::to_vec(&saved).unwrap()).unwrap();
                this.close_editor_view(cx);
                this.restore_editor_views(saved, cx);
                assert_eq!(this.views.layout.views().len(), 3);
                let snapshot = this.capture_editor_views(cx);
                for (index, (id, _)) in views.iter().enumerate() {
                    assert_eq!(
                        snapshot.states[id].selection,
                        velk_core::Selection {
                            anchor: index,
                            head: index + 1
                        }
                    );
                    assert_eq!(
                        snapshot.states[id].scroll,
                        [index as f32 * 20., index as f32 * 100.]
                    );
                }
                let editors: Vec<_> = this
                    .views
                    .panes
                    .values()
                    .map(|pane| pane.editor.clone())
                    .collect();
                editors[0].update(cx, |editor, _| editor.insert("shared"));
                assert!(editors.iter().all(|editor| {
                    editor
                        .read(cx)
                        .buffer()
                        .text()
                        .to_string()
                        .contains("shared")
                }));
            })
        });
    }
    #[test]
    fn closing_a_document_removes_all_its_views_and_prompts_only_once() {
        let (mut runtime, document, _directory) = document();
        runtime.update(|cx| {
            document.update(cx, |this, cx| {
                this.split_editor(Axis::Horizontal, cx);
                this.buffer
                    .editor
                    .update(cx, |editor, _| editor.insert("dirty"));
                this.intent(super::super::Intent::Buffer, cx);
                assert_eq!(this.lifecycle.prompted_document(), Some(1));
                let action = this.lifecycle.respond(
                    1,
                    velk_core::lifecycle::CloseResponse::Discard,
                    &this.lifecycle_documents(cx),
                );
                this.close_action(action, cx);
                assert!(this.ids().is_empty());
                assert!(this.views.panes.is_empty());
                assert!(this.views.layout.root.is_none());
            })
        });
        let mut ui = rxui::Ui::new(&mut runtime, document).unwrap();
        ui.prepare(&mut runtime, [1000., 700.], &mut Measure)
            .unwrap();
        assert!(
            ui.elements()
                .any(|element| element.key == Some(&rxui::Key::from("empty-workspace")))
        );
    }
}
