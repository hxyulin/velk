//! Backend-owned command payloads and LSP result conversion.
use super::{positions::Encoding, *};
use velk_plugin_sdk::commands::{Encoding as TargetEncoding, Location, Outcome, Position};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Request {
    pub method: String,
    pub result: ResultKind,
    pub capability: Option<String>,
}
#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ResultKind {
    Locations,
    MacroExpansion,
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
    pub fn params(&self, document_uri: &str, position: lsp_types::Position) -> Value {
        let mut params = json!({"textDocument":{"uri":document_uri},"position":position});
        if self.method == "textDocument/references" {
            params["context"] = json!({"includeDeclaration":true});
        }
        params
    }
    pub fn outcome(&self, value: Value, encoding: Encoding) -> Result<Outcome, String> {
        if value.is_null() {
            return Ok(Outcome::Message {
                text: "No result at the caret".into(),
            });
        }
        let outcome = match self.result {
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
