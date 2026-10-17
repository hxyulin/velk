//! Backend-owned command payloads and LSP result conversion.
use super::{positions::Encoding, *};
use velk_plugin_sdk::commands::{Encoding as TargetEncoding, Location, Outcome, Position};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub method: String,
    pub result: ResultKind,
    pub capability: Option<String>,
    pub parameters: Option<Value>,
}
#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ResultKind {
    Locations,
    MacroExpansion,
    TextEdits,
    WorkspaceEdit,
    CodeActions,
    ResolvedAction,
    Completion,
    ResolvedCompletion,
    CompletionDocumentation,
    SignatureHelp,
}
impl Request {
    pub fn decode(payload: Value, capabilities: &Value) -> Result<Self, String> {
        let request: Self =
            serde_json::from_value(payload).map_err(|e| format!("invalid LSP command: {e}"))?;
        if request.method.is_empty()
            || request.method.len() > 128
            || request.method.chars().any(char::is_control)
        {
            return Err("invalid LSP method".into());
        }
        if let Some(capability) = &request.capability {
            match capabilities.pointer(capability) {
                Some(Value::Bool(true) | Value::Object(_)) => {}
                _ => return Err("the language server does not support this command".into()),
            }
        }
        Ok(request)
    }
    pub fn completion_trigger_supported(
        &self,
        input: Option<&str>,
        capabilities: &Value,
    ) -> Result<bool, String> {
        if self.method != "textDocument/completion" {
            return Ok(true);
        }
        let Some(velk_completion_contracts::Invocation::Character { character }) =
            velk_completion_contracts::Invocation::decode(input)?
        else {
            return Ok(true);
        };
        Ok(character.chars().count() == 1
            && capabilities
                .pointer("/completionProvider/triggerCharacters")
                .and_then(Value::as_array)
                .is_some_and(|characters| {
                    characters
                        .iter()
                        .any(|value| value.as_str() == Some(&character))
                }))
    }
    pub fn params(
        &self,
        document_uri: &str,
        position: lsp_types::Position,
        input: Option<&str>,
        diagnostics: &[lsp_types::Diagnostic],
    ) -> Result<Value, String> {
        if let Some(parameters) = &self.parameters {
            return Ok(parameters.clone());
        }
        let mut params = json!({"textDocument":{"uri":document_uri},"position":position});
        match self.method.as_str() {
            "textDocument/references" => params["context"] = json!({"includeDeclaration":true}),
            "textDocument/formatting" => {
                params = json!({"textDocument":{"uri":document_uri},"options":{"tabSize":4,"insertSpaces":true}});
            }
            "textDocument/rename" => {
                let input = input
                    .filter(|s| {
                        !s.trim().is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control)
                    })
                    .ok_or("Enter a nonempty new name")?;
                params["newName"] = json!(input);
            }
            "textDocument/codeAction" => {
                let diagnostics: Vec<_> = diagnostics
                    .iter()
                    .filter(|d| d.range.start <= position && position <= d.range.end)
                    .collect();
                params = json!({"textDocument":{"uri":document_uri},"range":{"start":position,"end":position},"context":{"diagnostics":diagnostics,"triggerKind":1}});
            }
            "textDocument/completion" => {
                params["context"] = if velk_completion_contracts::incomplete_refresh(input)? {
                    json!({"triggerKind":3})
                } else {
                    match velk_completion_contracts::Invocation::decode(input)? {
                        Some(velk_completion_contracts::Invocation::Character { character }) => {
                            json!({"triggerKind":2,"triggerCharacter":character})
                        }
                        _ => json!({"triggerKind":1}),
                    }
                };
            }
            _ => {}
        }
        Ok(params)
    }
    pub fn outcome(
        &self,
        value: Value,
        encoding: Encoding,
        source: (&DocumentSnapshot, i32),
        request: PositionRequest,
        capabilities: &Value,
        documents: &[(DocumentSnapshot, i32)],
    ) -> Result<Outcome, String> {
        let (source, version) = source;
        if value.is_null() {
            if matches!(self.result, ResultKind::SignatureHelp) {
                return Ok(Outcome::SignatureHelp { help: None });
            }
            if matches!(self.result, ResultKind::Completion) {
                return Ok(Outcome::Completions {
                    items: vec![],
                    continuation: None,
                });
            }
            return Ok(Outcome::Message {
                text: "No result at the caret".into(),
            });
        }
        let outcome = match self.result {
            ResultKind::TextEdits => {
                let edits: Vec<lsp_types::TextEdit> =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                Outcome::Edit {
                    edit: velk_plugin_sdk::commands::WorkspaceEdit {
                        documents: vec![super::edits::document(
                            source.path.clone(),
                            Some(source.revision),
                            edits,
                            encoding,
                        )],
                    },
                }
            }
            ResultKind::WorkspaceEdit => Outcome::Edit {
                edit: super::edits::workspace(value, source, version, encoding, documents)?,
            },
            ResultKind::CodeActions => {
                code_actions(value, encoding, source, version, capabilities, documents)?
            }
            ResultKind::ResolvedAction => {
                resolved_action(value, encoding, source, version, documents)?
            }
            ResultKind::SignatureHelp => signature_help(value)?,
            ResultKind::Completion => completion(value, encoding, source, request, capabilities)?,
            ResultKind::CompletionDocumentation => {
                let item: lsp_types::CompletionItem =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                let documentation = completion_documentation(item.documentation);
                Outcome::Document {
                    title: item.label,
                    text: documentation
                        .as_ref()
                        .map(|doc| doc.text.clone())
                        .unwrap_or_default(),
                    language: documentation
                        .filter(|doc| doc.markdown)
                        .map(|_| "markdown".into()),
                }
            }
            ResultKind::ResolvedCompletion => {
                let item = serde_json::from_value(value).map_err(|e| e.to_string())?;
                match completion_action(item, encoding, source, request)? {
                    velk_plugin_sdk::commands::ChoiceAction::Edit { edit } => {
                        Outcome::Edit { edit }
                    }
                    velk_plugin_sdk::commands::ChoiceAction::Snippet { snippet } => {
                        Outcome::Snippet { snippet }
                    }
                    _ => unreachable!("completion edit action"),
                }
            }
            ResultKind::Locations => {
                let values = match value {
                    Value::Array(items) => items,
                    value => vec![value],
                };
                if values.len() > 10_000 {
                    return Err("too many navigation targets".into());
                }
                let mut locations = Vec::new();
                for value in values {
                    let (uri, position) = if value.get("targetUri").is_some() {
                        let link: lsp_types::LocationLink =
                            serde_json::from_value(value).map_err(|e| e.to_string())?;
                        (link.target_uri, link.target_selection_range.start)
                    } else {
                        let location: lsp_types::Location =
                            serde_json::from_value(value).map_err(|e| e.to_string())?;
                        (location.uri, location.range.start)
                    };
                    let path = Url::parse(uri.as_str())
                        .map_err(|e| e.to_string())?
                        .to_file_path()
                        .map_err(|_| "navigation target is not a local file")?;
                    let location = Location {
                        path,
                        position: Position {
                            line: position.line,
                            column: position.character,
                            encoding: match encoding {
                                Encoding::Utf8 => TargetEncoding::Utf8,
                                Encoding::Utf16 => TargetEncoding::Utf16,
                                Encoding::Utf32 => TargetEncoding::Utf32,
                            },
                        },
                    };
                    if !locations.contains(&location) {
                        locations.push(location);
                    }
                }
                Outcome::Locations { locations }
            }
            ResultKind::MacroExpansion => {
                #[derive(serde::Deserialize)]
                struct Expansion {
                    name: String,
                    expansion: String,
                }
                let expansion: Expansion =
                    serde_json::from_value(value).map_err(|e| e.to_string())?;
                Outcome::Document {
                    title: format!("Expansion of {}", expansion.name),
                    text: expansion.expansion,
                    language: Some("rust".into()),
                }
            }
        };
        outcome.validate()?;
        Ok(outcome)
    }
}

