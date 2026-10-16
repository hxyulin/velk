use super::*;

struct Pending {
    id: u64,
    revision: u64,
    deadline: Instant,
}
pub(super) struct Pull {
    identifier: Option<String>,
    result_id: Option<String>,
    items: Vec<lsp_types::Diagnostic>,
    pending: Option<Pending>,
    due: Option<Instant>,
    enabled: bool,
}
impl Pull {
    pub fn new(capabilities: &Value) -> Option<Self> {
        let provider = capabilities.get("diagnosticProvider")?.as_object()?;
        Some(Self {
            identifier: provider
                .get("identifier")
                .and_then(Value::as_str)
                .map(str::to_owned),
            result_id: None,
            items: Vec::new(),
            pending: None,
            due: Some(Instant::now()),
            enabled: true,
        })
    }
    pub async fn invalidate(&mut self, connection: &mut Connection) -> Result<(), String> {
        if let Some(pending) = self.pending.take() {
            connection
                .notify("$/cancelRequest", json!({"id":pending.id}))
                .await?;
        }
        if self.enabled {
            self.due = Some(Instant::now() + Duration::from_millis(150));
        }
        Ok(())
    }
    pub async fn tick(
        &mut self,
        connection: &mut Connection,
        next: &mut u64,
        document: &DocumentSnapshot,
    ) -> Result<(), String> {
        if self
            .pending
            .as_ref()
            .is_some_and(|p| Instant::now() >= p.deadline)
        {
            let pending = self.pending.take().unwrap();
            connection
                .notify("$/cancelRequest", json!({"id":pending.id}))
                .await?;
            velk_plugin_sdk::diagnostics::log!(
                velk_plugin_sdk::diagnostics::Level::Warn,
                "Document diagnostics request timed out: {}",
                document.path.display()
            );
        }
        if self.pending.is_some() || self.due.is_none_or(|due| Instant::now() < due) {
            return Ok(());
        }
        self.due = None;
        if *next >= i32::MAX as u64 {
            return Err("LSP request ID space exhausted".into());
        }
        let id = *next;
        *next += 1;
        let mut params = json!({"textDocument":{"uri":uri(&document.path)?}});
        if let Some(identifier) = &self.identifier {
            params["identifier"] = json!(identifier);
        }
        if let Some(result_id) = &self.result_id {
            params["previousResultId"] = json!(result_id);
        }
        connection
            .request(id, "textDocument/diagnostic", params)
            .await?;
        self.pending = Some(Pending {
            id,
            revision: document.revision,
            deadline: Instant::now() + Duration::from_secs(10),
        });
        Ok(())
    }
    pub fn matches(&self, message: &Value) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|p| message.get("id") == Some(&json!(p.id)))
    }
    pub fn receive(
        &mut self,
        message: &Value,
        revision: u64,
    ) -> Result<Option<Vec<lsp_types::Diagnostic>>, String> {
        let Some(pending) = self.pending.take() else {
            return Ok(None);
        };
        if pending.revision != revision {
            return Ok(None);
        }
        if let Some(error) = message.get("error") {
            match error["code"].as_i64() {
                Some(-32601) => {
                    self.enabled = false;
                    self.due = None;
                }
                Some(-32801) => self.due = Some(Instant::now() + Duration::from_millis(250)),
                _ => {}
            }
            return Err(format!("Document diagnostics failed: {error}"));
        }
        let report: lsp_types::DocumentDiagnosticReport =
            serde_json::from_value(message["result"].clone()).map_err(|e| e.to_string())?;
        match report {
            lsp_types::DocumentDiagnosticReport::Full(report) => {
                self.result_id = report.full_document_diagnostic_report.result_id;
                self.items = report
                    .full_document_diagnostic_report
                    .items
                    .into_iter()
                    .take(10_000)
                    .collect();
            }
            lsp_types::DocumentDiagnosticReport::Unchanged(report) => {
                if self.result_id.is_none() {
                    return Err("Unchanged diagnostics without a previous full report".into());
                }
                self.result_id = Some(report.unchanged_document_diagnostic_report.result_id);
            }
        }
        if self.result_id.as_ref().is_some_and(|id| id.len() > 4096) {
            self.result_id = None;
        }
        Ok(Some(self.items.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn pending(pull: &mut Pull, revision: u64) {
        pull.pending = Some(Pending {
            id: 9,
            revision,
            deadline: Instant::now() + Duration::from_secs(1),
        });
    }
    #[test]
    fn stale_invalid_unchanged_and_error_reports_never_replace_the_cache() {
        assert!(Pull::new(&json!({})).is_none());
        let mut pull = Pull::new(&json!({"diagnosticProvider": {}})).unwrap();
        pending(&mut pull, 1);
        assert!(
            pull.receive(
                &json!({"result":{"kind":"unchanged","resultId":"unknown"}}),
                1
            )
            .is_err()
        );
        pending(&mut pull, 1);
        assert!(
            pull.receive(
                &json!({"result":{"kind":"full","resultId":"old","items":[]}}),
                2
            )
            .unwrap()
            .is_none()
        );
        assert!(pull.result_id.is_none());
        pending(&mut pull, 2);
        assert_eq!(
            pull.receive(
                &json!({"result":{"kind":"full","resultId":"accepted","items":[]}}),
                2
            )
            .unwrap()
            .unwrap()
            .len(),
            0
        );
        pending(&mut pull, 2);
        assert!(
            pull.receive(
                &json!({"error":{"code":-32801,"message":"content changed"}}),
                2
            )
            .is_err()
        );
        assert!(pull.due.is_some());
        assert_eq!(pull.result_id.as_deref(), Some("accepted"));
        pending(&mut pull, 2);
        assert!(
            pull.receive(&json!({"error":{"code":-32601,"message":"unsupported"}}), 2)
                .is_err()
        );
        assert!(!pull.enabled);
    }
}
