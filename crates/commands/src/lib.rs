//! Command data shared by providers, semantic services, and presentation adapters.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Declaration {
    pub id: String,
    pub title: String,
    pub language: Option<String>,
    pub handler: Handler,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Handler {
    Request {
        hook: String,
    },
    /// Opaque backend-owned data. The host does not interpret protocol methods.
    Semantic {
        payload: serde_json::Value,
    },
}
impl Declaration {
    pub fn validate(&self) -> Result<(), String> {
        if !valid_id(&self.id)
            || self.title.trim().is_empty()
            || self.title.len() > 256
            || self.title.chars().any(char::is_control)
            || self
                .language
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.len() > 64 || s.chars().any(char::is_control))
        {
            return Err("invalid command declaration".into());
        }
        match &self.handler {
            Handler::Request { hook } if !valid_id(hook) => {
                return Err("invalid command hook".into());
            }
            Handler::Semantic { payload }
                if serde_json::to_vec(payload)
                    .map_err(|e| e.to_string())?
                    .len()
                    > 16 * 1024 =>
            {
                return Err("command payload exceeds 16 KiB".into());
            }
            _ => {}
        }
        Ok(())
    }
}
fn valid_id(id: &str) -> bool {
    id.len() < 128
        && id.contains('.')
        && id.split('.').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
        })
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Context {
    pub path: Option<PathBuf>,
    pub language: Option<String>,
    pub revision: Option<u64>,
    /// Unicode scalar offset in the source document.
    pub position: Option<usize>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Encoding {
    Utf8,
    Utf16,
    Utf32,
}
/// A zero-based line and column, measured in the specified encoding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Position {
    pub line: u32,
    pub column: u32,
    pub encoding: Encoding,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Location {
    pub path: PathBuf,
    pub position: Position,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Outcome {
    Message {
        text: String,
    },
    Locations {
        locations: Vec<Location>,
    },
    Document {
        title: String,
        text: String,
        language: Option<String>,
    },
}
impl Outcome {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Locations { locations }
                if locations.len() > 10_000 || locations.iter().any(|l| !l.path.is_absolute()) =>
            {
                return Err("invalid command locations".into());
            }
            Self::Document { title, .. }
                if title.trim().is_empty()
                    || title.len() > 256
                    || title.chars().any(char::is_control) =>
            {
                return Err("invalid command document title".into());
            }
            _ => {}
        }
        if serde_json::to_vec(self).map_err(|e| e.to_string())?.len() > 4 * 1024 * 1024 {
            return Err("command output exceeds 4 MiB".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod declaration_tests {
    use super::*;
    #[test]
    fn declarations_reject_invalid_ids_labels_hooks_and_unbounded_backend_payloads() {
        let valid = Declaration {
            id: "test.action".into(),
            title: "Action".into(),
            language: None,
            handler: Handler::Request {
                hook: "test.invoke".into(),
            },
        };
        valid.validate().unwrap();
        for id in ["", "unscoped", "test..action", "test.Action"] {
            let mut d = valid.clone();
            d.id = id.into();
            assert!(d.validate().is_err());
        }
        let mut d = valid.clone();
        d.title = "bad\ntitle".into();
        assert!(d.validate().is_err());
        let mut d = valid.clone();
        d.handler = Handler::Request {
            hook: "invalid".into(),
        };
        assert!(d.validate().is_err());
        let mut d = valid;
        d.handler = Handler::Semantic {
            payload: serde_json::json!("x".repeat(16 * 1024)),
        };
        assert!(d.validate().is_err());
        assert!(
            Outcome::Locations {
                locations: vec![Location {
                    path: "relative.rs".into(),
                    position: Position {
                        line: 0,
                        column: 0,
                        encoding: Encoding::Utf32
                    }
                }]
            }
            .validate()
            .is_err()
        );
    }
}