fn resolved_action(
    value: Value,
    encoding: Encoding,
    source: &DocumentSnapshot,
    version: i32,
    documents: &[(DocumentSnapshot, i32)],
) -> Result<Outcome, String> {
    let action: lsp_types::CodeAction = serde_json::from_value(value).map_err(|e| e.to_string())?;
    if let Some(disabled) = action.disabled {
        return Err(disabled.reason);
    }
    if action.command.is_some() {
        return Err("This action requires server command execution, which is not supported".into());
    }
    Ok(Outcome::Edit {
        edit: super::edits::workspace(
            serde_json::to_value(action.edit.ok_or("Action returned no edits")?)
                .map_err(|e| e.to_string())?,
            source,
            version,
            encoding,
            documents,
        )?,
    })
}
fn code_actions(
    value: Value,
    encoding: Encoding,
    source: &DocumentSnapshot,
    version: i32,
    capabilities: &Value,
    documents: &[(DocumentSnapshot, i32)],
) -> Result<Outcome, String> {
    use velk_plugin_sdk::commands::{Choice, ChoiceAction};
    let actions: Vec<lsp_types::CodeActionOrCommand> =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    let mut items = Vec::new();
    for action in actions.into_iter().take(1000) {
        let lsp_types::CodeActionOrCommand::CodeAction(action) = action else {
            continue;
        };
        if action.disabled.is_some() || action.command.is_some() {
            continue;
        }
        let title = action.title.clone();
        let handler = if action.edit.is_some() {
            let Outcome::Edit { edit } = resolved_action(
                serde_json::to_value(&action).map_err(|e| e.to_string())?,
                encoding,
                source,
                version,
                documents,
            )?
            else {
                unreachable!()
            };
            ChoiceAction::Edit { edit }
        } else if capabilities.pointer("/codeActionProvider/resolveProvider") == Some(&json!(true))
        {
            ChoiceAction::Semantic {
                payload: json!({"method":"codeAction/resolve","result":"resolved_action","capability":null,"parameters":action}),
            }
        } else {
            continue;
        };
        if let ChoiceAction::Semantic { payload } = &handler
            && serde_json::to_vec(payload)
                .map_err(|e| e.to_string())?
                .len()
                > 12 * 1024
        {
            continue;
        }
        items.push(Choice {
            title,
            detail: action.kind.map(|k| k.as_str().to_string()),
            action: handler,
        });
    }
    Ok(Outcome::Choices {
        title: "Code actions".into(),
        items,
    })
}
fn completion(
    value: Value,
    encoding: Encoding,
    source: &DocumentSnapshot,
    request: PositionRequest,
    capabilities: &Value,
) -> Result<Outcome, String> {
    use velk_plugin_sdk::commands::{Choice, ChoiceAction};
    let response: lsp_types::CompletionResponse =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    let (mut completions, incomplete) = match response {
        lsp_types::CompletionResponse::Array(items) => (items, false),
        lsp_types::CompletionResponse::List(list) => (list.items, list.is_incomplete),
    };
    completions.sort_by(|a, b| {
        a.sort_text
            .as_ref()
            .unwrap_or(&a.label)
            .cmp(b.sort_text.as_ref().unwrap_or(&b.label))
    });
    let mut items = Vec::new();
    let variables = snippet_variables(source, request.position);
    for item in completions.into_iter().take(1000) {
        if item.command.is_some() {
            continue;
        }
        let commit_characters = item
            .commit_characters
            .as_ref()
            .map(|values| {
                values
                    .iter()
                    .filter_map(|value| single_commit_character(value))
                    .take(32)
                    .collect()
            })
            .unwrap_or_else(|| {
                capabilities
                    .pointer("/completionProvider/allCommitCharacters")
                    .and_then(Value::as_array)
                    .map(|values| {
                        values
                            .iter()
                            .filter_map(|v| v.as_str())
                            .filter_map(single_commit_character)
                            .take(32)
                            .collect()
                    })
                    .unwrap_or_default()
            });
        let title = item.label.clone();
        let detail = item.detail.clone();
        let filter_text = item.filter_text.clone().unwrap_or_else(|| title.clone());
        let sort_text = item.sort_text.clone().unwrap_or_else(|| title.clone());
        // Label plus semantic detail distinguishes overloads without including
        // revision-dependent edits or server resolve tokens in identity.
        let id =
            serde_json::to_string(&(&item.label, &item.detail, &item.kind, &item.label_details))
                .map_err(|e| e.to_string())?;
        let documentation = completion_documentation(item.documentation.clone());
        let documentation_resolve = if documentation.is_none()
            && capabilities.pointer("/completionProvider/resolveProvider") == Some(&json!(true))
        {
            let payload = json!({"method":"completionItem/resolve","result":"completion_documentation","capability":null,"parameters":item});
            (serde_json::to_vec(&payload)
                .map_err(|e| e.to_string())?
                .len()
                <= 12 * 1024)
                .then_some(payload)
        } else {
            None
        };
        let action = if item.data.is_some()
            && capabilities.pointer("/completionProvider/resolveProvider") == Some(&json!(true))
        {
            let payload = json!({"method":"completionItem/resolve","result":"resolved_completion","capability":null,"parameters":item});
            if serde_json::to_vec(&payload)
                .map_err(|e| e.to_string())?
                .len()
                > 12 * 1024
            {
                continue;
            }
            ChoiceAction::Semantic { payload }
        } else {
            match completion_action_with_variables(item, encoding, source, request, &variables) {
                Ok(action) => action,
                Err(_) => continue,
            }
        };
        items.push(velk_completion_contracts::Item {
            documentation,
            documentation_resolve,
            commit_characters,
            id,
            filter_text,
            sort_text,
            choice: Choice {
                title,
                detail,
                action,
            },
        });
    }
    Ok(Outcome::Completions {
        items,
        continuation: incomplete.then(|| {
            serde_json::to_value(velk_completion_contracts::Continuation::Incomplete)
                .expect("completion marker is serializable")
        }),
    })
}
fn completion_documentation(
    documentation: Option<lsp_types::Documentation>,
) -> Option<velk_completion_contracts::Documentation> {
    let (mut text, markdown) = match documentation? {
        lsp_types::Documentation::String(text) => (text, false),
        lsp_types::Documentation::MarkupContent(content) => (
            content.value,
            content.kind == lsp_types::MarkupKind::Markdown,
        ),
    };
    let mut end = text.len().min(64 * 1024);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    if text.trim().is_empty() {
        return None;
    }
    Some(velk_completion_contracts::Documentation { text, markdown })
}

