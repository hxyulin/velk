use crate::Buffer;
use rxui::astrelis::{FontSlant, TextSpan};
use std::{error::Error, fmt, ops::Range};

/// Linear, straight RGBA (the same color space used by RXUI/Astrelis).
pub type Color = [f32; 4];

/// A configurable line under a text range.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Underline {
    pub color: Color,
    pub pattern: LinePattern,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LinePattern {
    #[default]
    Solid,
    Dashed,
    Wavy,
}

/// Optional attributes inherit the editor defaults. Later overlapping spans
/// override foreground/font attributes they set. Backgrounds and custom lines
/// are painted in span order, so translucent backgrounds can be layered.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SpanStyle {
    pub foreground: Option<Color>,
    pub background: Option<Color>,
    pub weight: Option<u16>,
    pub italic: Option<bool>,
    pub font_size: Option<f32>,
    pub underline: Option<Underline>,
    pub overline: Option<Color>,
    pub strikethrough: Option<bool>,
    /// Extends the background across every line touched by this range.
    pub full_line_background: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct StyledRange {
    /// Rope character offsets, on extended grapheme boundaries.
    pub range: Range<usize>,
    pub style: SpanStyle,
    pub hover: Option<HoverCard>,
}

impl StyledRange {
    pub fn new(range: Range<usize>, style: SpanStyle) -> Self {
        Self {
            range,
            style,
            hover: None,
        }
    }

