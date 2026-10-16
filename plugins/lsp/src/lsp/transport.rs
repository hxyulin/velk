use serde_json::{Value, json};
use std::{ffi::OsStr, io, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufRead, AsyncRead, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::mpsc,
    task::JoinHandle,
};

pub(super) struct Connection {
    child: Child,
    stdin: ChildStdin,
    messages: mpsc::Receiver<Result<Value, String>>,
    reader: JoinHandle<()>,
    stderr: JoinHandle<()>,
    pub root_uri: String,
    pub document_uri: Option<String>,
    configuration: Value,
}
impl Connection {
    pub fn spawn(
        executable: &OsStr,
        arguments: &[std::ffi::OsString],
        root: &Path,
        configuration: Value,
    ) -> Result<Self, String> {
        let _trace = velk_plugin_sdk::diagnostics::scope!("lsp.spawn_server");
        let mut child = Command::new(executable)
            .args(arguments)
            .current_dir(root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("Cannot start {}: {e}", executable.to_string_lossy()))?;
        let stdin = child
            .stdin
            .take()
            .ok_or("language server stdin is unavailable")?;
        let stdout = child
            .stdout
            .take()
            .ok_or("language server stdout is unavailable")?;
        let stderr = child
            .stderr
            .take()
            .ok_or("language server stderr is unavailable")?;
        let server = executable.to_string_lossy().into_owned();
        let stderr = tokio::spawn(async move {
            let result = read_stderr(stderr, |line| {
                velk_plugin_sdk::diagnostics::log(
                    velk_plugin_sdk::diagnostics::Level::Info,
                    "velk_plugin_lsp::server",
                    format_args!("{server}: {line}"),
                );
            })
            .await;
            if let Err(error) = result {
                velk_plugin_sdk::diagnostics::log!(
                    velk_plugin_sdk::diagnostics::Level::Warn,
                    "Cannot read {server} stderr: {error}"
                );
            }
        });
        let (send, messages) = mpsc::channel(32);
        let reader = tokio::spawn(async move {
            let mut stdout = BufReader::new(stdout);
            loop {
                let message = read_frame(&mut stdout)
                    .await
                    .map_err(|e| format!("LSP connection: {e}"));
                let failed = message.is_err();
                if send.send(message).await.is_err() || failed {
                    break;
                }
            }
        });
        Ok(Self {
            child,
            stdin,
            messages,
            reader,
            stderr,
            root_uri: String::new(),
            document_uri: None,
            configuration,
        })
    }
    pub async fn read(&mut self) -> Result<Value, String> {
        let _trace = velk_plugin_sdk::diagnostics::async_scope!("lsp.wait_message");
        self.messages
            .recv()
            .await
            .ok_or_else(|| "language server disconnected".to_string())?
    }
    async fn send(&mut self, message: Value) -> Result<(), String> {
        let _trace = velk_plugin_sdk::diagnostics::async_scope!("lsp.write_message");
        let bytes = {
            let _cpu = velk_plugin_sdk::diagnostics::scope!("lsp.encode");
            serde_json::to_vec(&message).map_err(|e| e.to_string())?
        };
        self.stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", bytes.len()).as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        self.stdin
            .write_all(&bytes)
            .await
            .map_err(|e| e.to_string())?;
        self.stdin.flush().await.map_err(|e| e.to_string())
    }
    pub async fn request(&mut self, id: u64, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))
            .await
    }
    pub async fn notify(&mut self, method: &str, params: Value) -> Result<(), String> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
            .await
    }
    pub async fn handle_server_request(&mut self, message: &Value) -> Result<(), String> {
        let Some(id) = message.get("id") else {
            return Ok(());
        };
        let method = message["method"].as_str().unwrap_or("");
        let result = match method {
            "workspace/configuration" => Value::Array(message["params"]["items"].as_array().into_iter().flatten().map(|item| {
                let section = item["section"].as_str().unwrap_or("");
                if section.is_empty() { return self.configuration.clone(); }
                section.split('.').fold(&self.configuration,|value,key| &value[key]).clone()
            }).collect()),
            "workspace/workspaceFolders" => json!([{"uri":self.root_uri,"name":"workspace"}]),
            "window/workDoneProgress/create" | "window/showMessageRequest" => Value::Null,
            "workspace/applyEdit" => json!({"applied":false,"failureReason":"Workspace edits are not supported yet"}),
            _ => return self.send(json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":format!("Unsupported server request: {method}")}})).await,
        };
        self.send(json!({"jsonrpc":"2.0","id":id,"result":result}))
            .await
    }
    pub async fn shutdown(&mut self) {
        let _trace = velk_plugin_sdk::diagnostics::async_scope!("lsp.shutdown");
        let _ = tokio::time::timeout(Duration::from_secs(2), async {
            if let Some(uri) = self.document_uri.take() {
                self.notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
                    .await?;
            }
            let id = i32::MAX as u64;
            self.request(id, "shutdown", Value::Null).await?;
            loop {
                let message = self.read().await?;
                if message.get("id") == Some(&json!(id)) {
                    break;
                }
                self.handle_server_request(&message).await?;
            }
            self.notify("exit", Value::Null).await
        })
        .await;
        if !matches!(
            tokio::time::timeout(Duration::from_secs(1), self.child.wait()).await,
            Ok(Ok(_))
        ) {
            let _ = self.child.kill().await;
        }
        self.reader.abort();
        if tokio::time::timeout(Duration::from_secs(1), &mut self.stderr)
            .await
            .is_err()
        {
            self.stderr.abort();
            let _ = (&mut self.stderr).await;
        }
    }
}
impl Drop for Connection {
    fn drop(&mut self) {
        self.reader.abort();
        self.stderr.abort();
    }
}