fn completion_action(
    item: lsp_types::CompletionItem,
    encoding: Encoding,
    source: &DocumentSnapshot,
    request: PositionRequest,
) -> Result<velk_plugin_sdk::commands::ChoiceAction, String> {
    let variables = snippet_variables(source, request.position);
    completion_action_with_variables(item, encoding, source, request, &variables)
}
fn completion_action_with_variables(
    item: lsp_types::CompletionItem,
    encoding: Encoding,
    source: &DocumentSnapshot,
    request: PositionRequest,
    variables: &velk_plugin_sdk::commands::snippet::Variables,
) -> Result<velk_plugin_sdk::commands::ChoiceAction, String> {
    use velk_plugin_sdk::commands::{ChoiceAction, snippet};
    if item.command.is_some() {
        return Err("Command-based completion is not supported".into());
    }
    let is_snippet = item.insert_text_format == Some(lsp_types::InsertTextFormat::SNIPPET);
    let primary = match item.text_edit {
        Some(lsp_types::CompletionTextEdit::Edit(edit)) => edit,
        Some(lsp_types::CompletionTextEdit::InsertAndReplace(edit)) => lsp_types::TextEdit {
            range: edit.replace,
            new_text: edit.new_text,
        },
        None => {
            let mut start = request.position;
            while start > 0 {
                let c = source.text.char(start - 1);
                if c.is_alphanumeric() || c == '_' {
                    start -= 1;
                } else {
                    break;
                }
            }
            lsp_types::TextEdit {
                range: encoding.range(&source.text, start..request.position),
                new_text: item.insert_text.unwrap_or(item.label),
            }
        }
    };
    let variables = variables
        .iter()
        .filter(|(name, _)| primary.new_text.contains(name.as_str()))
        .map(|(name, value)| (name.clone(), value.clone()))
        .collect();
    let mut edits = vec![primary];
    edits.extend(item.additional_text_edits.unwrap_or_default());
    let edit = velk_plugin_sdk::commands::WorkspaceEdit {
        documents: vec![super::edits::document(
            source.path.clone(),
            Some(source.revision),
            edits,
            encoding,
        )],
    };
    Ok(if is_snippet {
        let snippet = snippet::Template {
            edit,
            document: 0,
            replacement: 0,
            variables,
        };
        snippet.validate()?;
        ChoiceAction::Snippet { snippet }
    } else {
        ChoiceAction::Edit { edit }
    })
}

