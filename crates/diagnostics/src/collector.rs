use arc_swap::ArcSwapOption;
use serde_json::{Map, Value, json};
use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    fs::{File, OpenOptions},
    io::{BufWriter, IsTerminal, Write},
    path::PathBuf,
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tracing::{
    Subscriber,
    field::{Field, Visit},
    span::{Attributes, Id, Record},
};
use tracing_subscriber::{
    Layer,
    layer::{Context, Filter},
    prelude::*,
    registry::LookupSpan,
};

const QUEUE: usize = 8192;
const MAX_EVENTS: usize = 1_000_000;
const MAX_BYTES: usize = 128 * 1024 * 1024;
static STATE: OnceLock<Arc<State>> = OnceLock::new();
static IDS: AtomicU64 = AtomicU64::new(1);
static THREADS: AtomicU64 = AtomicU64::new(1);
thread_local! {
    static ROLE: RefCell<Option<String>> = const { RefCell::new(None) };
    static LAST_CAPTURE: Cell<u64> = const { Cell::new(0) };
    static THREAD: u64 = THREADS.fetch_add(1, Ordering::Relaxed);
    static ENTERED: RefCell<HashMap<u64, Vec<(u64, u64)>>> = RefCell::default();
}
struct State {
    clock: Instant,
    recording: AtomicBool,
    targets: Vec<String>,
    current: ArcSwapOption<Capture>,
    operation: Mutex<()>,
    directory: PathBuf,
    duration: Duration,
}
struct Capture {
    id: u64,
    start: u64,
    path: PathBuf,
    sender: mpsc::SyncSender<Message>,
    stopped: AtomicBool,
    dropped: AtomicU64,
    worker: Mutex<Option<JoinHandle<()>>>,
    result: Mutex<Option<Result<Report, String>>>,
}
enum Message {
    Event(Value),
    Stop,
}
#[derive(Clone, Debug)]
pub struct Report {
    pub path: PathBuf,
    pub events: usize,
    pub dropped: u64,
    pub reason: String,
}
#[derive(Clone, Debug)]
pub struct Status {
    pub recording: bool,
    pub path: PathBuf,
}

pub struct Guard {
    dropped_logs: tracing_appender::non_blocking::ErrorCounter,
    logs: Option<tracing_appender::non_blocking::WorkerGuard>,
}
impl Drop for Guard {
    fn drop(&mut self) {
        let _ = stop();
        drop(self.logs.take());
        let dropped = self.dropped_logs.dropped_lines();
        if dropped > 0 {
            eprintln!("velk: log queue dropped {dropped} records");
        }
    }
}
fn now() -> u64 {
    STATE.get().map_or(0, |state| {
        state.clock.elapsed().as_nanos().min(u64::MAX as u128) as u64
    })
}
fn active() -> Option<Arc<Capture>> {
    let state = STATE.get()?;
    if !state.recording.load(Ordering::Relaxed) {
        return None;
    }
    STATE
        .get()?
        .current
        .load_full()
        .filter(|c| !c.stopped.load(Ordering::Relaxed))
}
pub fn status() -> Option<Status> {
    STATE.get()?.current.load_full().map(|c| Status {
        recording: !c.stopped.load(Ordering::Acquire),
        path: c.path.clone(),
    })
}
pub fn available() -> bool {
    STATE.get().is_some()
}
pub fn recording() -> bool {
    STATE
        .get()
        .is_some_and(|state| state.recording.load(Ordering::Relaxed))
}
fn send(capture: &Capture, event: Value) {
    LAST_CAPTURE.with(|last| {
        if last.replace(capture.id) != capture.id {
            let current = thread::current();
            let name = current.name().map(str::to_owned).or_else(|| ROLE.with(|role| role.borrow().clone())).unwrap_or_else(|| "native worker".into());
            if capture.sender.try_send(Message::Event(json!({"name":"thread_name","ph":"M","pid":std::process::id(),"tid":THREAD.with(|id| *id),"args":{"name":name}}))).is_err() { capture.dropped.fetch_add(1, Ordering::Relaxed); }
        }
    });
    if !capture.stopped.load(Ordering::Relaxed)
        && capture.sender.try_send(Message::Event(event)).is_err()
    {
        capture.dropped.fetch_add(1, Ordering::Relaxed);
    }
}
#[expect(
    clippy::too_many_arguments,
    reason = "explicit fields mirror the Chrome trace event record"
)]
fn event(
    capture: &Capture,
    name: &str,
    target: &str,
    phase: &str,
    time: u64,
    duration: u64,
    id: u64,
    fields: Value,
) -> Value {
    let mut value = json!({"name":name,"cat":target,"ph":phase,"ts":time.saturating_sub(capture.start) as f64 / 1000.,"pid":std::process::id(),"tid":THREAD.with(|id| *id),"args":fields});
    match phase {
        "X" => value["dur"] = json!(duration as f64 / 1000.),
        "b" | "e" => {
            value["id"] = json!(format!("span-{id}"));
            value["scope"] = json!("velk");
        }
        "i" => value["s"] = json!("t"),
        _ => {}
    }
    value
}

