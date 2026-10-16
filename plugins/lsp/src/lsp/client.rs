//! One physical connection per workspace/configuration; document channels retain
//! their own request IDs, cancellation, versions, and event consumers.
use super::*;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tokio::sync::{mpsc, oneshot, watch};

#[derive(Clone, PartialEq, Eq)]
struct Key {
    root: PathBuf,
    executable: OsString,
    arguments: Vec<OsString>,
    options: Value,
    configuration: Value,
}
#[derive(Default)]
pub struct Client {
    state: Mutex<State>,
    host: Mutex<Option<velk_plugin_sdk::Host>>,
}
#[derive(Default)]
struct State {
    next: u64,
    workers: Vec<Worker>,
}
type Documents = Arc<Mutex<BTreeMap<u64, (DocumentSnapshot, i32)>>>;
struct Worker {
    active: Arc<std::sync::atomic::AtomicUsize>,
    documents: Documents,
    id: u64,
    key: Key,
    send: mpsc::Sender<Control>,
    stop: watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.stop.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
enum Control {
    Attach {
        id: u64,
        send: mpsc::Sender<Result<Value, String>>,
    },
    Message {
        id: u64,
        value: Value,
    },
    Detach {
        id: u64,
        done: oneshot::Sender<()>,
    },
}
struct Attachment {
    send: mpsc::Sender<Result<Value, String>>,
    document: Option<(String, i32)>,
}
pub(super) struct Connection {
    send: mpsc::Sender<Control>,
    messages: mpsc::Receiver<Result<Value, String>>,
    id: u64,
    worker: u64,
    documents: Documents,
    active: Arc<std::sync::atomic::AtomicUsize>,
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.active
            .fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
impl Client {
    pub fn attach_host(&self, host: velk_plugin_sdk::Host) {
        *self.host.lock().unwrap() = Some(host);
    }
    pub fn retire_idle(&self) {
        let idle = {
            let mut state = self.state.lock().unwrap();
            let mut idle = Vec::new();
            let mut index = 0;
            while index < state.workers.len() {
                if state.workers[index]
                    .active
                    .load(std::sync::atomic::Ordering::Acquire)
                    == 0
                {
                    idle.push(state.workers.remove(index));
                } else {
                    index += 1;
                }
            }
            idle
        };
        drop(idle);
    }
    pub async fn run(
        self: &Arc<Self>,
        backend: LspBackend,
        session: Session,
    ) -> Result<(), String> {
        let _trace = velk_plugin_sdk::diagnostics::async_scope!("lsp.session");
        let first = loop {
            match session.next().await {
                Some(Input::Document(d)) => break d,
                Some(_) => {}
                None => return Ok(()),
            }
        };
        let root = match backend.root {
            Some(root) => root,
            None => tokio::select! {
                root = project_root(&first.path) => root,
                _ = session.cancelled() => return Ok(()),
            },
        };
        let root = root
            .canonicalize()
            .map_err(|e| format!("LSP workspace root: {e}"))?;
        let options = backend.initialization_options;
        let key = Key {
            root: root.clone(),
            executable: backend.executable,
            arguments: backend.arguments,
            options: options.clone(),
            configuration: backend.configuration,
        };
        let mut connection = self.attach(key)?;
        let result = tokio::select! {
            result = drive(&mut connection, &session, first, &root, options) => result,
            _ = session.cancelled() => Ok(()),
        };
        if session.provider_retired() {
            let worker = {
                let mut state = self.state.lock().unwrap();
                state
                    .workers
                    .iter()
                    .position(|w| w.id == connection.worker)
                    .map(|index| state.workers.remove(index))
            };
            drop(worker);
        } else {
            connection.shutdown().await;
        }
        result
    }
    fn attach(&self, key: Key) -> Result<Connection, String> {
        let mut state = self.state.lock().unwrap();
        state
            .workers
            .retain(|w| !w.thread.as_ref().is_none_or(|t| t.is_finished()));
        state.next = state
            .next
            .checked_add(1)
            .ok_or("LSP client identity overflow")?;
        let id = state.next;
        let worker = match state.workers.iter().position(|w| w.key == key) {
            Some(index) => index,
            None => {
                if state.workers.len() >= 16 {
                    return Err("LSP workspace client limit reached".into());
                }
                let (send, receive) = mpsc::channel(256);
                let (stop, stopped) = watch::channel(false);
                let configuration = key.clone();
                let documents: Documents = Arc::default();
                let published_documents = documents.clone();
                let host = self.host.lock().unwrap().clone();
                let thread = std::thread::Builder::new()
                    .name("velk-lsp-workspace".into())
                    .spawn(move || {
                        if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                            .enable_all()
                            .build()
                        {
                            runtime.block_on(workspace(
                                configuration,
                                receive,
                                stopped,
                                host,
                                id,
                                published_documents,
                            ));
                        }
                    })
                    .map_err(|e| e.to_string())?;
                state.workers.push(Worker {
                    active: Arc::default(),
                    documents,
                    id,
                    key,
                    send,
                    stop,
                    thread: Some(thread),
                });
                state.workers.len() - 1
            }
        };
        let worker = &state.workers[worker];
        let (send, messages) = mpsc::channel(32);
        worker
            .send
            .try_send(Control::Attach { id, send })
            .map_err(|_| "LSP workspace queue unavailable")?;
        worker
            .active
            .fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Ok(Connection {
            active: worker.active.clone(),
            send: worker.send.clone(),
            messages,
            id,
            worker: worker.id,
            documents: worker.documents.clone(),
        })
    }
}
impl Connection {
    pub fn record(&self, snapshot: &DocumentSnapshot, version: i32) {
        self.documents
            .lock()
            .unwrap()
            .insert(self.id, (snapshot.clone(), version));
    }
    pub fn documents(&self) -> Vec<(DocumentSnapshot, i32)> {
        self.documents.lock().unwrap().values().cloned().collect()
    }
    async fn send(&self, value: Value) -> Result<(), String> {
        self.send
            .send(Control::Message { id: self.id, value })
            .await
            .map_err(|_| "LSP workspace stopped".into())
    }
    pub async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<(), String> {
        match self
            .send(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}))
            .await
        {
            Ok(()) => Ok(()),
            Err(error) => match self.read().await {
                Err(cause) => Err(cause),
                Ok(_) => Err(error),
            },
        }
    }
    pub async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc":"2.0", "method":method, "params":params}))
            .await
    }
    pub async fn read(&mut self) -> Result<Value, String> {
        self.messages
            .recv()
            .await
            .ok_or("LSP workspace disconnected")?
    }
    pub async fn handle_server_request(&mut self, _: &Value) -> Result<(), String> {
        Ok(())
    }
    async fn shutdown(&mut self) {
        self.documents.lock().unwrap().remove(&self.id);
        let (done, finished) = oneshot::channel();
        if self
            .send
            .send(Control::Detach { id: self.id, done })
            .await
            .is_ok()
        {
            let _ = finished.await;
        }
    }
}
fn deliver(attachment: &Attachment, value: Value) -> Result<(), String> {
    // A slow document must not block traffic for other documents. Disconnect it
    // explicitly rather than losing a command result in a full queue.
    attachment
        .send
        .try_send(Ok(value))
        .map_err(|_| "LSP document receiver unavailable or stalled".into())
}
async fn workspace(
    key: Key,
    mut receive: mpsc::Receiver<Control>,
    mut stopped: watch::Receiver<bool>,
    host: Option<velk_plugin_sdk::Host>,
    client: u64,
    documents: Documents,
) {
    let mut attachments = BTreeMap::<u64, Attachment>::new();
    // Queue the first attachment before attempting spawn, so startup failures
    // always reach its event receiver rather than racing a closed control queue.
    let Some(Control::Attach { id, send }) = (tokio::select! {
        control = receive.recv() => control,
        _ = stopped.changed() => None,
    }) else {
        return;
    };
    attachments.insert(
        id,
        Attachment {
            send,
            document: None,
        },
    );
    let mut connection = match transport::Connection::spawn(
        &key.executable,
        &key.arguments,
        &key.root,
        key.configuration.clone(),
    ) {
        Ok(connection) => connection,
        Err(error) => {
            // Attach was queued before this worker started. Report the actual
            // spawn error to every attachment already waiting for initialization.
            receive.close();
            for attachment in attachments.values() {
                let _ = attachment.send.try_send(Err(error.clone()));
            }
            while let Some(control) = receive.recv().await {
                match control {
                    Control::Attach { send, .. } => {
                        let _ = send.try_send(Err(error.clone()));
                    }
                    Control::Detach { done, .. } => {
                        let _ = done.send(());
                    }
                    _ => {}
                }
            }
            return;
        }
    };
    connection.root_uri = uri(&key.root).unwrap_or_default();
    let result = multiplex(
        &mut connection,
        &mut attachments,
        &mut receive,
        &mut stopped,
        &host,
        client,
        &documents,
    )
    .await;
    if let Err(error) = result {
        for attachment in attachments.values() {
            let _ = attachment.send.try_send(Err(error.clone()));
        }
    }
    for attachment in attachments.values() {
        if let Some((uri, _)) = &attachment.document {
            let _ = connection
                .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
                .await;
        }
    }
    attachments.clear();
    if let Some(host) = &host {
        let _ = host.request(velk_plugin_sdk::host::Request::ClearDiagnostics { client });
    }
    connection.shutdown().await;
}
async fn multiplex(
    connection: &mut transport::Connection,
    attachments: &mut BTreeMap<u64, Attachment>,
    receive: &mut mpsc::Receiver<Control>,
    stopped: &mut watch::Receiver<bool>,
    host: &Option<velk_plugin_sdk::Host>,
    client: u64,
    documents: &Documents,
) -> Result<(), String> {
    let mut encoding = Encoding::Utf16;
    let mut initialized = false;
    let mut initialize_sent = false;
    let mut initialization = None::<Value>;
    let mut waiting = Vec::<(u64, Value)>::new();
    let mut requests = BTreeMap::<u64, (u64, Value)>::new();
    let mut next = 2u64;
    let mut versions = BTreeMap::<String, i32>::new();
    loop {
        tokio::select! {
            _ = stopped.changed() => return Ok(()),
            control = receive.recv() => match control {
                None => return Ok(()),
                Some(Control::Attach { id, send }) => {
                    if attachments.len() >= 256 { let _ = send.try_send(Err("LSP document limit reached".into())); }
                    else { attachments.insert(id, Attachment { send, document: None }); }
                }
                Some(Control::Detach { id, done }) => {
                    if let Some(attachment) = attachments.remove(&id) {
                        if let Some((uri,_)) = attachment.document { connection.notify("textDocument/didClose", json!({"textDocument":{"uri":uri}})).await?; }
                        let pending: Vec<_> = requests.iter().filter_map(|(&request,(owner,_))| (*owner == id).then_some(request)).collect();
                        for request in pending {
                            requests.remove(&request);
                            connection.notify("$/cancelRequest",json!({"id":request})).await?;
                        }
                    }
                    waiting.retain(|(owner,_)| *owner != id);
                    let _ = done.send(());
                }
                Some(Control::Message { id, mut value }) => {
                    if !attachments.contains_key(&id) { continue; }
                    let method = value["method"].as_str().unwrap_or("").to_owned();
                    if method == "initialize" {
                        if let Some(result) = &initialization {
                            let mut result = result.clone(); result["id"] = value["id"].clone();
                            deliver(&attachments[&id],result)?;
                        } else {
                            if !initialize_sent { initialize_sent = true; let mut request = value.clone(); request["id"] = json!(1); connection.send(request).await?; }
                            waiting.push((id,value["id"].clone()));
                        }
                        continue;
                    }
                    if method == "initialized" { if initialized { continue; } initialized = true; }
                    if method == "textDocument/didOpen" {
                        let uri = value["params"]["textDocument"]["uri"].as_str().ok_or("missing document URI")?.to_owned();
                        if attachments.values().any(|a| a.document.as_ref().is_some_and(|(other,_)| other == &uri)) { return Err("duplicate LSP document attachment".into()); }
                        if versions.len() >= 4096 && !versions.contains_key(&uri) { return Err("LSP document version history limit reached; restart the client".into()); }
                        let base = *versions.get(&uri).unwrap_or(&0);
                        let version = base.checked_add(1).ok_or("LSP document version overflow")?;
                        versions.insert(uri.clone(), version);
                        value["params"]["textDocument"]["version"] = json!(version);
                        attachments.get_mut(&id).unwrap().document = Some((uri,base));
                    }
                    if method == "textDocument/didChange" {
                        let (uri,base) = attachments[&id].document.as_ref().ok_or("document not open")?;
                        let local = value["params"]["textDocument"]["version"].as_i64().ok_or("missing document version")?;
                        let version = i32::try_from(local).ok().and_then(|v| v.checked_add(*base)).ok_or("LSP document version overflow")?;
                        versions.insert(uri.clone(),version);
                        value["params"]["textDocument"]["version"] = json!(version);
                    }
                    if method == "$/cancelRequest" {
                        let local = &value["params"]["id"];
                        let Some(request) = requests.iter().find_map(|(&request,(owner,key))| (*owner == id && key == local).then_some(request)) else { continue; };
                        requests.remove(&request);
                        value["params"]["id"] = json!(request);
                    } else if let Some(local) = value.get("id").cloned() {
                        if requests.len() >= 1024 { return Err("LSP request limit reached".into()); }
                        let request = next; next = next.checked_add(1).filter(|id| *id < i32::MAX as u64).ok_or("LSP request ID space exhausted")?;
                        requests.insert(request,(id,local)); value["id"] = json!(request);
                    }
                    connection.send(value).await?;
                }
            },
            message = connection.read() => {
                let mut message = message?;
                if message["method"] == "textDocument/publishDiagnostics" {
                    if let Some(host) = host {
                        publish_diagnostics(host, client, &message, encoding, attachments, documents, &connection.root_uri).await?;
                    }
                    let uri = message["params"]["uri"].as_str().unwrap_or("");
                    if let Some(attachment) = attachments.values().find(|a| a.document.as_ref().is_some_and(|(path,_)| path == uri)) {
                        let base = attachment.document.as_ref().unwrap().1;
                        if let Some(version) = message["params"]["version"].as_i64() {
                            if version <= i64::from(base) { continue; }
                            message["params"]["version"] = json!(version-i64::from(base));
                        }
                        deliver(attachment,message)?;
                    }
                } else if message.get("method").is_some() {
                    connection.handle_server_request(&message).await?;
                } else if message.get("id") == Some(&json!(1)) {
                    if let Ok(result) = serde_json::from_value::<InitializeResult>(message["result"].clone()) {
                        encoding = Encoding::negotiate(result.capabilities.position_encoding.as_ref())?;
                    }
                    initialization = Some(message.clone());
                    for (id,local) in waiting.drain(..) {
                        if let Some(attachment) = attachments.get(&id) { let mut result = message.clone(); result["id"] = local; deliver(attachment,result)?; }
                    }
                } else if let Some((id,local)) = message["id"].as_u64().and_then(|id| requests.remove(&id))
                    && let Some(attachment) = attachments.get(&id) {
                    message["id"] = local;
                    translate_versions(&mut message["result"], attachments);
                    deliver(attachment,message)?;
                }
            }
        }
    }
}
fn translate_versions(value: &mut Value, attachments: &BTreeMap<u64, Attachment>) {
    match value {
        Value::Array(values) => {
            for value in values {
                translate_versions(value, attachments);
            }
        }
        Value::Object(values) => {
            if let Some(document) = values.get_mut("textDocument")
                && let (Some(uri), Some(version)) =
                    (document["uri"].as_str(), document["version"].as_i64())
                && let Some(base) = attachments
                    .values()
                    .filter_map(|a| a.document.as_ref())
                    .find_map(|(path, base)| (path == uri).then_some(*base))
            {
                document["version"] = json!(version - i64::from(base));
            }
            for value in values.values_mut() {
                translate_versions(value, attachments);
            }
        }
        _ => {}
    }
}

