//! Host renderer for bounded fixed-cell plugin surfaces. No PTY or VT dependency.
use super::Document;
use rxui::Bounds;
use rxui::astrelis::{
    FontFamily, FontSlant, PaintSession, PreparedText, Rect, TextBuffer, TextDraw, TextLayout,
    TextPosition, TextStyle, TextWrap,
};
use rxui::{CustomElement, CustomPrepare, Element, ElementInfo, UiError, prelude::*};
use std::{cell::RefCell, collections::HashMap, rc::Rc, sync::Arc};
use velk_panels::{
    Reply, Request,
    grid::{Cell, Grid, Input, Style},
};
use velk_plugin_host::{Client, HookHandle};
const SIZE: f32 = 14.;
const LINE: f32 = 20.;
use rxui::standard_commands::Paste;
#[derive(Default)]
struct Geometry {
    dimensions: [u16; 2],
    cell: f32,
    cursor: [u16; 2],
    preedit: String,
    wheel_remainder: f32,
}
pub(super) struct Connection {
    sender: async_channel::Sender<Input>,
    geometry: Rc<RefCell<Geometry>>,
    focus: FocusHandle,
    epoch: Rc<std::cell::Cell<u64>>,
    _task: Task,
}
impl Connection {
    pub fn new(
        client: Client,
        handle: HookHandle,
        panel: String,
        epoch: u64,
        cx: &mut Context<'_, Document>,
    ) -> Result<Rc<Self>, String> {
        let (sender, receiver) = async_channel::bounded(128);
        let epoch = Rc::new(std::cell::Cell::new(epoch));
        let current_epoch = epoch.clone();
        let task = cx
            .try_spawn(
                async move {
                    while let Ok(event) = receiver.recv().await {
                        let bytes = client
                            .call_hook(
                                handle.clone(),
                                velk_panels::encode(&Request::Input {
                                    panel: panel.clone(),
                                    events: vec![event],
                                })?,
                            )
                            .await?;
                        if !matches!(velk_panels::decode::<Reply>(&bytes)?, Reply::Ack) {
                            return Err("interactive panel did not acknowledge input".into());
                        }
                    }
                    Ok::<_, String>(())
                },
                move |this, result, _| {
                    if this.panels.epoch() != current_epoch.get() {
                        return;
                    }
                    match result {
                        Ok(Err(error)) => {
                            this.buffer.status = Some(format!("Panel input: {error}"))
                        }
                        Err(error) => this.buffer.status = Some(format!("Panel input: {error}")),
                        Ok(Ok(())) => {}
                    }
                },
            )
            .map_err(|e| e.to_string())?;
        Ok(Rc::new(Self {
            sender,
            geometry: Rc::new(RefCell::new(Geometry::default())),
            focus: FocusHandle::new(),
            epoch,
            _task: task,
        }))
    }
    pub fn set_epoch(&self, epoch: u64) {
        self.epoch.set(epoch);
    }
    fn send(&self, input: Input) -> Result<(), String> {
        input.validate()?;
        self.sender
            .try_send(input)
            .map_err(|_| "interactive panel input queue full or closed".into())
    }
    pub fn element(self: &Rc<Self>, grid: Grid, cx: &mut ViewContext<'_, Document>) -> Element {
        let keys = self.clone();
        let text = self.clone();
        let pointer = self.clone();
        let wheel = self.clone();
        let ime = self.clone();
        let candidate = self.geometry.clone();
        let paste_connection = self.clone();
        let paste = cx
            .command(Paste, move |this, _, cx| {
                match cx
                    .read_clipboard_text()
                    .map_err(|e| e.to_string())
                    .and_then(|text| paste_connection.send(Input::Text { text, paste: true }))
                {
                    Ok(()) => cx.unchanged(),
                    Err(e) => this.buffer.status = Some(e),
                }
            })
            .shortcut(Shortcut::primary("v"))
            .shortcut(Shortcut::primary("v").shift());
        let preedit = self.geometry.borrow().preedit.clone();
        let surface = rxui::custom(Surface {
            grid: Arc::new(grid.clone()),
            geometry: self.geometry.clone(),
            sender: self.sender.clone(),
            preedit,
        })
        .key("panel-grid-surface")
        .fill_width()
        .fill_height()
        .min_height(0.)
        .flex_shrink(1.)
        .clip()
        .text_input_client(true)
        .focus_handle(self.focus.clone())
        .accessibility_role(SemanticRole::MultilineTextInput)
        .accessibility_label("Terminal cell grid")
        .accessibility_value(grid.plain_text())
        .on_command(paste)
        .ime_cursor_area_with(move || {
            let g = candidate.borrow();
            Some(Bounds {
                x: 8. + f32::from(g.cursor[0]) * g.cell,
                y: 4. + f32::from(g.cursor[1]) * LINE,
                width: g.cell,
                height: LINE,
            })
        })
        .on_pointer_down(cx.listener(move |_, event: &PointerInput, cx| {
            event.focus();
            let _ = &pointer;
            cx.unchanged();
        }))
        .on_key_down(cx.listener(move |this, event: &KeyInput, cx| {
            let e = &event.event;
            if !e.pressed || e.modifiers.meta {
                cx.unchanged();
                return;
            }
            if let Some(input) = key_input(e) {
                event.prevent_default();
                event.stop_propagation();
                match keys.send(input) {
                    Ok(()) => cx.unchanged(),
                    Err(error) => this.buffer.status = Some(error),
                }
            } else {
                cx.unchanged();
            }
        }))
        .on_text_insert(cx.listener(move |this, event: &TextInsertInput, cx| {
            let had_preedit = !text.geometry.borrow().preedit.is_empty();
            text.geometry.borrow_mut().preedit.clear();
            event.stop_propagation();
            if let Err(error) = text.send(Input::Text {
                text: event.text.clone(),
                paste: event.source == TextInsertSource::Paste,
            }) {
                this.buffer.status = Some(error);
            } else if !had_preedit {
                cx.unchanged();
            }
        }))
        .on_ime(cx.listener(move |_, event: &ImeInput, _| {
            ime.geometry.borrow_mut().preedit = match &event.event {
                ImeEvent::Preedit { text, .. } => text.clone(),
                _ => String::new(),
            };
        }))
        .on_wheel(cx.listener(move |this, event: &WheelInput, cx| {
            event.prevent_default();
            event.stop_propagation();
            let lines = {
                let mut geometry = wheel.geometry.borrow_mut();
                geometry.wheel_remainder += event.delta[1];
                let lines = (geometry.wheel_remainder / LINE)
                    .trunc()
                    .clamp(-10000., 10000.) as i32;
                geometry.wheel_remainder -= lines as f32 * LINE;
                lines
            };
            if lines != 0
                && let Err(error) = wheel.send(Input::Scroll { lines })
            {
                this.buffer.status = Some(error);
                return;
            }
            cx.unchanged();
        }));
        column()
            .fill_width()
            .fill_height()
            .min_height(0.)
            .child(surface)
            .child(
                label(grid.status)
                    .font_size(11.)
                    .padding_x(8.)
                    .padding_y(2.)
                    .flex_shrink(0.),
            )
            .into_element()
    }
    pub fn focus(&self, cx: &mut Context<'_, Document>) -> Result<(), rxui::FocusError> {
        self.focus.focus(cx)
    }
}
#[derive(Clone)]
struct Surface {
    grid: Arc<Grid>,
    geometry: Rc<RefCell<Geometry>>,
    sender: async_channel::Sender<Input>,
    preedit: String,
}
impl PartialEq for Surface {
    fn eq(&self, other: &Self) -> bool {
        self.grid == other.grid
            && self.preedit == other.preedit
            && Rc::ptr_eq(&self.geometry, &other.geometry)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct Run {
    row: u16,
    col: u16,
    span: u16,
    text: String,
    style: Style,
}
struct Draw {
    run: Run,
    layout: Arc<TextLayout>,
    prepared: PreparedText,
}
#[derive(Default)]
struct State {
    draws: Vec<Draw>,
    cell: f32,
    scale: f32,
    origin: [f32; 2],
}
fn color(c: [u8; 3]) -> [f32; 4] {
    rxui::rgb8(c[0], c[1], c[2])
}
fn key_input(event: &rxui::KeyEvent) -> Option<Input> {
    if !event.pressed || event.modifiers.meta {
        return None;
    }
    let key = match &event.key {
        KeyboardKey::Character(text) => text.clone(),
        KeyboardKey::Other(name) => name
            .strip_prefix("Named(")
            .and_then(|n| n.strip_suffix(')'))
            .unwrap_or(name)
            .to_string(),
        other => format!("{other:?}"),
    };
    let modifiers = event.modifiers;
    if modifiers.control
        && ((modifiers.shift && matches!(key.to_ascii_lowercase().as_str(), "p" | "o" | "v"))
            || (modifiers.alt && key.eq_ignore_ascii_case("r"))
            || key == "`"
            || key == "Tab")
    {
        return None;
    }
    let special = matches!(
        key.as_str(),
        "Enter"
            | "Tab"
            | "Backspace"
            | "Delete"
            | "Escape"
            | "ArrowUp"
            | "ArrowDown"
            | "ArrowLeft"
            | "ArrowRight"
            | "Home"
            | "End"
            | "PageUp"
            | "PageDown"
            | "F1"
            | "F2"
            | "F3"
            | "F4"
            | "F5"
            | "F6"
            | "F7"
            | "F8"
            | "F9"
            | "F10"
            | "F11"
            | "F12"
    );
    let character = matches!(event.key, KeyboardKey::Character(_));
    if !special && !(character && (modifiers.control || modifiers.alt)) {
        return None;
    }
    Some(Input::Key {
        key,
        control: modifiers.control,
        alt: modifiers.alt,
        shift: modifiers.shift,
    })
}
fn text_style(flags: u8) -> TextStyle {
    TextStyle::new()
        .family(FontFamily::from(if cfg!(target_os = "macos") {
            "Menlo"
        } else if cfg!(target_os = "windows") {
            "Consolas"
        } else {
            "DejaVu Sans Mono"
        }))
        .font_size(SIZE)
        .line_height(LINE)
        .feature(*b"liga", 0)
        .weight(if flags & 1 != 0 { 700 } else { 400 })
        .slant(if flags & 2 != 0 {
            FontSlant::Italic
        } else {
            FontSlant::Normal
        })
}
fn layout(text: &str, flags: u8, cx: &mut CustomPrepare<'_>) -> Result<Arc<TextLayout>, UiError> {
    let mut b = TextBuffer::new();
    b.set_wrap(TextWrap::None);
    b.set_text(text, text_style(flags))?;
    Ok(b.layout(cx.fonts)?)
}
impl CustomElement for Surface {
    type State = State;
    fn prepare(
        &self,
        state: &mut State,
        element: &ElementInfo<'_>,
        cx: &mut CustomPrepare<'_>,
    ) -> Result<(), UiError> {
        if state.cell == 0. {
            state.cell = layout("M", 0, cx)?
                .caret(TextPosition::new(1))
                .map_or(8.4, |c| c.origin[0])
                .max(1.);
        }
        let dimensions = [
            ((element.bounds.width - 16.) / state.cell)
                .floor()
                .clamp(2., f32::from(velk_panels::grid::MAX_COLS)) as u16,
            ((element.bounds.height - 8.) / LINE)
                .floor()
                .clamp(2., f32::from(velk_panels::grid::MAX_ROWS)) as u16,
        ];
        let mut geometry = self.geometry.borrow_mut();
        geometry.cell = state.cell;
        geometry.cursor = self.grid.cursor.unwrap_or_default();
        if dimensions != geometry.dimensions
            && self
                .sender
                .try_send(Input::Resize {
                    cols: dimensions[0],
                    rows: dimensions[1],
                })
                .is_ok()
        {
            geometry.dimensions = dimensions;
        }
        drop(geometry);
        let mut runs: Vec<Run> = Vec::new();
        for Cell(row, col, text, span, style) in &self.grid.cells {
            let style = self.grid.styles[usize::from(*style)];
            let text = if text.is_empty() {
                " ".into()
            } else {
                text.clone()
            };
            if text.is_ascii()
                && *span == 1
                && let Some(last) = runs.last_mut()
                && last.row == *row
                && last.col + last.span == *col
                && last.style == style
                && last.text.is_ascii()
            {
                last.text.push_str(&text);
                last.span += 1;
                continue;
            }
            runs.push(Run {
                row: *row,
                col: *col,
                span: u16::from(*span),
                text,
                style,
            });
        }
        if !self.preedit.is_empty()
            && let Some([col, row]) = self.grid.cursor
        {
            runs.push(Run {
                row,
                col,
                span: self.preedit.chars().count().min(200) as u16,
                text: self.preedit.clone(),
                style: Style {
                    foreground: [225, 230, 240],
                    background: self.grid.background,
                    flags: 4,
                },
            });
        }
        let origin = [element.bounds.x + 8., element.bounds.y + 4.];
        let raster_changed = state.scale != cx.raster_scale || state.origin != origin;
        let mut previous: HashMap<_, _> = std::mem::take(&mut state.draws)
            .into_iter()
            .map(|draw| (draw.run.clone(), draw))
            .collect();
        for run in runs {
            let at = [
                origin[0] + f32::from(run.col) * state.cell,
                origin[1] + f32::from(run.row) * LINE,
            ];
            if let Some(mut draw) = previous.remove(&run) {
                if raster_changed {
                    draw.prepared = cx
                        .painter
                        .prepare_text(&draw.layout, cx.text_raster_options(at))?;
                }
                state.draws.push(draw);
            } else {
                let layout = layout(&run.text, run.style.flags, cx)?;
                let prepared = cx
                    .painter
                    .prepare_text(&layout, cx.text_raster_options(at))?;
                state.draws.push(Draw {
                    run,
                    layout,
                    prepared,
                });
            }
        }
        state.scale = cx.raster_scale;
        state.origin = origin;
        Ok(())
    }
    fn paint(
        &self,
        state: &State,
        element: &ElementInfo<'_>,
        paint: &mut PaintSession<'_, '_>,
    ) -> Result<(), UiError> {
        let b = element.bounds;
        paint.fill_rect(
            Rect::new(b.x, b.y, b.width, b.height),
            color(self.grid.background),
        )?;
        for draw in &state.draws {
            let r = &draw.run;
            let x = b.x + 8. + f32::from(r.col) * state.cell;
            let y = b.y + 4. + f32::from(r.row) * LINE;
            let width = f32::from(r.span) * state.cell;
            paint.fill_rect(Rect::new(x, y, width, LINE), color(r.style.background))?;
            let mut fg = color(r.style.foreground);
            if r.style.flags & 16 != 0 {
                fg[3] = 0.6;
            }
            paint.draw_text(&draw.prepared, TextDraw::new([x, y]).color(fg))?;
            for (flag, offset) in [(4, LINE - 2.), (8, LINE * 0.55), (32, 1.)] {
                if r.style.flags & flag != 0 {
                    paint.fill_rect(Rect::new(x, y + offset, width, 1.), fg)?;
                }
            }
        }
        if element.focused
            && let Some([x, y]) = self.grid.cursor
        {
            paint.fill_rect(
                Rect::new(
                    b.x + 8. + f32::from(x) * state.cell,
                    b.y + 4. + f32::from(y) * LINE,
                    state.cell,
                    LINE,
                ),
                [0.75, 0.83, 0.95, 0.35],
            )?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(key: KeyboardKey) -> rxui::KeyEvent {
        rxui::KeyEvent {
            key,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        }
    }
    #[test]
    fn text_and_ime_events_reach_text_client_while_navigation_and_control_keys_reach_plugin() {
        for key in [
            KeyboardKey::Character(" ".into()),
            KeyboardKey::Other("Named(Space)".into()),
            KeyboardKey::Other("Unidentified".into()),
            KeyboardKey::Character("世界".into()),
        ] {
            assert!(key_input(&event(key)).is_none());
        }
        assert!(
            matches!(key_input(&event(KeyboardKey::Other("Named(ArrowUp)".into()))),Some(Input::Key{key,..}) if key=="ArrowUp")
        );
        let mut control = event(KeyboardKey::Character("c".into()));
        control.modifiers.control = true;
        assert!(matches!(
            key_input(&control),
            Some(Input::Key { control: true, .. })
        ));
        control.modifiers.meta = true;
        assert!(key_input(&control).is_none());
        let mut palette = event(KeyboardKey::Character("p".into()));
        palette.modifiers.control = true;
        palette.modifiers.shift = true;
        assert!(key_input(&palette).is_none());
        assert!(
            matches!(key_input(&event(KeyboardKey::Tab)),Some(Input::Key{key,..}) if key=="Tab")
        );
    }
}