/// Install once in the executable, before any application/worker initialization.
/// The normal path emits filtered structured logs; detailed capture starts idle.
pub fn init() -> Result<Guard, String> {
    let duration: u64 = std::env::var("VELK_TRACE_SECONDS")
        .ok()
        .map(|v| {
            v.parse::<u64>()
                .map_err(|_| "VELK_TRACE_SECONDS must be an integer".to_string())
        })
        .transpose()?
        .unwrap_or(30);
    if !(1..=600).contains(&duration) {
        return Err("VELK_TRACE_SECONDS must be between 1 and 600".into());
    }
    let directory = std::env::var_os("VELK_TRACE_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("velk-traces"));
    init_with(Config {
        directory,
        duration: Duration::from_secs(duration),
        filter: std::env::var("VELK_LOG")
            .or_else(|_| std::env::var("RUST_LOG"))
            .unwrap_or_else(|_| "warn,velk=info".into()),
        json_logs: std::env::var("VELK_LOG_FORMAT").as_deref() == Ok("json"),
        startup_capture: std::env::var("VELK_TRACE").as_deref() == Ok("1"),
        log_directory: std::env::var_os("VELK_LOG_DIR").map(PathBuf::from),
        targets: std::env::var("VELK_TRACE_TARGETS")
            .unwrap_or_else(|_| "velk,rxui,astrelis".into())
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect(),
    })
}
pub struct Config {
    pub directory: PathBuf,
    pub duration: Duration,
    pub filter: String,
    pub json_logs: bool,
    pub startup_capture: bool,
    pub targets: Vec<String>,
    pub log_directory: Option<PathBuf>,
}
fn log_colors(terminal: bool, json: bool, file: bool, no_color: bool, dumb_terminal: bool) -> bool {
    terminal && !json && !file && !no_color && !dumb_terminal
}

pub fn init_with(config: Config) -> Result<Guard, String> {
    if config.duration.is_zero() || config.duration > Duration::from_secs(600) {
        return Err("capture duration must be positive and at most 600 seconds".into());
    }
    let state = Arc::new(State {
        clock: Instant::now(),
        recording: AtomicBool::new(false),
        targets: config.targets,
        current: ArcSwapOption::empty(),
        operation: Mutex::new(()),
        directory: config.directory,
        duration: config.duration,
    });
    let filter =
        tracing_subscriber::EnvFilter::try_new(&config.filter).map_err(|e| e.to_string())?;
    let ansi = log_colors(
        std::io::stderr().is_terminal(),
        config.json_logs,
        config.log_directory.is_some(),
        std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty()),
        std::env::var("TERM").as_deref() == Ok("dumb"),
    );
    let output: Box<dyn Write + Send> = if let Some(directory) = config.log_directory {
        std::fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        Box::new(
            OpenOptions::new()
                .create(true)
                .append(true)
                .open(directory.join(format!("velk-{}.log", std::process::id())))
                .map_err(|e| e.to_string())?,
        )
    } else {
        Box::new(std::io::stderr())
    };
    let (writer, logs) = tracing_appender::non_blocking::NonBlockingBuilder::default()
        .buffered_lines_limit(4096)
        .lossy(true)
        .thread_name("velk-logs")
        .finish(output);
    let dropped_logs = writer.error_counter();
    let log_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(ansi)
        .with_thread_names(true)
        .with_target(true);
    let log_layer = if config.json_logs {
        log_layer.json().boxed()
    } else {
        log_layer.boxed()
    };
    tracing_subscriber::registry()
        .with(log_layer.with_filter(filter))
        .with(TraceLayer.with_filter(CaptureFilter))
        .try_init()
        .map_err(|e| e.to_string())?;
    STATE
        .set(state)
        .map_err(|_| "diagnostics already initialized".to_string())?;
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |panic| {
        tracing::error!(target: "velk_diagnostics", %panic, "panic");
        previous(panic);
    }));
    let guard = Guard {
        logs: Some(logs),
        dropped_logs,
    };
    if config.startup_capture {
        start()?;
    }
    Ok(guard)
}
pub fn start() -> Result<PathBuf, String> {
    let state = STATE.get().ok_or("diagnostics unavailable")?;
    let _operation = state.operation.lock().map_err(|e| e.to_string())?;
    if recording() {
        return Err("a trace capture is already running".into());
    }
    if let Some(previous) = state.current.load_full()
        && let Err(error) = finish(&previous)
    {
        tracing::warn!(%error, "previous trace could not be finalized");
    }
    std::fs::create_dir_all(&state.directory).map_err(|e| e.to_string())?;
    let id = IDS.fetch_add(1, Ordering::Relaxed);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_micros();
    let path = state.directory.join(format!(
        "velk-{stamp}-{}-{id}.trace.json",
        std::process::id()
    ));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| e.to_string())?;
    let (sender, receive) = mpsc::sync_channel(QUEUE);
    let capture = Arc::new(Capture {
        id,
        start: now(),
        path: path.clone(),
        sender,
        stopped: AtomicBool::new(false),
        dropped: AtomicU64::new(0),
        worker: Mutex::new(None),
        result: Mutex::new(None),
    });
    let copy = capture.clone();
    let duration = state.duration;
    let worker = thread::Builder::new()
        .name("velk-trace-writer".into())
        .spawn(move || {
            let result = write_trace(file, &copy, receive, duration).map_err(|e| e.to_string());
            copy.stopped.store(true, Ordering::Release);
            if let Some(state) = STATE.get() {
                state.recording.store(false, Ordering::Release);
            }
            *copy.result.lock().unwrap() = Some(result);
        })
        .map_err(|e| e.to_string())?;
    *capture.worker.lock().unwrap() = Some(worker);
    state.current.store(Some(capture.clone()));
    state.recording.store(true, Ordering::Release);
    send(
        &capture,
        json!({"name":"process_name","ph":"M","pid":std::process::id(),"tid":0,"args":{"name":"Velk"}}),
    );
    marker("capture started");
    tracing::info!(target: "velk_diagnostics", path = %path.display(), seconds = duration.as_secs(), "trace capture started");
    Ok(path)
}
fn finish(capture: &Capture) -> Result<Report, String> {
    if let Some(state) = STATE.get() {
        state.recording.store(false, Ordering::Release);
    }
    capture.stopped.store(true, Ordering::Release);
    if let Some(worker) = capture.worker.lock().map_err(|e| e.to_string())?.take() {
        let _ = capture.sender.send(Message::Stop);
        worker
            .join()
            .map_err(|_| "trace writer panicked".to_string())?;
    }
    capture
        .result
        .lock()
        .map_err(|e| e.to_string())?
        .clone()
        .unwrap_or_else(|| Err("trace writer did not finish".into()))
}
pub fn stop() -> Result<Report, String> {
    let state = STATE.get().ok_or("diagnostics unavailable")?;
    let _operation = state.operation.lock().map_err(|e| e.to_string())?;
    let capture = state
        .current
        .load_full()
        .ok_or("no trace capture has been started")?;
    finish(&capture)
}
pub fn marker(name: &str) {
    if let Some(capture) = active() {
        send(
            &capture,
            event(
                &capture,
                &truncate(name),
                "velk.marker",
                "i",
                now(),
                0,
                0,
                json!({}),
            ),
        );
    }
}
fn write_trace(
    file: File,
    capture: &Capture,
    receive: mpsc::Receiver<Message>,
    duration: Duration,
) -> std::io::Result<Report> {
    let mut writer = BufWriter::new(file);
    writer.write_all(b"{\"traceEvents\":[")?;
    let deadline = Instant::now() + duration;
    let mut events = 0;
    let mut bytes = 0;
    let reason = loop {
        if Instant::now() >= deadline {
            break "duration limit";
        }
        match receive.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok(Message::Event(value)) => {
                let value = serde_json::to_vec(&value)?;
                if events >= MAX_EVENTS || bytes + value.len() + 1 > MAX_BYTES - 64 * 1024 {
                    capture.dropped.fetch_add(1, Ordering::Relaxed);
                    break "size/event limit";
                }
                if events > 0 {
                    writer.write_all(b",")?;
                }
                writer.write_all(&value)?;
                bytes += value.len() + 1;
                events += 1;
            }
            Ok(Message::Stop) => break "stopped",
            Err(_) => break "duration limit",
        }
    };
    capture.stopped.store(true, Ordering::Release);
    // Stop is queued after prior producer events. A timeout/limit deliberately
    // cuts off the tail rather than draining an unbounded stream.
    let dropped = capture.dropped.load(Ordering::Relaxed)
        + receive
            .try_iter()
            .filter(|m| matches!(m, Message::Event(_)))
            .count() as u64;
    if events > 0 {
        writer.write_all(b",")?;
    }
    serde_json::to_writer(
        &mut writer,
        &json!({"name":"velk.capture.summary","ph":"i","s":"g","ts":now().saturating_sub(capture.start) as f64 / 1000.,"pid":std::process::id(),"tid":0,"args":{"events":events,"dropped":dropped,"reason":reason,"cpu_timing_only":true}}),
    )?;
    writer.write_all(b"],\"displayTimeUnit\":\"ms\"}")?;
    writer.flush()?;
    writer.get_ref().sync_all()?;
    Ok(Report {
        path: capture.path.clone(),
        events,
        dropped,
        reason: reason.into(),
    })
}
#[derive(Default)]
struct Fields(Map<String, Value>);
fn truncate(text: &str) -> String {
    text.chars().take(256).collect()
}
impl Visit for Fields {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if self.0.len() < 24 {
            self.0
                .insert(field.name().into(), json!(truncate(&format!("{value:?}"))));
        }
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        if self.0.len() < 24 {
            self.0.insert(field.name().into(), json!(truncate(value)));
        }
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.0.insert(field.name().into(), json!(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), json!(value));
    }
}
struct SpanData {
    capture: u64,
    start: u64,
    fields: Map<String, Value>,
    asynchronous: bool,
}
struct TraceLayer;
struct CaptureFilter;
impl<S: Subscriber> Filter<S> for CaptureFilter {
    fn enabled(&self, metadata: &tracing::Metadata<'_>, _: &Context<'_, S>) -> bool {
        recording()
            && STATE.get().is_some_and(|state| {
                state
                    .targets
                    .iter()
                    .any(|prefix| prefix == "*" || metadata.target().starts_with(prefix))
            })
    }
    fn callsite_enabled(
        &self,
        _: &'static tracing::Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        tracing::subscriber::Interest::sometimes()
    }
}
impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for TraceLayer {
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, cx: Context<'_, S>) {
        let Some(capture) = active() else {
            return;
        };
        let mut fields = Fields::default();
        attrs.record(&mut fields);
        let asynchronous = fields
            .0
            .get("async_work")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let start = now();
        if let Some(span) = cx.span(id) {
            if let Some(parent) = span.parent() {
                fields
                    .0
                    .insert("parent_span_id".into(), json!(parent.id().into_u64()));
            }
            fields.0.insert("span_id".into(), json!(id.into_u64()));
            if asynchronous {
                send(
                    &capture,
                    event(
                        &capture,
                        attrs.metadata().name(),
                        attrs.metadata().target(),
                        "b",
                        start,
                        0,
                        id.into_u64(),
                        json!(fields.0),
                    ),
                );
            }
            span.extensions_mut().insert(SpanData {
                capture: capture.id,
                start,
                fields: fields.0,
                asynchronous,
            });
        }
    }
    fn on_record(&self, id: &Id, values: &Record<'_>, cx: Context<'_, S>) {
        if let Some(span) = cx.span(id)
            && let Some(data) = span.extensions_mut().get_mut::<SpanData>()
        {
            let mut fields = Fields(std::mem::take(&mut data.fields));
            values.record(&mut fields);
            data.fields = fields.0;
        }
    }
    fn on_enter(&self, id: &Id, cx: Context<'_, S>) {
        if let Some(span) = cx.span(id)
            && let Some(data) = span.extensions().get::<SpanData>()
        {
            ENTERED.with(|stack| {
                stack
                    .borrow_mut()
                    .entry(id.into_u64())
                    .or_default()
                    .push((data.capture, now()))
            });
        }
    }
    fn on_exit(&self, id: &Id, cx: Context<'_, S>) {
        let started = ENTERED.with(|stack| {
            let mut stack = stack.borrow_mut();
            let entries = stack.get_mut(&id.into_u64())?;
            let result = entries.pop();
            if entries.is_empty() {
                stack.remove(&id.into_u64());
            }
            result
        });
        if let (Some((generation, start)), Some(capture), Some(span)) =
            (started, active(), cx.span(id))
            && generation == capture.id
            && let Some(data) = span.extensions().get::<SpanData>()
        {
            send(
                &capture,
                event(
                    &capture,
                    span.name(),
                    span.metadata().target(),
                    "X",
                    start,
                    now().saturating_sub(start),
                    id.into_u64(),
                    json!(data.fields),
                ),
            );
        }
    }
    fn on_close(&self, id: Id, cx: Context<'_, S>) {
        if let (Some(capture), Some(span)) = (active(), cx.span(&id))
            && let Some(data) = span.extensions().get::<SpanData>()
            && data.asynchronous
            && data.capture == capture.id
        {
            send(
                &capture,
                event(
                    &capture,
                    span.name(),
                    span.metadata().target(),
                    "e",
                    now(),
                    0,
                    id.into_u64(),
                    {
                        let mut fields = data.fields.clone();
                        fields.insert(
                            "wall_ms".into(),
                            json!(now().saturating_sub(data.start) as f64 / 1_000_000.),
                        );
                        json!(fields)
                    },
                ),
            );
        }
    }
    fn on_event(&self, value: &tracing::Event<'_>, _: Context<'_, S>) {
        if let Some(capture) = active() {
            let mut fields = Fields::default();
            value.record(&mut fields);
            fields
                .0
                .insert("level".into(), json!(value.metadata().level().as_str()));
            send(
                &capture,
                event(
                    &capture,
                    value.metadata().name(),
                    value.metadata().target(),
                    "i",
                    now(),
                    0,
                    0,
                    json!(fields.0),
                ),
            );
        }
    }
}

