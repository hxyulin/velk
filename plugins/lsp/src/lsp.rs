//! LSP adapter. Wire types, URIs, encoding negotiation, and child processes stay
//! here; the editor and other semantic backends do not depend on them.
mod commands;
mod edits;
mod positions;
mod transport;
use self::{positions::Encoding, transport::Connection};
use crate::{
    Backend, BackendFuture, Capabilities, Diagnostic, DocumentSnapshot, Event, Hover, Input,
    PositionRequest, Session, Severity, Status, text::difference,
};
use lsp_types::{
    HoverContents, InitializeResult, MarkedString, PublishDiagnosticsParams,
    TextDocumentSyncCapability, TextDocumentSyncKind,
};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::{Instant, interval};
use url::Url;

pub struct LspBackend {
    pub executable: OsString,
    pub arguments: Vec<OsString>,
    pub initialization_options: Value,
    /// Full settings object addressed by workspace/configuration sections.
    pub configuration: Value,
    pub root: Option<PathBuf>,
}
impl Backend for LspBackend {
    fn run(self: Box<Self>, session: Session) -> BackendFuture {
        Box::pin(async move {
            let _trace = velk_plugin_sdk::diagnostics::async_scope!("lsp.session");
            let first = loop {
                match session.next().await {
                    Some(Input::Document(document)) => break document,
                    Some(_) => continue,
                    None => return Ok(()),
                }
            };
            let root = self
                .root
                .clone()
                .unwrap_or_else(|| project_root(&first.path));
            let mut connection = Connection::spawn(
                &self.executable,
                &self.arguments,
                &root,
                self.configuration.clone(),
            )?;
            let result = tokio::select! {
                result = drive(&mut connection, &session, first, &root, self.initialization_options) => result,
                _ = session.cancelled() => Ok(()),
            };
            connection.shutdown().await;
            result
        })
    }
}

fn project_root(path: &Path) -> PathBuf {
    let parent = path.parent().unwrap_or(Path::new("."));
    parent
        .ancestors()
        .find(|p| p.join("Cargo.toml").is_file())
        .unwrap_or(parent)
        .to_path_buf()
}
fn uri(path: &Path) -> Result<String, String> {
    Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| "LSP requires an absolute file path".into())
}