#[cfg(test)]
mod completion_trigger_tests {
    use super::*;
    #[test]
    fn advertised_characters_gate_requests_and_set_lsp_context() {
        let capabilities = json!({"completionProvider":{"triggerCharacters":[".",":"]}});
        let spec = Request::decode(json!({"method":"textDocument/completion","result":"completion","capability":"/completionProvider"}), &capabilities).unwrap();
        let dot = serde_json::to_string(&velk_completion_contracts::Invocation::Character {
            character: ".".into(),
        })
        .unwrap();
        let dot = velk_plugin_sdk::commands::contributions::Input {
            activation: Some(dot),
            ..Default::default()
        }
        .encode()
        .unwrap();
        assert!(
            spec.completion_trigger_supported(Some(&dot), &capabilities)
                .unwrap()
        );
        let params = spec
            .params(
                "file:///test.rs",
                lsp_types::Position::new(0, 4),
                Some(&dot),
                &[],
            )
            .unwrap();
        assert_eq!(
            params["context"],
            json!({"triggerKind":2,"triggerCharacter":"."})
        );
        assert!(
            !spec
                .completion_trigger_supported(Some(&dot), &json!({"completionProvider":{}}))
                .unwrap()
        );
        let unsupported =
            serde_json::to_string(&velk_completion_contracts::Invocation::Character {
                character: "!".into(),
            })
            .unwrap();
        let unsupported = velk_plugin_sdk::commands::contributions::Input {
            activation: Some(unsupported),
            ..Default::default()
        }
        .encode()
        .unwrap();
        assert!(
            !spec
                .completion_trigger_supported(Some(&unsupported), &capabilities)
                .unwrap()
        );
        assert!(
            spec.completion_trigger_supported(None, &capabilities)
                .unwrap()
        );
        let params = spec
            .params("file:///test.rs", lsp_types::Position::new(0, 4), None, &[])
            .unwrap();
        assert_eq!(params["context"], json!({"triggerKind":1}));
    }
}

