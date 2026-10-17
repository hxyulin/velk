//! Callable signatures use Unicode scalar ranges, independent of backend encoding.
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signature {
    pub label: String,
    pub active_parameter: Option<std::ops::Range<usize>>,
    pub documentation: Option<crate::completion::Documentation>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Help {
    pub signatures: Vec<Signature>,
    pub active: usize,
}
impl Help {
    pub fn validate(&self) -> Result<(), String> {
        if self.signatures.len() > 32 || self.active >= self.signatures.len() {
            return Err("invalid signature selection".into());
        }
        for signature in &self.signatures {
            if signature.label.is_empty()
                || signature.label.len() > 4096
                || signature.label.contains('\0')
                || signature
                    .active_parameter
                    .as_ref()
                    .is_some_and(|r| r.start > r.end || r.end > signature.label.chars().count())
            {
                return Err("invalid signature label or parameter".into());
            }
            if let Some(documentation) = &signature.documentation {
                documentation.validate()?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signatures_reject_invalid_selections_ranges_and_oversized_labels() {
        let mut help = Help {
            signatures: vec![Signature {
                label: "call(value)".into(),
                active_parameter: Some(5..10),
                documentation: None,
            }],
            active: 0,
        };
        help.validate().unwrap();
        help.active = 1;
        assert!(help.validate().is_err());
        help.active = 0;
        help.signatures[0].active_parameter = Some(0..100);
        assert!(help.validate().is_err());
        help.signatures[0].active_parameter = None;
        help.signatures[0].label = "x".repeat(4097);
        assert!(help.validate().is_err());
    }
}