/// Process-owned callback table, copied into SDK plugins after factory creation.
pub fn sink() -> velk_plugin_api::DiagnosticsSink {
    velk_plugin_api::DiagnosticsSink {
        log_enabled: bridge_log_enabled,
        log: bridge_log,
        enabled: bridge_enabled,
        now_ns: bridge_now,
        next_id: bridge_id,
        record: bridge_record,
    }
}
#[allow(unsafe_code)]
unsafe extern "C" fn bridge_log_enabled(level: u32) -> bool {
    std::panic::catch_unwind(|| match level {
        1 => tracing::enabled!(target: "velk_plugin", tracing::Level::ERROR),
        2 => tracing::enabled!(target: "velk_plugin", tracing::Level::WARN),
        3 => tracing::enabled!(target: "velk_plugin", tracing::Level::INFO),
        4 => tracing::enabled!(target: "velk_plugin", tracing::Level::DEBUG),
        5 => tracing::enabled!(target: "velk_plugin", tracing::Level::TRACE),
        _ => false,
    })
    .unwrap_or(false)
}
#[allow(unsafe_code)]
unsafe extern "C" fn bridge_log(
    level: u32,
    target: velk_plugin_api::Bytes,
    message: velk_plugin_api::Bytes,
) {
    let _ = std::panic::catch_unwind(|| {
        if target.length > 256
            || message.length > 8192
            || target.data.is_null()
            || message.data.is_null()
        {
            return;
        }
        // SAFETY: the producer lends readable byte slices for this call only.
        let (Ok(target), Ok(message)) = (
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(target.data, target.length) }),
            std::str::from_utf8(unsafe {
                std::slice::from_raw_parts(message.data, message.length)
            }),
        ) else {
            return;
        };
        match level {
            1 => tracing::error!(target: "velk_plugin", plugin_target = target, "{message}"),
            2 => tracing::warn!(target: "velk_plugin", plugin_target = target, "{message}"),
            3 => tracing::info!(target: "velk_plugin", plugin_target = target, "{message}"),
            4 => tracing::debug!(target: "velk_plugin", plugin_target = target, "{message}"),
            5 => tracing::trace!(target: "velk_plugin", plugin_target = target, "{message}"),
            _ => {}
        }
    });
}
#[allow(unsafe_code)]
unsafe extern "C" fn bridge_enabled() -> bool {
    recording()
}
#[allow(unsafe_code)]
unsafe extern "C" fn bridge_now() -> u64 {
    now()
}
#[allow(unsafe_code)]
unsafe extern "C" fn bridge_id() -> u64 {
    IDS.fetch_add(1, Ordering::Relaxed)
}
#[allow(unsafe_code)]
unsafe extern "C" fn bridge_record(
    kind: u32,
    id: u64,
    name: velk_plugin_api::Bytes,
    target: velk_plugin_api::Bytes,
    start: u64,
    duration: u64,
) {
    let _ = std::panic::catch_unwind(|| {
        let Some(capture) = active() else {
            return;
        };
        if start < capture.start
            || start > now()
            || name.length == 0
            || name.length > 128
            || target.length > 128
            || name.data.is_null()
            || target.data.is_null()
        {
            return;
        }
        // SAFETY: SDK lends these UTF-8 slices for this synchronous callback only.
        let (Ok(name), Ok(target)) = (
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(name.data, name.length) }),
            std::str::from_utf8(unsafe { std::slice::from_raw_parts(target.data, target.length) }),
        ) else {
            return;
        };
        if thread::current().name().is_none() {
            ROLE.with(|role| {
                if role.borrow().is_none() {
                    *role.borrow_mut() =
                        Some(target.split("::").next().unwrap_or(target).to_owned());
                }
            });
        }
        if !STATE.get().is_some_and(|state| {
            state
                .targets
                .iter()
                .any(|prefix| prefix == "*" || target.starts_with(prefix))
        }) {
            return;
        }
        let phase = match kind {
            0 => "X",
            1 => "i",
            2 => "b",
            3 => "e",
            _ => return,
        };
        let time = if kind == 3 {
            start.saturating_add(duration)
        } else {
            start
        };
        let mut value = event(
            &capture,
            name,
            target,
            phase,
            time,
            duration,
            id,
            json!({"source":"native_plugin"}),
        );
        if kind == 2 || kind == 3 {
            value["id"] = json!(format!("plugin-{id}"));
        }
        send(&capture, value);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn probe() {
        let _scope = tracing::debug_span!("capture.probe", revision = 7).entered();
    }
    fn renderer_probe() {
        let _scope =
            tracing::info_span!(target: "rxui::painting", "rxui::UiPainter::prepare").entered();
    }
    #[test]
    fn capture_switches_live_callsites_tracks_threads_and_rejects_previous_sessions() {
        let directory = tempfile::tempdir().unwrap();
        let _guard = init_with(Config {
            directory: directory.path().into(),
            duration: Duration::from_millis(250),
            filter: "warn,velk=info".into(),
            json_logs: true,
            startup_capture: false,
            targets: vec!["velk".into(), "rxui".into()],
            log_directory: None,
        })
        .unwrap();
        renderer_probe();
        probe(); // Register the callsite while recording is disabled.
        assert!(!recording());
        let first = thread::spawn(start).join().unwrap().unwrap();
        renderer_probe();
        assert!(start().is_err());
        probe();
        let wall = tracing::debug_span!("capture.wall", async_work = true);
        let worker = wall.clone();
        thread::Builder::new()
            .name("capture-test-worker".into())
            .spawn(move || worker.in_scope(probe))
            .unwrap()
            .join()
            .unwrap();
        drop(wall);
        marker("test marker");
        let stale = tracing::debug_span!("capture.stale", async_work = true);
        let report = stop().unwrap();
        assert_eq!(report.path, first);
        assert_eq!(report.dropped, 0);
        let trace: Value = serde_json::from_reader(File::open(&first).unwrap()).unwrap();
        let events = trace["traceEvents"].as_array().unwrap();
        assert!(
            events.iter().any(|e| e["ph"] == "X"
                && e["name"] == "capture.probe"
                && e["args"]["revision"] == 7)
        );
        assert!(
            events
                .iter()
                .any(|e| e["ph"] == "b" && e["name"] == "capture.wall")
        );
        assert!(
            events
                .iter()
                .any(|e| e["ph"] == "e" && e["name"] == "capture.wall")
        );
        assert!(
            events
                .iter()
                .any(|e| e["name"] == "thread_name" && e["args"]["name"] == "capture-test-worker")
        );
        assert!(events.iter().any(|e| e["name"] == "test marker"));
        assert_eq!(
            events
                .iter()
                .filter(|e| e["name"] == "rxui::UiPainter::prepare")
                .count(),
            1
        );
        renderer_probe();
        let next = thread::spawn(start).join().unwrap().unwrap();
        renderer_probe();
        stale.in_scope(probe);
        drop(stale);
        std::thread::sleep(Duration::from_millis(300));
        assert!(!recording());
        assert_eq!(stop().unwrap().reason, "duration limit");
        let trace: Value = serde_json::from_reader(File::open(next).unwrap()).unwrap();
        assert_eq!(
            trace["traceEvents"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|e| e["name"] == "rxui::UiPainter::prepare")
                .count(),
            1
        );
        assert!(
            !trace["traceEvents"]
                .as_array()
                .unwrap()
                .iter()
                .any(|e| e["name"] == "capture.stale")
        );
    }
    #[test]
    fn a_full_queue_is_lossy_and_the_writer_produces_a_completed_trace() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("bounded.json");
        let (sender, receive) = mpsc::sync_channel(1);
        let capture = Capture {
            id: u64::MAX,
            start: 0,
            path: path.clone(),
            sender,
            stopped: AtomicBool::new(false),
            dropped: AtomicU64::new(0),
            worker: Mutex::new(None),
            result: Mutex::new(None),
        };
        for _ in 0..10 {
            send(
                &capture,
                json!({"name":"probe","ph":"i","ts":0,"pid":1,"tid":1}),
            );
        }
        assert!(capture.dropped.load(Ordering::Relaxed) >= 9);
        let report = write_trace(
            File::create(&path).unwrap(),
            &capture,
            receive,
            Duration::from_millis(1),
        )
        .unwrap();
        assert!(report.dropped >= 9);
        let trace: Value = serde_json::from_reader(File::open(path).unwrap()).unwrap();
        assert_eq!(
            trace["traceEvents"].as_array().unwrap().last().unwrap()["name"],
            "velk.capture.summary"
        );
    }
}

#[cfg(test)]
mod log_tests {
    use super::log_colors;

    #[test]
    fn colors_only_readable_terminal_logs_unless_disabled() {
        assert!(log_colors(true, false, false, false, false));
        assert!(!log_colors(false, false, false, false, false));
        assert!(!log_colors(true, true, false, false, false));
        assert!(!log_colors(true, false, true, false, false));
        assert!(!log_colors(true, false, false, true, false));
        assert!(!log_colors(true, false, false, false, true));
    }
}