#[cfg(test)]
mod documentation_tests {
    use super::*;
    #[test]
    fn documentation_preserves_format_and_bounds_unicode_without_splitting_characters() {
        let plain = completion_documentation(Some(lsp_types::Documentation::String(
            "**literal text**".into(),
        )))
        .unwrap();
        assert!(!plain.markdown);
        assert_eq!(plain.text, "**literal text**");
        let markdown = completion_documentation(Some(lsp_types::Documentation::MarkupContent(
            lsp_types::MarkupContent {
                kind: lsp_types::MarkupKind::Markdown,
                value: "# Title\n\n`code`".into(),
            },
        )))
        .unwrap();
        assert!(markdown.markdown);
        assert!(markdown.text.contains("`code`"));
        let bounded =
            completion_documentation(Some(lsp_types::Documentation::String("界".repeat(30000))))
                .unwrap();
        assert!(bounded.text.len() <= 64 * 1024);
        bounded.validate().unwrap();
        assert!(
            completion_documentation(Some(lsp_types::Documentation::String(" \n".into())))
                .is_none()
        );
    }
}

#[cfg(test)]
mod snippet_tests {
    use super::*;
    #[test]
    fn resolved_snippet_templates_preserve_additional_edits_and_leave_syntax_to_the_provider() {
        let source = DocumentSnapshot {
            path: "/test.rs".into(),
            language: "rust".into(),
            revision: 7,
            text: ropey::Rope::from_str("ca"),
        };
        let position = PositionRequest {
            id: 1,
            revision: 7,
            position: 2,
        };
        let item = json!({"label":"call", "insertTextFormat":2, "insertText":"call(${1:世界})$0", "additionalTextEdits":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"newText":"use dep;\n"}]});
        let request = Request {
            method: "completionItem/resolve".into(),
            result: ResultKind::ResolvedCompletion,
            capability: None,
            parameters: None,
        };
        let outcome = request
            .outcome(
                item,
                Encoding::Utf16,
                (&source, 1),
                position,
                &Value::Null,
                &[],
            )
            .unwrap();
        outcome.validate().unwrap();
        let Outcome::Snippet { snippet } = outcome else {
            panic!("missing snippet");
        };
        assert_eq!(snippet.edit.documents[0].edits.len(), 2);
        assert_eq!(snippet.edit.documents[0].edits[0].text, "call(${1:世界})$0");
        assert!(snippet.variables.is_empty());
        let outcome = completion(json!([{"label":"unsupported", "insertTextFormat":2,"insertText":"${1/a/b/x}"},{"label":"plain"}]), Encoding::Utf16, &source, position, &Value::Null).unwrap();
        let Outcome::Completions { items, .. } = outcome else {
            panic!("missing candidates");
        };
        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|item| item.title == "plain"));
    }
}