async fn publish_diagnostics(
    host: &velk_plugin_sdk::Host,
    client: u64,
    message: &Value,
    encoding: Encoding,
    attachments: &BTreeMap<u64, Attachment>,
    documents: &Documents,
    root_uri: &str,
) -> Result<(), String> {
    use velk_plugin_sdk::host::DiagnosticPublication;
    let Ok(params) = serde_json::from_value::<PublishDiagnosticsParams>(message["params"].clone())
    else {
        return Ok(());
    };
    let Ok(path) = Url::parse(params.uri.as_str()).and_then(|uri| {
        uri.to_file_path()
            .map_err(|_| url::ParseError::RelativeUrlWithoutBase)
    }) else {
        return Ok(());
    };
    let root = Url::parse(root_uri)
        .ok()
        .and_then(|uri| uri.to_file_path().ok());
    let snapshot = attachments.iter().find_map(|(id, attachment)| {
        let (uri, base) = attachment.document.as_ref()?;
        if uri != params.uri.as_str() {
            return None;
        }
        let (snapshot, local) = documents.lock().unwrap().get(id)?.clone();
        (params.version == local.checked_add(*base)).then_some(snapshot)
    });
    // Versioned publications require their exact attached snapshot. Unversioned
    // workspace checks describe disk contents, never an arbitrary dirty revision.
    if params.version.is_some() && snapshot.is_none() {
        return Ok(());
    }
    let disk_path = path;
    let (path, disk) = tokio::task::spawn_blocking(move || {
        use std::io::Read;
        let path = disk_path.canonicalize().unwrap_or(disk_path);
        let disk = (|| {
            if root.as_ref().is_none_or(|root| !path.starts_with(root)) {
                return None;
            }
            if !std::fs::metadata(&path).ok()?.is_file() {
                return None;
            }
            let file = std::fs::File::open(&path).ok()?;
            if !file.metadata().ok()?.is_file() {
                return None;
            }
            let mut text = String::new();
            file.take(velk_plugin_sdk::host::MAX_FILE_BYTES as u64 + 1)
                .read_to_string(&mut text)
                .ok()?;
            (text.len() <= velk_plugin_sdk::host::MAX_FILE_BYTES)
                .then(|| text.replace("\r\n", "\n").replace('\r', "\n"))
        })();
        (path, disk)
    })
    .await
    .map_err(|e| e.to_string())?;
    let root = Url::parse(root_uri)
        .ok()
        .and_then(|uri| uri.to_file_path().ok());
    if root.is_none_or(|root| !path.starts_with(root)) {
        return Ok(());
    }
    let revision = snapshot.as_ref().map(|s| s.revision);
    let text = match snapshot {
        Some(snapshot) => snapshot.text.to_string(),
        None => match disk.as_ref() {
            Some(text) => text.clone(),
            None if params.diagnostics.is_empty() => String::new(),
            None => return Ok(()),
        },
    };
    if text.len() > velk_plugin_sdk::host::MAX_FILE_BYTES {
        return Ok(());
    }
    let rope = ropey::Rope::from_str(&text);
    let truncated = params.diagnostics.len() > 1000;
    let diagnostics = params
        .diagnostics
        .into_iter()
        .take(1000)
        .filter_map(|d| {
            let range = encoding.decode_range(&rope, d.range)?;
            let line = rope.char_to_line(range.start);
            Some(velk_plugin_sdk::host::Diagnostic {
                start: range.start,
                end: range.end,
                line,
                column: range.start - rope.line_to_char(line),
                severity: match d.severity {
                    Some(lsp_types::DiagnosticSeverity::ERROR) => {
                        velk_plugin_sdk::host::Severity::Error
                    }
                    Some(lsp_types::DiagnosticSeverity::WARNING) => {
                        velk_plugin_sdk::host::Severity::Warning
                    }
                    Some(lsp_types::DiagnosticSeverity::HINT) => {
                        velk_plugin_sdk::host::Severity::Hint
                    }
                    _ => velk_plugin_sdk::host::Severity::Information,
                },
                message: d.message.chars().take(1000).collect(),
                source: d.source,
                code: d.code.map(|code| match code {
                    lsp_types::NumberOrString::Number(n) => n.to_string(),
                    lsp_types::NumberOrString::String(s) => s,
                }),
            })
        })
        .collect();
    let request = bounded_publication(
        client,
        DiagnosticPublication {
            path,
            revision,
            disk_matches: disk.as_ref() == Some(&text),
            truncated,
            text,
            diagnostics,
        },
    )?;
    if let Some(request) = request {
        host.request(request)?;
    }
    Ok(())
}

