//! Editing commands, input policy, and presentation data shared by editor implementations.
//! Renderer objects, native input, and platform scheduling belong to adapters.
mod annotations;
pub mod commands;
mod hover;
pub mod inline;
mod input;
pub mod navigation;
mod position;
pub mod projection;
pub mod selections;

pub use annotations::{
    AnnotationError, Annotations, Color, EditorAppearance, HoverCard, LinePattern, OverlayKind,
    OverlayPlacement, SpanStyle, StyledRange, TextOverlay, Underline,
};
pub use hover::{HoverSource, HoverTarget};
pub use position::{prepare_document_edit, resolve_position};

pub use commands::EditCommand;

pub mod editing;

pub mod views;
