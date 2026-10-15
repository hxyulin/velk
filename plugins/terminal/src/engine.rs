use ghostty_vt::{
    RenderState, Terminal as Vt, TerminalOptions, key,
    render::{CellIterator, RowIterator},
    screen::CellWide,
    style::{RgbColor, Underline},
    terminal::{Mode, ScrollViewport},
};
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    path::Path,
};
use velk_panels::grid::{Cell, Grid, Input, Style};
pub const BACKGROUND: [u8; 3] = [18, 24, 34];
pub const FOREGROUND: [u8; 3] = [210, 220, 232];
const MAX_INPUT: usize = 256 * 1024;
fn rgb(c: RgbColor) -> [u8; 3] {
    [c.r, c.g, c.b]
}
fn color(c: [u8; 3]) -> RgbColor {
    RgbColor {
        r: c[0],
        g: c[1],
        b: c[2],
    }
}
struct Pty {
    master: Box<dyn MasterPty + Send>,
    child: Box<dyn Child + Send + Sync>,
    reader: Box<dyn Read + Send>,
    writer: Box<dyn Write + Send>,
}
impl Drop for Pty {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use nix::{
                sys::signal::{Signal, killpg},
                unistd::{Pid, getpgrp},
            };
            if self.child.try_wait().ok().flatten().is_none() {
                if let Some(group) = self
                    .master
                    .process_group_leader()
                    .filter(|p| *p > 1 && *p != getpgrp().as_raw())
                {
                    let _ = killpg(Pid::from_raw(group), Signal::SIGKILL);
                }
                if let Some(pid) = self.child.process_id() {
                    let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
                }
            }
        }

        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}