    pub fn with_hover(mut self, title: impl Into<String>, body: impl Into<String>) -> Self {
        self.hover = Some(HoverCard {
            title: title.into(),
            body: body.into(),
        });
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HoverCard {
    pub title: String,
    pub body: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OverlayKind {
    InlayHint,
    Diagnostic,
    DebugValue,
    GhostText,
}

/// Overlays never change document text or caret positions. LineEnd places badges
/// after the source line and stacks multiple badges horizontally. AtAnchor paints
/// at the caret position; use it for ghost completions at an empty/end-of-line
/// position, as it intentionally does not reserve space in source text. Inline
/// reserves space at its anchor and shifts following glyphs; its virtual text
/// still never enters document edits, selections, clipboard, or undo history.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OverlayPlacement {
    #[default]
    LineEnd,
    AtAnchor,
    Inline,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextOverlay {
    /// Rope character offset, on an extended grapheme boundary.
    pub anchor: usize,
    pub text: String,
    pub kind: OverlayKind,
    pub placement: OverlayPlacement,
    pub foreground: Color,
    pub background: Option<Color>,
    pub hover: Option<HoverCard>,
}

/// An immutable result for one document revision. A syntax/LSP/debug producer
/// can compute this off-thread and apply it only if the revision still matches.
/// Editing, undo, and redo invalidate the snapshot; the producer must refresh it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Annotations {
    pub document_revision: u64,
    pub spans: Vec<StyledRange>,
    pub overlays: Vec<TextOverlay>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EditorAppearance {
    pub background: Color,
    pub foreground: Color,
    pub selection: Color,
}

impl Default for EditorAppearance {
    fn default() -> Self {
        Self::dark()
    }
}

impl EditorAppearance {
    pub const fn dark() -> Self {
        Self {
            // Graphite #1B1E24 on #D4D2CB, in linear RGB.
            background: [0.011, 0.013, 0.0176, 1.],
            foreground: [0.658, 0.644, 0.597, 1.],
            selection: [0.10, 0.28, 0.60, 0.45],
        }
    }

    pub const fn light() -> Self {
        Self {
            background: [0.94, 0.96, 0.99, 1.],
            foreground: [0.025, 0.04, 0.07, 1.],
            selection: [0.20, 0.46, 0.82, 0.25],
        }
    }

    pub fn validate(self) -> Result<(), AnnotationError> {
        for color in [self.background, self.foreground, self.selection] {
            validate_color(color)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AnnotationError {
    StaleRevision { expected: u64, received: u64 },
    InvalidRange,
    InvalidColor,
    InvalidFont,
    InvalidOverlayText,
}

impl fmt::Display for AnnotationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::StaleRevision { expected, received } => write!(
                f,
                "annotations target revision {received}; current document revision is {expected}"
            ),
            Self::InvalidRange => {
                f.write_str("annotation positions must be in-bounds extended grapheme boundaries")
            }
            Self::InvalidColor => {
                f.write_str("annotation colors must be finite RGBA with alpha in 0..=1")
            }
            Self::InvalidFont => {
                f.write_str("font size must be finite and in 6..=96; weight must be in 1..=1000")
            }
            Self::InvalidOverlayText => {
                f.write_str("overlay text must be a single line without control characters")
            }
        }
    }
}
impl Error for AnnotationError {}

impl Annotations {
    pub fn validate(&self, document: &Buffer) -> Result<(), AnnotationError> {
        if self.document_revision != document.revision() {
            return Err(AnnotationError::StaleRevision {
                expected: document.revision(),
                received: self.document_revision,
            });
        }
        for span in &self.spans {
            if span.range.start > span.range.end
                || !document.is_grapheme_boundary(span.range.start)
                || !document.is_grapheme_boundary(span.range.end)
            {
                return Err(AnnotationError::InvalidRange);
            }
            let s = &span.style;
            for color in [
                s.foreground,
                s.background,
                s.overline,
                s.underline.map(|u| u.color),
            ]
            .into_iter()
            .flatten()
            {
                validate_color(color)?;
            }
            if s.weight.is_some_and(|w| !(1..=1000).contains(&w))
                || s.font_size
                    .is_some_and(|v| !v.is_finite() || !(6. ..=96.).contains(&v))
            {
                return Err(AnnotationError::InvalidFont);
            }
        }
        for overlay in &self.overlays {
            if !document.is_grapheme_boundary(overlay.anchor) {
                return Err(AnnotationError::InvalidRange);
            }
            if overlay.text.chars().any(char::is_control)
                || overlay.text.contains(['\u{2028}', '\u{2029}'])
            {
                return Err(AnnotationError::InvalidOverlayText);
            }
            validate_color(overlay.foreground)?;
            if let Some(color) = overlay.background {
                validate_color(color)?;
            }
        }
        Ok(())
    }
}

impl SpanStyle {
    pub(crate) fn text_span(&self, range: Range<usize>) -> Option<TextSpan> {
        if self.foreground.is_none()
            && self.weight.is_none()
            && self.italic.is_none()
            && self.font_size.is_none()
            && self.strikethrough.is_none()
        {
            return None;
        }
        Some(TextSpan {
            range,
            color: self.foreground,
            weight: self.weight,
            slant: self.italic.map(|italic| {
                if italic {
                    FontSlant::Italic
                } else {
                    FontSlant::Normal
                }
            }),
            font_size: self.font_size,
            strikethrough: self.strikethrough,
            ..TextSpan::default()
        })
    }
}

fn validate_color(color: Color) -> Result<(), AnnotationError> {
    if color.into_iter().all(f32::is_finite) && (0. ..=1.).contains(&color[3]) {
        Ok(())
    } else {
        Err(AnnotationError::InvalidColor)
    }
}

/// Maps a source byte range around a transient IME replacement. The preedit
/// itself remains unstyled; source spans before/after it retain their identity.
pub(crate) fn display_ranges(
    range: Range<usize>,
    replacement: Option<&(Range<usize>, usize)>,
) -> Vec<Range<usize>> {
    let Some((replaced, inserted)) = replacement else {
        return if range.is_empty() {
            Vec::new()
        } else {
            vec![range]
        };
    };
    let mut result = Vec::with_capacity(2);
    if range.start < replaced.start {
        let end = range.end.min(replaced.start);
        if range.start < end {
            result.push(range.start..end);
        }
    }
    if range.end > replaced.end {
        let start = range.start.max(replaced.end) - replaced.end + replaced.start + inserted;
        let end = range.end - replaced.end + replaced.start + inserted;
        if start < end {
            result.push(start..end);
        }
    }
    result
}

pub(crate) fn display_anchor(
    anchor: usize,
    replacement: Option<&(Range<usize>, usize)>,
) -> Option<usize> {
    match replacement {
        Some((range, inserted)) if anchor >= range.end => {
            Some(anchor - range.end + range.start + inserted)
        }
        Some((range, _)) if anchor >= range.start => None,
        _ => Some(anchor),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_ranges_and_anchors_map_around_ime_preedit() {
        let replacement = (4..8, 2);
        assert_eq!(display_ranges(1..3, Some(&replacement)), vec![1..3]);
        assert_eq!(display_ranges(1..12, Some(&replacement)), vec![1..4, 6..10]);
        assert_eq!(
            display_ranges(4..8, Some(&replacement)),
            Vec::<Range<usize>>::new()
        );
        assert_eq!(display_anchor(3, Some(&replacement)), Some(3));
        assert_eq!(display_anchor(5, Some(&replacement)), None);
        assert_eq!(display_anchor(10, Some(&replacement)), Some(8));
        let insertion = (4..4, 2);
        assert_eq!(display_ranges(1..8, Some(&insertion)), vec![1..4, 6..10]);
        assert_eq!(display_anchor(4, Some(&insertion)), Some(6));
    }
}
