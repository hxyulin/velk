#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HoverSource {
    Pointer,
    Caret,
}
/// A view-scoped request token. The owning view validates identity and document
/// revision before accepting a result; position is a Unicode scalar offset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HoverTarget {
    pub id: u64,
    pub revision: u64,
    pub position: usize,
    pub source: HoverSource,
}