async fn drive(
    connection: &mut Connection,
    session: &Session,
    first: DocumentSnapshot,
    root: &Path,
    options: Value,
) -> Result<(), String> {
    let root_uri = uri(root)?;
    let initialize = json!({
        "processId":std::process::id(), "clientInfo":{"name":"Velk","version":env!("CARGO_PKG_VERSION")},
        "rootUri":root_uri, "workspaceFolders":[{"uri":root_uri,"name":root.file_name().unwrap_or_default().to_string_lossy()}],
        "capabilities": {
            "general":{"positionEncodings":["utf-8","utf-16","utf-32"]},
            "workspace":{"configuration":true,"workspaceFolders":true,"applyEdit":false,"workspaceEdit":{"documentChanges":true,"failureHandling":"abort"}},
            "textDocument":{
                "synchronization":{"dynamicRegistration":false,"didSave":true},
                "hover":{"dynamicRegistration":false,"contentFormat":["markdown","plaintext"]},
                "formatting":{"dynamicRegistration":false}, "rename":{"dynamicRegistration":false},
                "codeAction":{"dynamicRegistration":false,"codeActionLiteralSupport":{"codeActionKind":{"valueSet":["quickfix","refactor","source"]}},"resolveSupport":{"properties":["edit"]}},
                "completion":{"dynamicRegistration":false,"completionItem":{"snippetSupport":false,"insertReplaceSupport":true,"resolveSupport":{"properties":["additionalTextEdits"]}}},
                "publishDiagnostics":{"versionSupport":true,"relatedInformation":false}
            }
        }, "initializationOptions":options
    });
    connection.root_uri = root_uri;
    let initialize_trace = velk_plugin_sdk::diagnostics::async_scope!("lsp.initialize");
    connection.request(1, "initialize", initialize).await?;
    let initialized = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let message = connection.read().await?;
            if message.get("id") == Some(&json!(1)) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    return Err(format!("LSP initialization failed: {error}"));
                }
                return serde_json::from_value::<InitializeResult>(message["result"].clone())
                    .map_err(|e| e.to_string());
            }
            connection.handle_server_request(&message).await?;
        }
    })
    .await
    .map_err(|_| "LSP initialization timed out".to_string())??;
    drop(initialize_trace);
    let server_capabilities =
        serde_json::to_value(&initialized.capabilities).map_err(|e| e.to_string())?;
    let encoding = Encoding::negotiate(initialized.capabilities.position_encoding.as_ref())?;
    let capabilities = Capabilities {
        diagnostics: true,
        hover: initialized
            .capabilities
            .hover_provider
            .as_ref()
            .is_some_and(|cap| match cap {
                lsp_types::HoverProviderCapability::Simple(value) => *value,
                lsp_types::HoverProviderCapability::Options(_) => true,
            }),
    };
    let (sync, save) = match initialized.capabilities.text_document_sync {
        Some(TextDocumentSyncCapability::Kind(kind)) => (kind, None),
        Some(TextDocumentSyncCapability::Options(options)) => (
            options.change.unwrap_or(TextDocumentSyncKind::NONE),
            options.save,
        ),
        None => (TextDocumentSyncKind::NONE, None),
    };
    connection.notify("initialized", json!({})).await?;
    let mut document = first;
    let document_uri = uri(&document.path)?;
    connection.document_uri = Some(document_uri.clone());
    let mut version = 1i32;
    connection.notify("textDocument/didOpen", json!({"textDocument":{"uri":document_uri,"languageId":document.language,"version":version,"text":document.text.to_string()}})).await?;
    session
        .events()
        .publish(Event::Status(Status::Ready(capabilities)));
    let mut diagnostic_context = Vec::new();
    let mut command: Option<(u64, PositionRequest, commands::Request, Instant)> = None;
    let mut pending: Option<(u64, PositionRequest, Instant)> = None;
    let mut hover_trace = None;
    let mut next_id = 2u64;
    let mut tick = interval(Duration::from_millis(250));
    loop {
        if pending.is_none() {
            drop(hover_trace.take());
        }
        tokio::select! {
            input = session.next() => match input {
                None => return Ok(()),
                Some(Input::Document(next)) => {
                    if next.path != document.path || next.language != document.language { return Err("semantic service document identity changed".into()); }
                    if next.revision <= document.revision { continue; }
                    cancel_hover(connection, session, &mut pending, None).await?;
                    cancel_command(connection, session, &mut command, "Source changed; command cancelled").await?;
                    if sync == TextDocumentSyncKind::NONE {
                        return Err("language server does not support document changes".into());
                    }
                    let change = if sync == TextDocumentSyncKind::INCREMENTAL {
                        difference(&document.text, &next.text).map(|(range,text)| json!({"range":encoding.range(&document.text, range),"text":text}))
                    } else { Some(json!({"text":next.text.to_string()})) };
                    // Equal-content undo/redo still changes identity. An empty
                    // incremental edit advances the version without copying text.
                    let change = change.unwrap_or_else(|| json!({"range":encoding.range(&document.text,0..0),"text":""}));
                    version = version.checked_add(1).ok_or("LSP document version overflow")?;
                    connection.notify("textDocument/didChange", json!({"textDocument":{"uri":document_uri,"version":version},"contentChanges":[change]})).await?;
                    document = next;
                    diagnostic_context.clear();
                }
                Some(Input::Saved(saved)) => {
                    if saved.path != document.path { continue; }
                    if let Some(ref support) = save {
                        let include = matches!(support, lsp_types::TextDocumentSyncSaveOptions::SaveOptions(options) if options.include_text == Some(true));
                        if !matches!(support, lsp_types::TextDocumentSyncSaveOptions::Supported(false)) {
                            let mut params = json!({"textDocument":{"uri":document_uri}});
                            if include { params["text"] = json!(saved.text.to_string()); }
                            connection.notify("textDocument/didSave", params).await?;
                        }
                    }
                }
                Some(Input::Command { request, payload, input }) => {
                    if request.revision != document.revision || request.position > document.text.len_chars() {
                        session.events().publish(Event::Command { request, result:Err("command source is stale".into()) }); continue;
                    }
                    let spec = match commands::Request::decode(payload, &server_capabilities) {
                        Ok(spec) => spec,
                        Err(error) => { session.events().publish(Event::Command { request, result:Err(error) }); continue; }
                    };
                    if command.is_some() { session.events().publish(Event::Command { request, result:Err("another command is running".into()) }); continue; }
                    if next_id >= i32::MAX as u64 { return Err("LSP request ID space exhausted".into()); }
                    let id = next_id; next_id += 1;
                    let params=match spec.params(&document_uri, encoding.position(&document.text, request.position), input.as_deref(), &diagnostic_context) {
                        Ok(params)=>params,Err(error)=>{session.events().publish(Event::Command {request,result:Err(error)});continue;}
                    };
                    connection.request(id, &spec.method, params).await?;
                    command = Some((id, request, spec, Instant::now()+Duration::from_secs(10)));
                }
                Some(Input::CancelHover) => {
                    cancel_hover(connection, session, &mut pending, None).await?;
                }
                Some(Input::Hover(request)) => {
                    cancel_hover(connection, session, &mut pending, None).await?;
                    if request.revision != document.revision || request.position > document.text.len_chars() || !capabilities.hover {
                        session.events().publish(Event::Hover { request, hover:None }); continue;
                    }
                    if next_id >= i32::MAX as u64 { return Err("LSP request ID space exhausted".into()); }
                    let id = next_id; next_id += 1;
                    hover_trace = Some(velk_plugin_sdk::diagnostics::async_scope!("lsp.hover.round_trip"));
                    connection.request(id,"textDocument/hover", json!({"textDocument":{"uri":document_uri},"position":encoding.position(&document.text, request.position)})).await?;
                    pending = Some((id,request,Instant::now()+Duration::from_secs(5)));
                }
            },
            message = connection.read() => {
                let message = message?;
                if message.get("method").is_some() {
                    if message["method"] == "textDocument/publishDiagnostics" {
                        if let Ok(params) = serde_json::from_value::<PublishDiagnosticsParams>(message["params"].clone()) {
                            if params.uri.as_str() != document_uri || !(params.version == Some(version) || params.version.is_none() && version == 1) { continue; }
                            diagnostic_context = params.diagnostics.clone();
                            let diagnostics = params.diagnostics.into_iter().take(10_000).filter_map(|d| {
                                let range = encoding.decode_range(&document.text,d.range)?;
                                Some(Diagnostic { range, severity: match d.severity {
                                    Some(lsp_types::DiagnosticSeverity::ERROR) => Severity::Error,
                                    Some(lsp_types::DiagnosticSeverity::WARNING) => Severity::Warning,
                                    Some(lsp_types::DiagnosticSeverity::HINT) => Severity::Hint,
                                    _ => Severity::Information,
                                }, message:d.message, source:d.source, code:d.code.map(|code| match code { lsp_types::NumberOrString::Number(n) => n.to_string(), lsp_types::NumberOrString::String(s) => s }) })
                            }).collect();
                            session.events().publish(Event::Diagnostics { revision:document.revision, diagnostics });
                        }
                    } else { connection.handle_server_request(&message).await?; }
                } else if command.as_ref().is_some_and(|(id,_,_,_)| message.get("id") == Some(&json!(id))) {
                    let (_, request, spec, _) = command.take().unwrap();
                    let result = if let Some(error) = message.get("error") { Err(format!("Command failed: {error}")) }
                        else { spec.outcome(message["result"].clone(), encoding, &document, version, request, &server_capabilities) };
                    session.events().publish(Event::Command { request, result });
                } else if let Some((id,request,_)) = pending {
                    if message.get("id") != Some(&json!(id)) { continue; }
                    pending = None;
                    drop(hover_trace.take());
                    if let Some(error) = message.get("error") {
                        session.events().publish(Event::RequestFailed { request, message:format!("Hover failed: {error}") });
                    } else if message["result"].is_null() {
                        session.events().publish(Event::Hover {request,hover:None});
                    } else {
                        match serde_json::from_value::<lsp_types::Hover>(message["result"].clone()) {
                            Ok(result) => {
                                let (contents, markdown) = hover_text(result.contents);
                                let range = result.range.and_then(|range| encoding.decode_range(&document.text,range));
                                session.events().publish(Event::Hover { request, hover:Some(Hover { request, contents, markdown, range }) });
                            }
                            Err(error) => session.events().publish(Event::RequestFailed { request, message:error.to_string() }),
                        }
                    }
                }
            },
            _ = tick.tick() => {
                if command.as_ref().is_some_and(|(_,_,_,deadline)| Instant::now() >= *deadline) {
                    cancel_command(connection, session, &mut command, "Command timed out").await?;
                }
                if pending.is_some_and(|(_,_,deadline)| Instant::now() >= deadline) {
                    cancel_hover(connection, session, &mut pending, Some("Hover request timed out")).await?;
                }
            }
        }
    }
}
async fn cancel_hover(
    connection: &mut Connection,
    session: &Session,
    pending: &mut Option<(u64, PositionRequest, Instant)>,
    reason: Option<&str>,
) -> Result<(), String> {
    if let Some((id, request, _)) = pending.take() {
        connection
            .notify("$/cancelRequest", json!({"id":id}))
            .await?;
        if let Some(message) = reason {
            session.events().publish(Event::RequestFailed {
                request,
                message: message.into(),
            });
        }
    }
    Ok(())
}
fn hover_text(contents: HoverContents) -> (String, bool) {
    let marked = |s: MarkedString| match s {
        MarkedString::String(s) => s,
        MarkedString::LanguageString(s) => {
            // Pick a fence longer than any backtick run in server content.
            let longest = s.value.split(|c| c != '`').map(str::len).max().unwrap_or(0);
            let fence = "`".repeat(longest.max(2) + 1);
            let language = s.language.replace(['\n', '\r', '`'], "");
            format!("{fence}{language}\n{}\n{fence}", s.value)
        }
    };
    match contents {
        HoverContents::Markup(markup) => {
            (markup.value, markup.kind == lsp_types::MarkupKind::Markdown)
        }
        HoverContents::Scalar(s) => (marked(s), true),
        HoverContents::Array(strings) => (
            strings
                .into_iter()
                .map(marked)
                .collect::<Vec<_>>()
                .join("\n\n"),
            true,
        ),
    }
}

