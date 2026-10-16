//! Compact styled-text model for documentation, independent of RXUI and LSP.
//!
//! This is not a universal markup AST: block layout, embedded widgets, images,
//! and arbitrary extensions require a richer model and a matching consumer.
//! Providers for the standard documentation surfaces return these owned runs;
//! community extensions can define their own contracts without this crate.
//! The async Rust interface is host-side, not a stable dynamic-library ABI.

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    pub bold: bool,
    pub italic: bool,
    pub code: bool,
    pub strikethrough: bool,
    pub heading: u8,
    /// Preserved as data; the UI decides whether/how links may be activated.
    pub link: Option<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub text: String,
    pub style: Style,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Document {
    pub runs: Vec<Run>,
}
impl Document {
    pub fn plain(text: impl Into<String>) -> Self {
        Self {
            runs: vec![Run {
                text: text.into(),
                style: Style::default(),
            }],
        }
    }
    pub fn text(&self) -> String {
        self.runs.iter().map(|r| r.text.as_str()).collect()
    }
    pub fn push(&mut self, text: &str, style: &Style) {
        if text.is_empty() {
            return;
        }
        if let Some(last) = self.runs.last_mut().filter(|r| r.style == *style) {
            last.text.push_str(text);
        } else {
            self.runs.push(Run {
                text: text.into(),
                style: style.clone(),
            });
        }
    }
    pub fn separate(&mut self, count: usize) {
        let existing = self
            .runs
            .iter()
            .rev()
            .flat_map(|r| r.text.chars().rev())
            .take_while(|c| *c == '\n')
            .count();
        if !self.runs.is_empty() && existing < count {
            self.push(&"\n".repeat(count - existing), &Style::default());
        }
    }
}
/// Host-facing async interface; Rust futures never cross a plugin ABI.
pub type RenderFuture =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<Document, String>> + Send>>;
pub trait Renderer: Send + Sync {
    fn render(&self, markdown: String) -> RenderFuture;
}
/// Literal fallback when no provider is loaded. No parser is linked into the host.
#[derive(Default)]
pub struct PlainText;
impl Renderer for PlainText {
    fn render(&self, text: String) -> RenderFuture {
        Box::pin(async move { Ok(Document::plain(text)) })
    }
}