fn bounded_publication(
    client: u64,
    publication: velk_plugin_sdk::host::DiagnosticPublication,
) -> Result<Option<velk_plugin_sdk::host::Request>, String> {
    use velk_plugin_sdk::host::Request;
    let mut request = Request::PublishDiagnostics {
        client,
        publication,
    };
    loop {
        if serde_json::to_vec(&request)
            .map_err(|e| e.to_string())?
            .len()
            <= velk_plugin_sdk::api::MAX_INPUT_BYTES
        {
            return Ok(Some(request));
        }
        let Request::PublishDiagnostics { publication, .. } = &mut request else {
            unreachable!()
        };
        if publication.diagnostics.len() <= 1 {
            return Ok(None);
        }
        publication
            .diagnostics
            .truncate(publication.diagnostics.len() / 2);
        publication.truncated = true;
    }
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    #[test]
    fn publications_fit_the_native_encoded_budget_and_report_truncation() {
        use velk_plugin_sdk::host::*;
        let publication = DiagnosticPublication {
            path: "/project/file.rs".into(),
            revision: None,
            disk_matches: true,
            text: "x".repeat(600_000),
            truncated: false,
            diagnostics: vec![
                Diagnostic {
                    start: 0,
                    end: 1,
                    line: 0,
                    column: 0,
                    severity: Severity::Error,
                    message: "warning".repeat(80),
                    source: None,
                    code: None
                };
                1000
            ],
        };
        let request = bounded_publication(1, publication).unwrap().unwrap();
        assert!(
            serde_json::to_vec(&request).unwrap().len() <= velk_plugin_sdk::api::MAX_INPUT_BYTES
        );
        let Request::PublishDiagnostics {
            mut publication, ..
        } = request
        else {
            panic!()
        };
        assert!(publication.truncated);
        assert!(!publication.diagnostics.is_empty());
        publication.text = "\\".repeat(velk_plugin_sdk::api::MAX_INPUT_BYTES);
        assert!(
            bounded_publication(1, publication).unwrap().is_none(),
            "oversized encoded source must not terminate the workspace client"
        );
    }
}
