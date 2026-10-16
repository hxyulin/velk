use std::ops::Range;

/// Maps a source range around a transient IME replacement. Offsets and inserted
/// lengths must use the same units (bytes or Unicode scalars). The preedit
/// itself remains unstyled; source spans before/after it retain their identity.
pub fn display_ranges(
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

/// Maps an anchor using the same coordinate units as the replacement. Anchors
/// inside replaced text disappear; anchors at its end follow the inserted text.
pub fn display_anchor(anchor: usize, replacement: Option<&(Range<usize>, usize)>) -> Option<usize> {
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
