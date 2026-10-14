use std::ops::Range;

/// A view-only insertion into the IME-projected source string. Neither its bytes
/// nor its caret stops belong to the document. Stable sort preserves provider
/// order when several hints share an anchor.
#[derive(Clone, Debug)]
pub(crate) struct InlineInsertion {
    pub base_anchor: usize,
    pub overlay_index: usize,
    pub display: Range<usize>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct InlineProjection {
    pub insertions: Vec<InlineInsertion>,
}

impl InlineProjection {
    pub fn build(base: &str, mut hints: Vec<(usize, usize, &str)>) -> (String, Self) {
        hints.sort_by_key(|hint| hint.0);
        let mut projection = Self::default();
        let mut text = String::with_capacity(base.len());
        let mut cursor = 0;
        for (anchor, overlay_index, label) in hints {
            text.push_str(&base[cursor..anchor]);
            let start = text.len();
            // Format controls keep surrounding combining marks from joining a
            // hint's padding. Spaces reserve shaped horizontal padding.
            text.push('\u{2060}');
            text.push(' ');
            text.push_str(label);
            text.push(' ');
            text.push('\u{2060}');
            projection.insertions.push(InlineInsertion {
                base_anchor: anchor,
                overlay_index,
                display: start..text.len(),
            });
            cursor = anchor;
        }
        text.push_str(&base[cursor..]);
        (text, projection)
    }

    /// Source carets use right affinity: at an anchor they sit after every hint.
    /// Range endpoints use left affinity so an ending selection excludes hints.
    pub fn display_offset(&self, base: usize, after: bool) -> usize {
        base + self
            .insertions
            .iter()
            .take_while(|hint| hint.base_anchor < base || after && hint.base_anchor == base)
            .map(|hint| hint.display.len())
            .sum::<usize>()
    }

    /// Every pointer position inside a virtual label maps to its source anchor.
    pub fn base_offset(&self, display: usize) -> usize {
        let mut inserted = 0;
        for hint in &self.insertions {
            if display < hint.display.start {
                break;
            }
            if display <= hint.display.end {
                return hint.base_anchor;
            }
            inserted += hint.display.len();
        }
        display - inserted
    }

    /// Project source spans/selection while excluding all inserted hint text.
    pub fn ranges(&self, base: Range<usize>) -> Vec<Range<usize>> {
        if base.is_empty() {
            return Vec::new();
        }
        let mut result = Vec::new();
        let mut cursor = base.start;
        for hint in &self.insertions {
            let anchor = hint.base_anchor;
            if anchor <= cursor || anchor >= base.end {
                continue;
            }
            result.push(self.display_offset(cursor, true)..self.display_offset(anchor, false));
            cursor = anchor;
        }
        result.push(self.display_offset(cursor, true)..self.display_offset(base.end, false));
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inline_labels_reserve_space_without_entering_source_ranges() {
        let base = "let count = 42;";
        let (text, projection) = InlineProjection::build(base, vec![(9, 0, ": i32")]);
        assert!(text.contains("count\u{2060} : i32 \u{2060} ="));
        for byte in 0..=base.len() {
            assert_eq!(
                projection.base_offset(projection.display_offset(byte, true)),
                byte
            );
        }
        let selected = projection
            .ranges(0..base.len())
            .iter()
            .map(|range| &text[range.clone()])
            .collect::<String>();
        assert_eq!(selected, base);
        let hint = &projection.insertions[0];
        for byte in hint.display.clone() {
            assert_eq!(projection.base_offset(byte), 9);
        }
    }

    #[test]
    fn repeated_anchors_preserve_order_and_boundary_selections_exclude_hints() {
        let (text, projection) = InlineProjection::build(
            "abc",
            vec![
                (1, 7, "first"),
                (1, 2, "second"),
                (0, 3, "start"),
                (3, 4, "end"),
            ],
        );
        assert_eq!(
            projection
                .insertions
                .iter()
                .map(|h| h.overlay_index)
                .collect::<Vec<_>>(),
            vec![3, 7, 2, 4]
        );
        for range in [0..1, 1..2, 0..3, 2..3] {
            let projected = projection
                .ranges(range.clone())
                .iter()
                .map(|r| &text[r.clone()])
                .collect::<String>();
            assert_eq!(projected, &"abc"[range]);
        }
        assert!(projection.ranges(1..1).is_empty());
    }

    #[test]
    fn unicode_source_and_hint_bytes_round_trip_on_all_character_edges() {
        let base = "é👩‍💻世界 العربية";
        let anchors = ["é".len(), "é👩‍💻".len(), "é👩‍💻世界".len()];
        let (text, projection) = InlineProjection::build(
            base,
            vec![
                (anchors[1], 0, ": 类型"),
                (anchors[0], 1, "👋"),
                (anchors[2], 2, "قيمة"),
            ],
        );
        for byte in base.char_indices().map(|(b, _)| b).chain([base.len()]) {
            let display = projection.display_offset(byte, true);
            assert!(text.is_char_boundary(display));
            assert_eq!(projection.base_offset(display), byte);
        }
        let source = projection
            .ranges(0..base.len())
            .iter()
            .map(|range| &text[range.clone()])
            .collect::<String>();
        assert_eq!(source, base);
    }
}