#[cfg(test)]
mod continuation_tests {
    use super::*;
    use velk_plugin_sdk::commands::contributions;
    #[test]
    fn continuation_only_affects_refresh_and_arrays_and_null_responses_are_complete() {
        let request = Request {
            method: "textDocument/completion".into(),
            result: ResultKind::Completion,
            capability: None,
            parameters: None,
        };
        let input = |refresh| {
            contributions::Input {
                activation: None,
                continuation: Some(json!("incomplete")),
                refresh,
            }
            .encode()
            .unwrap()
        };
        assert_eq!(
            request
                .params(
                    "file:///test.rs",
                    lsp_types::Position::new(0, 0),
                    Some(&input(true)),
                    &[]
                )
                .unwrap()["context"],
            json!({"triggerKind":3})
        );
        assert_eq!(
            request
                .params(
                    "file:///test.rs",
                    lsp_types::Position::new(0, 0),
                    Some(&input(false)),
                    &[]
                )
                .unwrap()["context"],
            json!({"triggerKind":1})
        );
        let source = DocumentSnapshot {
            path: "/test.rs".into(),
            language: "rust".into(),
            revision: 1,
            text: ropey::Rope::new(),
        };
        for value in [
            json!([]),
            Value::Null,
            json!({"isIncomplete":false,"items":[]}),
        ] {
            let outcome = request
                .outcome(
                    value,
                    Encoding::Utf8,
                    (&source, 0),
                    PositionRequest {
                        id: 1,
                        revision: 1,
                        position: 0,
                    },
                    &Value::Null,
                    &[],
                )
                .unwrap();
            assert!(matches!(
                outcome,
                Outcome::Completions {
                    continuation: None,
                    ..
                }
            ));
        }
    }
}

