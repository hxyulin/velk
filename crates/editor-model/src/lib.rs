//! Presentation data and text projections shared by editor implementations.
//! Renderer objects, native input, and platform scheduling belong to adapters.
mod annotations;
mod hover;
pub mod inline;
pub mod projection;

pub use annotations::{
    AnnotationError, Annotations, Color, EditorAppearance, HoverCard, LinePattern, OverlayKind,
    OverlayPlacement, SpanStyle, StyledRange, TextOverlay, Underline,
};
pub use hover::{HoverSource, HoverTarget};
