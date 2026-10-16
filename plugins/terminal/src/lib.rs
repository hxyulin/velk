//! PTY, shell lifetime and Ghostty state stay inside the plugin worker.
mod engine;
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender, TrySendError},
    },
    time::{Duration, Instant},
};
use velk_panels::{
    Content, Context, Panel, Reply, Request, Slot, Snapshot,
    grid::{Grid, Input},
};
use velk_plugin_sdk::{Plugin, Registrar, hooks};
struct Published {
    snapshot: Snapshot,
}
enum Work {
    Context(Context),
    Input(Vec<Input>),
}
#[derive(Clone)]
struct Configuration {
    shell: String,
    scrollback_bytes: usize,
}
impl Default for Configuration {
    fn default() -> Self {
        Self {
            shell: String::new(),
            scrollback_bytes: 10_000,
        }
    }
}
pub struct Terminal {
    configuration: Arc<Mutex<Configuration>>,
    shared: Arc<Mutex<Published>>,
    sender: Option<SyncSender<Work>>,
    worker: Option<std::thread::JoinHandle<()>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    wake: Option<Arc<mio::Waker>>,
}
fn snapshot(grid: Grid, revision: u64) -> Snapshot {
    Snapshot {
        watch: false,
        stream: true,
        effects: vec![],
        panels: vec![Panel {
            id: "terminal".into(),
            title: "Terminal".into(),
            slot: Slot::Bottom,
            revision,
            content: Content::Grid(grid),
        }],
    }
}
fn empty(status: impl Into<String>) -> Grid {
    let mut status = status.into();
    while status.len() > 240 {
        status.pop();
    }
    Grid {
        cols: 80,
        rows: 12,
        background: engine::BACKGROUND,
        cells: vec![],
        styles: vec![],
        cursor: None,
        status,
    }
}
fn publish(shared: &Mutex<Published>, grid: Grid) {
    let mut state = shared.lock().unwrap();
    let revision = state.snapshot.panels[0].revision.saturating_add(1);
    let mut frame = snapshot(grid, revision);
    if frame.validate().is_err() || velk_panels::encode(&frame).is_err() {
        frame = snapshot(
            empty("Terminal frame exceeds panel limits; resize the panel or restart the plugin."),
            revision,
        );
    }
    state.snapshot = frame;
}
fn root(context: Context) -> anyhow::Result<PathBuf> {
    context.validate().map_err(anyhow::Error::msg)?;
    if let Some(path) = context.root {
        return Ok(std::fs::canonicalize(
            path.to_path().map_err(anyhow::Error::msg)?,
        )?);
    }
    let source = context
        .document
        .ok_or_else(|| anyhow::anyhow!("terminal has no workspace context"))?
        .to_path()
        .map_err(anyhow::Error::msg)?;
    let parent = source
        .parent()
        .ok_or_else(|| anyhow::anyhow!("document has no parent"))?;
    let mut cargo = None;
    for path in parent.ancestors() {
        if path.join(".git").exists() {
            return Ok(path.into());
        }
        if cargo.is_none() && path.join("Cargo.toml").is_file() {
            cargo = Some(path.to_path_buf());
        }
    }
    Ok(cargo.unwrap_or_else(|| parent.into()))
}
impl Default for Terminal {
    fn default() -> Self {
        let shared = Arc::new(Mutex::new(Published {
            snapshot: snapshot(empty("Show the terminal panel to start a shell."), 0),
        }));
        let configuration = Arc::new(Mutex::new(Configuration::default()));
        let options = configuration.clone();
        let state = shared.clone();
        let (sender, receiver) = mpsc::sync_channel(128);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stopping = stop.clone();
        let poll = mio::Poll::new().ok();
        let wake = poll
            .as_ref()
            .and_then(|p| mio::Waker::new(p.registry(), mio::Token(0)).ok())
            .map(Arc::new);
        let worker = poll.zip(wake.clone()).and_then(|(mut poll, _)| {
            std::thread::Builder::new()
                .name("velk-terminal".into())
                .spawn(move || {
                    let mut directory = None;
                    let mut engine: Option<engine::Engine> = None;
                    let mut dirty = false;
                    let mut last = Instant::now();
                    let mut io_events = mio::Events::with_capacity(8);
                    let mut registered = false;
                    while !stopping.load(std::sync::atomic::Ordering::Acquire) {
                        let result = (|| -> anyhow::Result<()> {
                            let timeout = if dirty {
                                Some(Duration::from_millis(8).saturating_sub(last.elapsed()))
                            } else if engine.as_ref().is_some_and(|e| e.alive()) {
                                Some(Duration::from_millis(250))
                            } else {
                                None
                            };
                            match poll.poll(&mut io_events, timeout) {
                                Ok(()) => {}
                                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {
                                    return Ok(());
                                }
                                Err(e) => return Err(e.into()),
                            }
                            if stopping.load(std::sync::atomic::Ordering::Acquire) {
                                return Ok(());
                            }
                            for work in receiver.try_iter().take(128) {
                                match work {
                                    Work::Context(context) => {
                                        if engine.is_none() {
                                            directory = Some(root(context)?);
                                        }
                                    }
                                    Work::Input(events) => {
                                        for input in events {
                                            if engine.is_none() {
                                                let (cols, rows) = match input {
                                                    Input::Resize { cols, rows } => (cols, rows),
                                                    _ => (80, 12),
                                                };
                                                let root = directory.as_ref().ok_or_else(|| {
                                                    anyhow::anyhow!(
                                                        "terminal has no workspace directory"
                                                    )
                                                })?;
                                                let configuration = options.lock().unwrap().clone();
                                                let shell =
                                                    (!configuration.shell.is_empty()).then(|| {
                                                        std::path::Path::new(&configuration.shell)
                                                    });
                                                engine = Some(engine::Engine::new(
                                                    root,
                                                    cols,
                                                    rows,
                                                    shell,
                                                    configuration.scrollback_bytes,
                                                )?);
                                            }
                                            engine.as_mut().unwrap().input(input)?;
                                            dirty = true;
                                        }
                                    }
                                }
                            }
                            if let Some(engine) = &mut engine {
                                dirty |= engine.tick()?;
                                engine.interest(poll.registry(), registered)?;
                                registered = engine.alive();
                                if dirty && last.elapsed() >= Duration::from_millis(8) {
                                    publish(&state, engine.snapshot()?);
                                    dirty = false;
                                    last = Instant::now();
                                }
                            }
                            Ok(())
                        })();
                        if let Err(error) = result {
                            let message: String = format!("Terminal error: {error}")
                                .chars()
                                .take(200)
                                .collect();
                            publish(&state, empty(message));
                            break; // A stopped/failed terminal is explicitly restarted by the user.
                        }
                    }
                    drop(engine); // Closes the PTY, terminates/reaps the owned child.
                })
                .ok()
        });
        Self {
            configuration,
            shared,
            sender: Some(sender),
            worker,
            stop,
            wake,
        }
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Release);
        self.sender.take();
        if let Some(wake) = &self.wake {
            let _ = wake.wake();
        }
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}
impl Plugin for Terminal {
    const ID: &'static str = "velk.terminal";
    const VERSION: &'static str = env!("CARGO_PKG_VERSION");
    const NAME: &'static str = "Terminal";
    const README: &'static str = include_str!("../README.md");
    const CAPABILITIES: &'static [velk_plugin_sdk::CapabilityDeclaration] = &[
        velk_plugin_sdk::CapabilityDeclaration::required(
            "process.spawn",
            "Run the configured interactive shell in a pseudo-terminal.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::required(
            "environment.read",
            "Read the configured shell and its inherited environment.",
        ),
        velk_plugin_sdk::CapabilityDeclaration::required(
            "filesystem.read",
            "Resolve the terminal working directory.",
        ),
    ];
    fn settings() -> Vec<velk_plugin_sdk::settings::Definition> {
        use velk_plugin_sdk::settings::{Definition, Kind};
        vec![
            Definition {
                key: "shell".into(), title: "Shell executable".into(),
                description: "Shell path or command. Empty uses VELK_TERMINAL_SHELL, then SHELL, then /bin/sh.".into(),
                kind: Kind::Text { max_length: 4096 }, default: "".into(),
            },
            Definition {
                key: "scrollback_bytes".into(), title: "Scrollback limit (bytes)".into(),
                description: "Approximate history memory budget in bytes. Zero disables scrollback. Restarting ends its shell session.".into(),
                kind: Kind::Integer { min: 0, max: 100_000_000 }, default: 10_000.into(),
            },
        ]
    }
    fn configure(&mut self, values: &velk_plugin_sdk::settings::Values) -> Result<(), String> {
        let shell = values["shell"]
            .as_str()
            .ok_or("invalid terminal shell")?
            .to_owned();
        let scrollback_bytes = values["scrollback_bytes"]
            .as_u64()
            .ok_or("invalid scrollback limit")? as usize;
        *self.configuration.lock().unwrap() = Configuration {
            shell,
            scrollback_bytes,
        };
        Ok(())
    }
    fn describe(&mut self) -> String {
        "Ghostty terminal emulator with an owned PTY".into()
    }
    fn register(reg: &mut Registrar<Self>) -> Result<(), String> {
        reg.provide(
            hooks::Request(velk_panels::HOOK),
            |this: &mut Self, bytes| {
                let _trace = velk_plugin_sdk::diagnostics::scope!("terminal.protocol");
                velk_panels::encode(&this.request(velk_panels::decode(bytes)?)?)
            },
        )
    }
}
impl Terminal {
    fn send(&self, work: Work) -> Result<(), String> {
        if self.worker.as_ref().is_none_or(|w| w.is_finished()) {
            return Err("terminal worker unavailable".into());
        }
        self.sender
            .as_ref()
            .ok_or("terminal stopped")?
            .try_send(work)
            .map_err(|e| match e {
                TrySendError::Full(_) => String::from("terminal input queue full"),
                TrySendError::Disconnected(_) => String::from("terminal worker stopped"),
            })?;
        if let Some(wake) = &self.wake {
            wake.wake().map_err(|e| e.to_string())?;
        }
        Ok(())
    }
    fn request(&mut self, request: Request) -> Result<Reply, String> {
        match request {
            Request::Describe { context } => {
                context.validate()?;
                self.send(Work::Context(context))?;
                let state = self.shared.lock().unwrap();
                Ok(Reply::Ready {
                    snapshot: state.snapshot.clone(),
                })
            }
            Request::Updates { revision } => {
                let state = self.shared.lock().unwrap();
                if state.snapshot.panels[0].revision == revision {
                    return Ok(Reply::Unchanged);
                }
                Ok(Reply::Ready {
                    snapshot: state.snapshot.clone(),
                })
            }
            Request::Input { panel, events } => {
                if panel != "terminal" || events.len() > 64 {
                    return Err("invalid terminal input target or batch".into());
                }
                let mut bytes = 0;
                for event in &events {
                    event.validate()?;
                    if let Input::Text { text, .. } = event {
                        bytes += text.len();
                    }
                }
                if bytes > 64 * 1024 {
                    return Err("terminal input batch too large".into());
                }
                self.send(Work::Input(events))?;
                Ok(Reply::Ack)
            }
            Request::Changes => Ok(Reply::Unchanged),
            _ => Err("unsupported terminal panel request".into()),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ghostty_links_and_parses_vt() {
        let mut term = ghostty_vt::Terminal::new(ghostty_vt::TerminalOptions {
            cols: 80,
            rows: 24,
            max_scrollback: 1000,
        })
        .unwrap();
        term.vt_write(b"hello\r\n");
        assert_eq!(term.cursor_y().unwrap(), 1);
    }
    #[test]
    fn plugin_queues_input_streams_frames_and_restarts_without_a_shell_until_shown() {
        let dir = tempfile::tempdir().unwrap();
        let mut plugin = Terminal::default();
        plugin
            .configure(&velk_plugin_sdk::settings::Values::from([
                ("shell".into(), "/bin/sh".into()),
                ("scrollback_bytes".into(), 100.into()),
            ]))
            .unwrap();
        let context = Context {
            root: Some(velk_panels::NativePath::new(dir.path())),
            ..Default::default()
        };
        let Reply::Ready { snapshot } = plugin
            .request(Request::Describe {
                context: context.clone(),
            })
            .unwrap()
        else {
            panic!("expected snapshot")
        };
        let Content::Grid(grid) = &snapshot.panels[0].content else {
            panic!("expected grid")
        };
        assert!(grid.cursor.is_none() && !grid.status.contains("PID"));
        assert!(
            plugin
                .request(Request::Input {
                    panel: "wrong".into(),
                    events: vec![]
                })
                .is_err()
        );
        assert!(matches!(
            plugin
                .request(Request::Input {
                    panel: "terminal".into(),
                    events: vec![
                        Input::Resize { cols: 80, rows: 12 },
                        Input::Text {
                            text: "printf 'PLUGIN_%s\\n' READY\n".into(),
                            paste: false
                        }
                    ]
                })
                .unwrap(),
            Reply::Ack
        ));
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            if let Reply::Ready { snapshot } =
                plugin.request(Request::Updates { revision: 0 }).unwrap()
                && let Content::Grid(grid) = &snapshot.panels[0].content
            {
                let text: String = grid.cells.iter().map(|cell| cell.2.as_str()).collect();
                if text.contains("PLUGIN_READY") {
                    assert!(snapshot.effects.is_empty());
                    break;
                }
            }
            assert!(Instant::now() < deadline, "terminal did not publish output");
            std::thread::sleep(Duration::from_millis(10));
        }
        let start = Instant::now();
        drop(plugin);
        assert!(start.elapsed() < Duration::from_secs(2));
        let mut fresh = Terminal::default();
        let Reply::Ready { snapshot } = fresh.request(Request::Describe { context }).unwrap()
        else {
            panic!("expected snapshot")
        };
        let Content::Grid(grid) = &snapshot.panels[0].content else {
            panic!("expected grid")
        };
        assert!(grid.cursor.is_none() && !grid.status.contains("PID"));
    }
}