fn signature_help(value: Value) -> Result<Outcome, String> {
    use velk_plugin_sdk::commands::signature::{Help, Signature};
    let help: lsp_types::SignatureHelp =
        serde_json::from_value(value).map_err(|e| e.to_string())?;
    if help.signatures.is_empty() {
        return Ok(Outcome::SignatureHelp { help: None });
    }
    let active = help
        .active_signature
        .map(|index| index as usize)
        .filter(|index| *index < help.signatures.len().min(32))
        .unwrap_or(0);
    let signatures = help
        .signatures
        .into_iter()
        .take(32)
        .map(|signature| {
            let parameter = signature
                .active_parameter
                .or(help.active_parameter)
                .unwrap_or(0) as usize;
            let parameters = signature.parameters.unwrap_or_default();
            let parameter = parameters.get(parameter).or_else(|| parameters.first());
            let active_parameter = parameter.and_then(|p| match &p.label {
                lsp_types::ParameterLabel::Simple(text) => {
                    signature.label.find(text).map(|start| {
                        let start = signature.label[..start].chars().count();
                        start..start + text.chars().count()
                    })
                }
                lsp_types::ParameterLabel::LabelOffsets([start, end]) => {
                    fn scalar(label: &str, units: u32) -> Option<usize> {
                        let mut offset = 0;
                        for (index, c) in label.chars().enumerate() {
                            if offset == units {
                                return Some(index);
                            }
                            offset += c.len_utf16() as u32;
                        }
                        (offset == units).then(|| label.chars().count())
                    }
                    let start = scalar(&signature.label, *start)?;
                    let end = scalar(&signature.label, *end)?;
                    (start <= end).then_some(start..end)
                }
            });
            let documentation = parameter
                .and_then(|p| p.documentation.clone())
                .or(signature.documentation);
            Signature {
                label: signature.label,
                active_parameter,
                documentation: completion_documentation(documentation),
            }
        })
        .collect();
    let help = Help { signatures, active };
    help.validate()?;
    Ok(Outcome::SignatureHelp { help: Some(help) })
}

fn single_commit_character(value: &str) -> Option<char> {
    let mut chars = value.chars();
    let c = chars.next()?;
    (chars.next().is_none()
        && !c.is_control()
        && !c.is_whitespace()
        && !c.is_alphanumeric()
        && c != '_')
        .then_some(c)
}

#[cfg(test)]
mod signature_tests {
    use super::*;
    #[test]
    fn signatures_decode_utf16_and_per_signature_parameter_overrides() {
        let Outcome::SignatureHelp { help: Some(help) } = signature_help(json!({
            "activeSignature":99, "activeParameter":0, "signatures":[
                {"label":"call(𐐀, x)","activeParameter":1,"parameters":[{"label":[5,7]},{"label":[9,10],"documentation":"active doc"}]},
                {"label":"other(value)","parameters":[{"label":"value"}]}
            ]
        })).unwrap() else { panic!() };
        assert_eq!(help.active, 0);
        assert_eq!(help.signatures[0].active_parameter, Some(8..9));
        assert_eq!(
            help.signatures[0].documentation.as_ref().unwrap().text,
            "active doc"
        );
        assert_eq!(help.signatures[1].active_parameter, Some(6..11));
        let Outcome::SignatureHelp { help: Some(help) } = signature_help(
            json!({"signatures":[{"label":"call(𐐀)","parameters":[{"label":[6,7]}]}]}),
        )
        .unwrap() else {
            panic!()
        };
        assert_eq!(
            help.signatures[0].active_parameter, None,
            "reject split surrogate"
        );
        assert!(matches!(
            signature_help(json!({"signatures":[]})).unwrap(),
            Outcome::SignatureHelp { help: None }
        ));
    }
    #[test]
    fn commit_characters_respect_item_overrides_and_validate_single_punctuation() {
        let snapshot = DocumentSnapshot {
            path: "/file.rs".into(),
            language: "rust".into(),
            revision: 1,
            text: ropey::Rope::from_str("pr"),
        };
        let request = PositionRequest {
            id: 1,
            revision: 1,
            position: 2,
        };
        let Outcome::Completions { items, .. } = completion(
            json!([
                {"label":"print"}, {"label":"private","commitCharacters":[]},
                {"label":"process","commitCharacters":["(","too long","a"," ","_","→"]}
            ]),
            Encoding::Utf16,
            &snapshot,
            request,
            &json!({"completionProvider":{"allCommitCharacters":["."]}}),
        )
        .unwrap() else {
            panic!()
        };
        let find = |name| items.iter().find(|i| i.title == name).unwrap();
        assert_eq!(find("print").commit_characters, vec!['.']);
        assert!(find("private").commit_characters.is_empty());
        assert_eq!(find("process").commit_characters, vec!['(', '→']);
    }
}