async fn cancel_command(
    connection: &mut Connection,
    session: &Session,
    pending: &mut Option<(u64, PositionRequest, commands::Request, Instant)>,
    reason: &str,
) -> Result<(), String> {
    if let Some((id, request, _, _)) = pending.take() {
        connection
            .notify("$/cancelRequest", json!({"id":id}))
            .await?;
        session.events().publish(Event::Command {
            request,
            result: Err(reason.into()),
        });
    }
    Ok(())
}

#[cfg(test)]
mod hover_markup_tests {
    use super::*;
    #[test]
    fn preserves_markup_format_and_fences_legacy_language_strings() {
        let (text, markdown) = hover_text(HoverContents::Scalar(MarkedString::LanguageString(
            lsp_types::LanguageString {
                language: "rust".into(),
                value: "let fence = \"```\";".into(),
            },
        )));
        assert!(markdown);
        assert!(text.starts_with("````rust\n"));
        assert!(text.ends_with("\n````"));
        for (kind, expected) in [
            (lsp_types::MarkupKind::PlainText, false),
            (lsp_types::MarkupKind::Markdown, true),
        ] {
            assert_eq!(
                hover_text(HoverContents::Markup(lsp_types::MarkupContent {
                    kind,
                    value: "**same**".into()
                })),
                ("**same**".into(), expected)
            );
        }
    }
}