pub struct Engine {
    term: Vt,
    render: RenderState,
    rows: RowIterator,
    cells: CellIterator,
    encoder: key::Encoder,
    event: key::Event,
    pty: Pty,
    input: VecDeque<u8>,
    cols: u16,
    height: u16,
    exited: bool,
    status: String,
}
impl Engine {
    pub fn new(root: &Path, cols: u16, rows: u16, shell: Option<&Path>) -> anyhow::Result<Self> {
        let _trace = velk_plugin_sdk::diagnostics::scope!("terminal.spawn");
        let mut term = Vt::new(TerminalOptions {
            cols,
            rows,
            max_scrollback: 10_000,
        })?;
        term.set_default_fg_color(Some(color(FOREGROUND)))?;
        term.set_default_bg_color(Some(color(BACKGROUND)))?;
        let render = RenderState::new()?;
        let row_iter = RowIterator::new()?;
        let cell_iter = CellIterator::new()?;
        let encoder = key::Encoder::new()?;
        let event = key::Event::new()?;
        let pair = native_pty_system().openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })?;
        #[cfg(unix)]
        {
            use nix::fcntl::{FcntlArg, OFlag, fcntl};
            let fd = pair
                .master
                .as_raw_fd()
                .ok_or_else(|| anyhow::anyhow!("PTY has no native descriptor"))?;
            let flags = OFlag::from_bits_truncate(fcntl(fd, FcntlArg::F_GETFL)?);
            fcntl(fd, FcntlArg::F_SETFL(flags | OFlag::O_NONBLOCK))?;
        }
        #[cfg(not(unix))]
        anyhow::bail!("terminal PTY cancellation currently requires Unix");
        let reader = pair.master.try_clone_reader()?;
        let writer = pair.master.take_writer()?;
        let shell = shell
            .map(Path::to_path_buf)
            .or_else(|| {
                std::env::var_os("VELK_TERMINAL_SHELL")
                    .or_else(|| std::env::var_os("SHELL"))
                    .map(Into::into)
            })
            .unwrap_or_else(|| "/bin/sh".into());
        let mut command = CommandBuilder::new(&shell);
        command.arg("-i");
        command.cwd(root);
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        command.env("TERM_PROGRAM", "Velk");
        let child = pair.slave.spawn_command(command)?;
        drop(pair.slave);
        let status = format!(
            "{} · PID {}",
            shell.file_name().unwrap_or_default().to_string_lossy(),
            child.process_id().unwrap_or_default()
        );
        Ok(Self {
            term,
            render,
            rows: row_iter,
            cells: cell_iter,
            encoder,
            event,
            pty: Pty {
                master: pair.master,
                child,
                reader,
                writer,
            },
            input: VecDeque::new(),
            cols,
            height: rows,
            exited: false,
            status,
        })
    }
    pub fn alive(&self) -> bool {
        !self.exited
    }
    #[cfg(unix)]
    pub fn interest(&self, registry: &mio::Registry, registered: bool) -> anyhow::Result<()> {
        let fd = self
            .pty
            .master
            .as_raw_fd()
            .ok_or_else(|| anyhow::anyhow!("PTY has no descriptor"))?;
        let mut source = mio::unix::SourceFd(&fd);
        if self.exited {
            if registered {
                registry.deregister(&mut source)?;
            }
            return Ok(());
        }
        let interest = if self.input.is_empty() {
            mio::Interest::READABLE
        } else {
            mio::Interest::READABLE | mio::Interest::WRITABLE
        };
        if registered {
            registry.reregister(&mut source, mio::Token(1), interest)?;
        } else {
            registry.register(&mut source, mio::Token(1), interest)?;
        }
        Ok(())
    }
    #[cfg(not(unix))]
    pub fn interest(&self, _: &mio::Registry, _: bool) -> anyhow::Result<()> {
        anyhow::bail!("terminal currently requires Unix")
    }
    fn enqueue(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.input.len() + bytes.len() <= MAX_INPUT,
            "terminal input queue full"
        );
        self.input.extend(bytes);
        Ok(())
    }
    pub fn input(&mut self, input: Input) -> anyhow::Result<()> {
        let _trace = velk_plugin_sdk::diagnostics::scope!("terminal.input");
        input.validate().map_err(anyhow::Error::msg)?;
        if self.exited {
            return Ok(());
        }
        match input {
            Input::Resize { cols, rows } => {
                if self.cols != cols || self.height != rows {
                    self.pty.master.resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })?;
                    self.term.resize(cols, rows, 0, 0)?;
                    self.cols = cols;
                    self.height = rows;
                }
            }
            Input::Scroll { lines } => self
                .term
                .scroll_viewport(ScrollViewport::Delta(lines as isize)),
            Input::Text { text, paste } => {
                self.term.scroll_viewport(ScrollViewport::Bottom);
                if paste {
                    let mut data = text.into_bytes();
                    let mut encoded = vec![0; data.len() + 16];
                    let length = ghostty_vt::paste::encode(
                        &mut data,
                        self.term.mode(Mode::BRACKETED_PASTE)?,
                        &mut encoded,
                    )?;
                    self.enqueue(&encoded[..length])?;
                } else {
                    self.enqueue(text.replace("\r\n", "\n").replace('\n', "\r").as_bytes())?;
                }
            }
            Input::Key {
                key,
                control,
                alt,
                shift,
            } => {
                self.term.scroll_viewport(ScrollViewport::Bottom);
                let (code, text) = key_code(&key);
                let mut mods = key::Mods::empty();
                if control {
                    mods |= key::Mods::CTRL;
                }
                if alt {
                    mods |= key::Mods::ALT;
                }
                if shift {
                    mods |= key::Mods::SHIFT;
                }
                self.event
                    .set_key(code)
                    .set_action(key::Action::Press)
                    .set_mods(mods)
                    .set_utf8(text.clone());
                if let Some(c) = text.as_ref().and_then(|s| s.chars().next()) {
                    self.event.set_unshifted_codepoint(c.to_ascii_lowercase());
                }
                let mut bytes = Vec::new();
                self.encoder
                    .set_options_from_terminal(&self.term)
                    .set_macos_option_as_alt(key::OptionAsAlt::True)
                    .encode_to_vec(&self.event, &mut bytes)?;
                self.enqueue(&bytes)?;
            }
        }
        Ok(())
    }
    pub fn tick(&mut self) -> anyhow::Result<bool> {
        let _trace = velk_plugin_sdk::diagnostics::scope!("terminal.io");
        if self.exited {
            return Ok(false);
        }
        let mut dirty = false;
        let mut buffer = [0u8; 16384];
        for _ in 0..16 {
            match self.pty.reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(n) => {
                    self.term.vt_write(&buffer[..n]);
                    dirty = true;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                // Unix PTYs report EIO after the slave closes. Reap below.
                Err(e) if e.raw_os_error() == Some(5) => break,
                Err(e) => return Err(e.into()),
            }
        }
        let events: Vec<_> = self.term.events().collect();
        for event in events {
            if let ghostty_vt::Event::PtyWrite(bytes) = event {
                self.enqueue(&bytes)?;
            }
        }
        if !self.input.is_empty() {
            match self.pty.writer.write(self.input.as_slices().0) {
                Ok(n) => {
                    self.input.drain(..n);
                }
                Err(e)
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) => {}
                Err(e) => return Err(e.into()),
            }
        }
        if let Some(status) = self.pty.child.try_wait()? {
            self.exited = true;
            self.input.clear();
            self.status = format!("Shell exited ({status}) · restart plugin for a new session");
            dirty = true;
        }
        Ok(dirty)
    }
    pub fn snapshot(&mut self) -> anyhow::Result<Grid> {
        let _trace = velk_plugin_sdk::diagnostics::scope!("terminal.frame");
        let snapshot = self.render.update(&self.term)?;
        let colors = snapshot.colors()?;
        let cursor = if snapshot.cursor_visible()? {
            snapshot.cursor_viewport()?.map(|p| [p.x, p.y])
        } else {
            None
        };
        let mut result = Grid {
            cols: self.cols,
            rows: self.height,
            background: rgb(colors.background),
            cells: Vec::new(),
            styles: Vec::new(),
            cursor,
            status: self.status.clone(),
        };
        let mut styles = HashMap::new();
        let mut rows = self.rows.update(&snapshot)?;
        let mut y = 0;
        while let Some(row) = rows.next() {
            let mut cells = self.cells.update(row)?;
            let mut x = 0;
            while let Some(cell) = cells.next() {
                let wide = cell.raw_cell()?.wide()?;
                if matches!(wide, CellWide::SpacerTail | CellWide::SpacerHead) {
                    x += 1;
                    continue;
                }
                let span = if wide == CellWide::Wide { 2 } else { 1 };
                let text: String = cell
                    .graphemes()?
                    .into_iter()
                    .filter(|c| !c.is_control())
                    .collect();
                let style = cell.style()?;
                let mut fg = rgb(cell.fg_color()?.unwrap_or(colors.foreground));
                let mut bg = rgb(cell.bg_color()?.unwrap_or(colors.background));
                if style.inverse {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if style.invisible {
                    fg = bg;
                }
                let flags = u8::from(style.bold)
                    | (u8::from(style.italic) << 1)
                    | (u8::from(style.underline != Underline::None) << 2)
                    | (u8::from(style.strikethrough) << 3)
                    | (u8::from(style.faint) << 4)
                    | (u8::from(style.overline) << 5);
                if !text.trim().is_empty() || bg != result.background || flags & 44 != 0 {
                    let value = Style {
                        foreground: fg,
                        background: bg,
                        flags,
                    };
                    let index = *styles.entry(value).or_insert_with(|| {
                        let index = result.styles.len() as u16;
                        result.styles.push(value);
                        index
                    });
                    result
                        .cells
                        .push(Cell(y, x, text.chars().take(32).collect(), span, index));
                }
                x += 1;
            }
            row.set_dirty(false)?;
            y += 1;
        }
        snapshot.set_dirty(ghostty_vt::render::Dirty::Clean)?;
        result.validate().map_err(anyhow::Error::msg)?;
        Ok(result)
    }
}
fn key_code(value: &str) -> (key::Key, Option<String>) {
    use key::Key as K;
    let code = match value {
        "Enter" => K::Enter,
        "Tab" => K::Tab,
        "Backspace" => K::Backspace,
        "Delete" => K::Delete,
        "Escape" => K::Escape,
        "ArrowUp" => K::ArrowUp,
        "ArrowDown" => K::ArrowDown,
        "ArrowLeft" => K::ArrowLeft,
        "ArrowRight" => K::ArrowRight,
        "Home" => K::Home,
        "End" => K::End,
        "PageUp" => K::PageUp,
        "PageDown" => K::PageDown,
        "F1" => K::F1,
        "F2" => K::F2,
        "F3" => K::F3,
        "F4" => K::F4,
        "F5" => K::F5,
        "F6" => K::F6,
        "F7" => K::F7,
        "F8" => K::F8,
        "F9" => K::F9,
        "F10" => K::F10,
        "F11" => K::F11,
        "F12" => K::F12,
        _ => {
            if value.chars().count() == 1 {
                let c = value.chars().next().unwrap().to_ascii_lowercase();
                let letters = [
                    K::A,
                    K::B,
                    K::C,
                    K::D,
                    K::E,
                    K::F,
                    K::G,
                    K::H,
                    K::I,
                    K::J,
                    K::K,
                    K::L,
                    K::M,
                    K::N,
                    K::O,
                    K::P,
                    K::Q,
                    K::R,
                    K::S,
                    K::T,
                    K::U,
                    K::V,
                    K::W,
                    K::X,
                    K::Y,
                    K::Z,
                ];
                let code = if c.is_ascii_lowercase() {
                    letters[(c as u8 - b'a') as usize]
                } else {
                    K::Unidentified
                };
                return (code, Some(value.into()));
            }
            K::Unidentified
        }
    };
    (code, None)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn text(grid: &Grid) -> String {
        let mut rows = vec![String::new(); usize::from(grid.rows)];
        let mut columns = vec![0; usize::from(grid.rows)];
        for Cell(y, x, text, span, _) in &grid.cells {
            let y = usize::from(*y);
            while columns[y] < *x {
                rows[y].push(' ');
                columns[y] += 1;
            }
            rows[y].push_str(if text.is_empty() { " " } else { text });
            columns[y] = x + u16::from(*span);
        }
        rows.join("\n")
    }
    fn wait(engine: &mut Engine, expected: &str) -> Grid {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            engine.tick().unwrap();
            let grid = engine.snapshot().unwrap();
            if text(&grid).contains(expected) {
                return grid;
            }
            assert!(
                Instant::now() < deadline,
                "missing {expected:?}: {}",
                text(&grid)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn command(engine: &mut Engine, command: &str) {
        engine
            .input(Input::Text {
                text: format!("{command}\n"),
                paste: false,
            })
            .unwrap();
    }
    #[test]
    fn real_pty_runs_in_workspace_resizes_and_reaps_on_drop() {
        let directory = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(directory.path()).unwrap();
        let mut engine = Engine::new(&root, 80, 12, Some(Path::new("/bin/sh"))).unwrap();
        let pid = engine.pty.child.process_id().unwrap();
        command(&mut engine, "printf '\\033[31mREADY_%s\\033[0m\\n' 314159");
        let grid = wait(&mut engine, "READY_314159");
        assert!(
            grid.styles
                .iter()
                .any(|s| s.foreground[0] > s.foreground[1])
        );
        command(&mut engine, "pwd");
        wait(&mut engine, root.to_str().unwrap());
        engine.input(Input::Resize { cols: 67, rows: 9 }).unwrap();
        command(&mut engine, "stty size");
        let grid = wait(&mut engine, "9 67");
        assert_eq!((grid.cols, grid.rows), (67, 9));
        command(&mut engine, "sleep 30");
        std::thread::sleep(Duration::from_millis(30));
        engine.tick().unwrap();
        engine
            .input(Input::Key {
                key: "c".into(),
                control: true,
                alt: false,
                shift: false,
            })
            .unwrap();
        command(&mut engine, "printf 'AFTER_%s\\n' INT");
        wait(&mut engine, "AFTER_INT");
        let start = Instant::now();
        drop(engine);
        assert!(start.elapsed() < Duration::from_secs(2));
        assert!(
            nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None).is_err(),
            "owned child was reaped"
        );
    }
    #[test]
    fn ghostty_replaces_cells_and_handles_unicode_and_alternate_screen() {
        let directory = tempfile::tempdir().unwrap();
        let mut engine = Engine::new(directory.path(), 80, 12, Some(Path::new("/bin/sh"))).unwrap();
        engine.term.reset();
        engine
            .term
            .vt_write("old\rNEW\x1b[2K\r世界 café\x1b[4;9H!".as_bytes());
        let grid = engine.snapshot().unwrap();
        assert!(text(&grid).contains("世界 café"));
        assert!(
            grid.cells
                .iter()
                .any(|Cell(_, _, t, w, _)| t == "世" && *w == 2)
        );
        assert!(
            grid.cells
                .iter()
                .any(|Cell(y, x, t, _, _)| *y == 3 && *x == 8 && t == "!")
        );
        engine.term.vt_write(b"\x1b[?1049hALT");
        assert!(!text(&engine.snapshot().unwrap()).contains("世界"));
        engine.term.vt_write(b"\x1b[?1049l");
        assert!(text(&engine.snapshot().unwrap()).contains("世界"));
    }
}