fn snippet_variables(
    source: &DocumentSnapshot,
    position: usize,
) -> velk_plugin_sdk::commands::snippet::Variables {
    let line = source.text.char_to_line(position);
    let mut start = position;
    let mut end = position;
    while start > 0
        && position - start <= 64 * 1024
        && (source.text.char(start - 1).is_alphanumeric() || source.text.char(start - 1) == '_')
    {
        start -= 1;
    }
    while end < source.text.len_chars()
        && end - start <= 64 * 1024
        && (source.text.char(end).is_alphanumeric() || source.text.char(end) == '_')
    {
        end += 1;
    }
    let bounded = |slice: ropey::RopeSlice<'_>| {
        if slice.len_bytes() <= 64 * 1024 {
            slice.to_string()
        } else {
            String::new()
        }
    };
    [
        (
            "TM_FILENAME",
            source
                .path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "TM_FILENAME_BASE",
            source
                .path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        (
            "TM_DIRECTORY",
            source
                .path
                .parent()
                .unwrap_or(std::path::Path::new(""))
                .to_string_lossy()
                .into_owned(),
        ),
        ("TM_FILEPATH", source.path.to_string_lossy().into_owned()),
        ("TM_LINE_INDEX", line.to_string()),
        ("TM_LINE_NUMBER", (line + 1).to_string()),
        (
            "TM_CURRENT_LINE",
            bounded(source.text.line(line))
                .trim_end_matches(['\r', '\n'])
                .into(),
        ),
        ("TM_CURRENT_WORD", bounded(source.text.slice(start..end))),
        ("TM_SELECTED_TEXT", String::new()),
    ]
    .into_iter()
    .map(|(name, value)| (name.into(), value))
    .collect()
}

#[cfg(test)]
mod snippet_variable_tests {
    use super::*;
    #[test]
    fn oversized_line_and_word_values_are_unavailable_without_dropping_file_variables() {
        let source = DocumentSnapshot {
            path: "/project/large.rs".into(),
            language: "rust".into(),
            revision: 1,
            text: ropey::Rope::from_str(&"x".repeat(65537)),
        };
        let variables = snippet_variables(&source, 65537);
        assert_eq!(variables["TM_CURRENT_LINE"], "");
        assert_eq!(variables["TM_CURRENT_WORD"], "");
        assert_eq!(variables["TM_FILENAME"], "large.rs");
    }
    #[test]
    fn file_and_document_variables_come_from_the_snapshot() {
        let source = DocumentSnapshot {
            path: "/project/some_file.rs".into(),
            language: "rust".into(),
            revision: 1,
            text: ropey::Rope::from_str("first\nhello_world"),
        };
        let variables = snippet_variables(&source, 9);
        assert_eq!(variables["TM_FILENAME"], "some_file.rs");
        assert_eq!(variables["TM_FILENAME_BASE"], "some_file");
        assert_eq!(variables["TM_LINE_NUMBER"], "2");
        assert_eq!(variables["TM_CURRENT_WORD"], "hello_world");
        assert_eq!(variables["TM_SELECTED_TEXT"], "");
    }
}
