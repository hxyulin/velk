//! Extended grapheme queries over borrowed Ropey chunks, without flattening a line.
use ropey::Rope;
use unicode_segmentation::{GraphemeCursor, GraphemeIncomplete};

pub fn is_boundary(text: &Rope, character: usize) -> bool {
    query(text, character, GraphemeCursor::is_boundary)
}

pub fn previous(text: &Rope, character: usize) -> usize {
    query(text, character, GraphemeCursor::prev_boundary).map_or(0, |byte| text.byte_to_char(byte))
}

pub fn next(text: &Rope, character: usize) -> usize {
    query(text, character, GraphemeCursor::next_boundary)
        .map_or_else(|| text.len_chars(), |byte| text.byte_to_char(byte))
}

fn query<T>(
    text: &Rope,
    character: usize,
    mut operation: impl FnMut(&mut GraphemeCursor, &str, usize) -> Result<T, GraphemeIncomplete>,
) -> T {
    let byte = text.char_to_byte(character);
    let mut cursor = GraphemeCursor::new(byte, text.len_bytes(), true);
    let (mut chunk, mut start, _, _) = text.chunk_at_byte(byte);
    loop {
        match operation(&mut cursor, chunk, start) {
            Ok(result) => return result,
            Err(GraphemeIncomplete::PreContext(end)) => {
                // Context must end exactly where the cursor requests it. It may
                // precede the chunk currently being traversed, or end inside it.
                let (context, context_start, _, _) = text.chunk_at_byte(end - 1);
                cursor.provide_context(&context[..end - context_start], context_start);
            }
            Err(GraphemeIncomplete::PrevChunk) => {
                (chunk, start, _, _) = text.chunk_at_byte(start - 1);
            }
            Err(GraphemeIncomplete::NextChunk) => {
                (chunk, start, _, _) = text.chunk_at_byte(start + chunk.len());
            }
            Err(GraphemeIncomplete::InvalidOffset) => {
                unreachable!("Ropey chunks must contain the grapheme cursor offset")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use unicode_segmentation::UnicodeSegmentation;

    fn check(text: &Rope, positions: impl IntoIterator<Item = usize>) {
        let source = text.to_string();
        let mut boundaries = vec![0];
        for grapheme in source.graphemes(true) {
            boundaries.push(boundaries.last().unwrap() + grapheme.chars().count());
        }
        for position in positions {
            assert_eq!(
                is_boundary(text, position),
                boundaries.binary_search(&position).is_ok(),
                "boundary at {position}"
            );
            assert_eq!(
                previous(text, position),
                boundaries[boundaries
                    .partition_point(|&p| p < position)
                    .saturating_sub(1)],
                "previous at {position}"
            );
            assert_eq!(
                next(text, position),
                boundaries
                    .get(boundaries.partition_point(|&p| p <= position))
                    .copied()
                    .unwrap_or(text.len_chars()),
                "next at {position}"
            );
        }
    }

    #[test]
    fn chunk_queries_match_contiguous_extended_segmentation() {
        for source in [
            String::new(),
            "a\r\n\t\u{2028}\u{2029}é👩🏽‍💻🇭🇰🇯🇵\u{600}aक्‍क각".repeat(64),
        ] {
            let text = Rope::from_str(&source);
            check(&text, 0..=text.len_chars());
        }
    }

    #[test]
    fn multi_chunk_context_and_clusters_match_contiguous_segmentation() {
        for sequence in [
            format!("a{}", "\u{301}".repeat(3000)),
            "🇭🇰🇯".repeat(601),
            format!("👩{}\u{200d}💻", "\u{301}".repeat(1800)),
            format!("क{}क", "\u{94d}\u{93c}".repeat(1800)),
            format!("{}a", "\u{600}".repeat(1800)),
            "각".repeat(600),
        ] {
            // Different leaf boundaries after insertion/removal exercise Ropey
            // chunk layouts beyond the initial from_str split.
            let mut text = Rope::from_str(&format!("{}{}z\n", "x".repeat(1100), sequence));
            for prefix in [0, 17, 1011] {
                text.insert(0, &"a".repeat(prefix));
                let mut positions = vec![0, text.len_chars(), 1100 + prefix];
                let mut offset = 0_usize;
                for chunk in text.chunks() {
                    positions.extend(offset.saturating_sub(3)..=(offset + 3).min(text.len_chars()));
                    offset += chunk.chars().count();
                }
                positions.extend(text.len_chars().saturating_sub(8)..=text.len_chars());
                check(&text, positions);
                text.remove(0..prefix);
            }
        }
    }

    #[test]
    fn mixed_categories_match_at_every_scalar_after_rope_mutations() {
        let tokens = [
            "a", "\n", "\r", "\t", "\u{301}", "\u{600}", "\u{200d}", "\u{200c}", "👩", "💻", "🏽",
            "🇭", "🇰", "क", "\u{94d}", "\u{93c}", "ᄀ", "ᅡ", "ᆨ", "\u{903}", "\u{fe0f}",
        ];
        let mut seed = 0x5eed_u64;
        for _ in 0..24 {
            let mut source = String::new();
            for _ in 0..512 {
                seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
                source.push_str(tokens[(seed >> 32) as usize % tokens.len()]);
            }
            let mut text = Rope::from_str(&source);
            text.insert(113, "👩🏽‍💻\nक्‍क");
            text.remove(231..237);
            check(&text, 0..=text.len_chars());
        }
    }
}