async fn read_stderr(
    mut reader: impl AsyncRead + Unpin,
    mut emit: impl FnMut(&str),
) -> io::Result<()> {
    let mut bytes = [0; 4096];
    let mut line = Vec::new();
    let mut truncated = false;
    loop {
        let count = reader.read(&mut bytes).await?;
        if count == 0 {
            if !line.is_empty() {
                emit(&String::from_utf8_lossy(&line));
            }
            return Ok(());
        }
        for &byte in &bytes[..count] {
            if byte == b'\n' {
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                if !truncated {
                    emit(&String::from_utf8_lossy(&line));
                }
                line.clear();
                truncated = false;
            } else if !truncated {
                line.push(byte);
                if line.len() == 4096 {
                    emit(&String::from_utf8_lossy(&line));
                    line.clear();
                    truncated = true;
                }
            }
        }
    }
}

async fn read_frame(reader: &mut (impl AsyncBufRead + Unpin)) -> io::Result<Value> {
    let mut length = None;
    let mut total = 0;
    loop {
        let mut line = Vec::new();
        loop {
            let byte = reader.read_u8().await?;
            total += 1;
            if total > 8192 {
                return Err(io::Error::other("LSP headers exceed 8 KiB"));
            }
            line.push(byte);
            if byte == b'\n' {
                break;
            }
        }
        if line == b"\r\n" {
            break;
        }
        let line = std::str::from_utf8(&line).map_err(io::Error::other)?;
        let (name, value) = line
            .trim_end()
            .split_once(':')
            .ok_or_else(|| io::Error::other("invalid LSP header"))?;
        if name.eq_ignore_ascii_case("Content-Length") {
            if length.is_some() {
                return Err(io::Error::other("duplicate Content-Length"));
            }
            length = Some(value.trim().parse::<usize>().map_err(io::Error::other)?);
        }
    }
    let length = length
        .filter(|&n| n <= 64 * 1024 * 1024)
        .ok_or_else(|| io::Error::other("missing or excessive LSP Content-Length"))?;
    let mut body = vec![0; length];
    reader.read_exact(&mut body).await?;
    let _cpu = velk_plugin_sdk::diagnostics::scope!("lsp.decode");
    let message: Value = serde_json::from_slice(&body).map_err(io::Error::other)?;
    if message["jsonrpc"] != "2.0" || !message.is_object() {
        return Err(io::Error::other("invalid JSON-RPC envelope"));
    }
    Ok(message)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn stderr_drains_oversized_lines_and_preserves_following_messages() {
        let input = format!("first\r\n{}\nlast", "x".repeat(20000));
        let mut lines = Vec::new();
        read_stderr(input.as_bytes(), |line| lines.push(line.to_owned()))
            .await
            .unwrap();
        assert_eq!(lines, ["first", &"x".repeat(4096), "last"]);
        let mut lines = Vec::new();
        read_stderr(&b"bad: \xff\n"[..], |line| lines.push(line.to_owned()))
            .await
            .unwrap();
        assert_eq!(lines, ["bad: \u{fffd}"]);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn framing_accepts_byte_lengths_and_extra_headers_and_rejects_bad_frames() {
        let message = json!({"jsonrpc":"2.0","method":"test","params":"世界👩‍💻"});
        let body = serde_json::to_vec(&message).unwrap();
        let mut frame = format!(
            "content-length: {}\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n",
            body.len()
        )
        .into_bytes();
        frame.extend(body);
        assert_eq!(
            read_frame(&mut BufReader::new(frame.as_slice()))
                .await
                .unwrap(),
            message
        );
        for bad in [
            b"\r\n".as_slice(),
            b"Content-Length: 1\r\nContent-Length: 1\r\n\r\n0",
            b"Content-Length: 999999999\r\n\r\n",
            b"Content-Length: 5\r\n\r\n{}",
            b"Content-Length: 2\r\n\r\n{}",
        ] {
            assert!(read_frame(&mut BufReader::new(bad)).await.is_err());
        }
        assert!(
            read_frame(&mut BufReader::new(vec![b'a'; 8193].as_slice()))
                .await
                .is_err()
        );
    }
}
