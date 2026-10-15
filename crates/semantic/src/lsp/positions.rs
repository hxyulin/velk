use lsp_types::{Position, PositionEncodingKind, Range};
use ropey::Rope;
#[derive(Clone, Copy, Debug)]
pub(super) enum Encoding {
    Utf8,
    Utf16,
    Utf32,
}
impl Encoding {
    pub fn negotiate(kind: Option<&PositionEncodingKind>) -> Result<Self, String> {
        match kind.map(PositionEncodingKind::as_str).unwrap_or("utf-16") {
            "utf-8" => Ok(Self::Utf8),
            "utf-16" => Ok(Self::Utf16),
            "utf-32" => Ok(Self::Utf32),
            other => Err(format!("unsupported LSP position encoding: {other}")),
        }
    }
    pub fn position(self, text: &Rope, offset: usize) -> Position {
        let line = text.char_to_line(offset);
        let start = text.line_to_char(line);
        let character = match self {
            Self::Utf8 => text.char_to_byte(offset) - text.char_to_byte(start),
            Self::Utf16 => text.char_to_utf16_cu(offset) - text.char_to_utf16_cu(start),
            Self::Utf32 => offset - start,
        };
        Position::new(line as u32, character as u32)
    }
    pub fn range(self, text: &Rope, range: std::ops::Range<usize>) -> Range {
        Range::new(
            self.position(text, range.start),
            self.position(text, range.end),
        )
    }
    fn decode(self, text: &Rope, position: Position) -> Option<usize> {
        let line = position.line as usize;
        if line >= text.len_lines() {
            return None;
        }
        let start = text.line_to_char(line);
        let length = text.line(line).len_chars();
        let end = start + length - usize::from(length > 0 && text.char(start + length - 1) == '\n');
        let column = position.character as usize;
        let offset = match self {
            Self::Utf8 => {
                let byte = text.char_to_byte(start).checked_add(column)?;
                if byte > text.char_to_byte(end) {
                    return None;
                }
                let c = text.byte_to_char(byte);
                if text.char_to_byte(c) != byte {
                    return None;
                }
                c
            }
            Self::Utf16 => {
                let unit = text.char_to_utf16_cu(start).checked_add(column)?;
                if unit > text.char_to_utf16_cu(end) {
                    return None;
                }
                let c = text.utf16_cu_to_char(unit);
                if text.char_to_utf16_cu(c) != unit {
                    return None;
                }
                c
            }
            Self::Utf32 => start.checked_add(column)?,
        };
        (offset <= end).then_some(offset)
    }
    pub fn decode_range(self, text: &Rope, range: Range) -> Option<std::ops::Range<usize>> {
        let start = self.decode(text, range.start)?;
        let end = self.decode(text, range.end)?;
        (start <= end).then_some(start..end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_offsets_round_trip_and_partial_codepoints_are_rejected() {
        let text = Rope::from_str("a👩‍💻é世界\nnext\n");
        for encoding in [Encoding::Utf8, Encoding::Utf16, Encoding::Utf32] {
            for offset in 0..=text.len_chars() {
                assert_eq!(
                    encoding.decode(&text, encoding.position(&text, offset)),
                    Some(offset)
                );
            }
            assert!(encoding.decode(&text, Position::new(99, 0)).is_none());
            assert!(encoding.decode(&text, Position::new(0, 100)).is_none());
            assert!(
                encoding
                    .decode_range(&text, Range::new(Position::new(1, 1), Position::new(0, 0)))
                    .is_none()
            );
        }
        assert!(Encoding::Utf8.decode(&text, Position::new(0, 2)).is_none());
        assert!(Encoding::Utf16.decode(&text, Position::new(0, 2)).is_none());
        assert!(matches!(
            Encoding::negotiate(None).unwrap(),
            Encoding::Utf16
        ));
        assert!(Encoding::negotiate(Some(&PositionEncodingKind::new("unknown"))).is_err());
    }
}
