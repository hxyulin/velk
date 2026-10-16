use velk_commands::{Encoding, Position};

/// Reject invalid lines, columns, and partial code points instead of moving
/// the caret to a different source location.
pub fn resolve_position(position: Position, text: &ropey::Rope) -> Option<usize> {
    let line = position.line as usize;
    if line >= text.len_lines() {
        return None;
    }
    let start = text.line_to_char(line);
    let mut end = start + text.line(line).len_chars();
    while end > start && matches!(text.char(end - 1), '\n' | '\r') {
        end -= 1;
    }
    let column = position.column as usize;
    let offset = match position.encoding {
        Encoding::Utf32 => start.checked_add(column)?,
        Encoding::Utf8 => {
            let byte = text.char_to_byte(start).checked_add(column)?;
            if byte > text.char_to_byte(end) {
                return None;
            }
            let offset = text.byte_to_char(byte);
            if text.char_to_byte(offset) != byte {
                return None;
            }
            offset
        }
        Encoding::Utf16 => {
            let unit = text.char_to_utf16_cu(start).checked_add(column)?;
            if unit > text.char_to_utf16_cu(end) {
                return None;
            }
            let offset = text.utf16_cu_to_char(unit);
            if text.char_to_utf16_cu(offset) != unit {
                return None;
            }
            offset
        }
    };
    (offset <= end).then_some(offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn positions_resolve_unicode_and_reject_partial_characters_and_line_endings() {
        let text = ropey::Rope::from_str("a界𐐀\r\nnext");
        for (encoding, column) in [
            (Encoding::Utf8, 8),
            (Encoding::Utf16, 4),
            (Encoding::Utf32, 3),
        ] {
            assert_eq!(
                resolve_position(
                    Position {
                        line: 0,
                        column,
                        encoding
                    },
                    &text
                ),
                Some(3)
            );
            assert!(
                resolve_position(
                    Position {
                        line: 0,
                        column: column + 1,
                        encoding
                    },
                    &text
                )
                .is_none()
            );
        }
        assert!(
            resolve_position(
                Position {
                    line: 0,
                    column: 2,
                    encoding: Encoding::Utf8
                },
                &text
            )
            .is_none()
        );
        assert!(
            resolve_position(
                Position {
                    line: 0,
                    column: 3,
                    encoding: Encoding::Utf16
                },
                &text
            )
            .is_none()
        );
        assert!(
            resolve_position(
                Position {
                    line: 9,
                    column: 0,
                    encoding: Encoding::Utf32
                },
                &text
            )
            .is_none()
        );
    }
}
