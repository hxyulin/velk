use ropey::Rope;
use std::ops::Range;

/// Minimal bounding replacement. Compare borrowed chunks (often pointer-equal)
/// rather than visiting every scalar in unchanged parts of a large document.
pub(crate) fn difference(before: &Rope, after: &Rope) -> Option<(Range<usize>, String)> {
    let limit = before.len_bytes().min(after.len_bytes());
    let prefix = common(before.chunks(), after.chunks(), limit, false);
    if prefix == before.len_bytes() && prefix == after.len_bytes() {
        return None;
    }
    let suffix = common(
        before.chunks_at_byte(before.len_bytes()).0.reversed(),
        after.chunks_at_byte(after.len_bytes()).0.reversed(),
        limit - prefix,
        true,
    );
    let start = before.byte_to_char(prefix);
    let end = |text: &Rope| {
        let byte = text.len_bytes() - suffix;
        let c = text.byte_to_char(byte);
        c + usize::from(text.char_to_byte(c) < byte)
    };
    Some((
        start..end(before),
        after.slice(start..end(after)).to_string(),
    ))
}
fn common<'a>(
    mut a: impl Iterator<Item = &'a str>,
    mut b: impl Iterator<Item = &'a str>,
    limit: usize,
    reverse: bool,
) -> usize {
    let mut x = &[][..];
    let mut y = &[][..];
    let mut count = 0;
    while count < limit {
        if x.is_empty() {
            let Some(chunk) = a.next() else { break };
            x = chunk.as_bytes();
        }
        if y.is_empty() {
            let Some(chunk) = b.next() else { break };
            y = chunk.as_bytes();
        }
        let n = x.len().min(y.len()).min(limit - count);
        let (left, right) = if reverse {
            (&x[x.len() - n..], &y[y.len() - n..])
        } else {
            (&x[..n], &y[..n])
        };
        if !std::ptr::eq(left, right) && left != right {
            return count
                + if reverse {
                    left.iter()
                        .rev()
                        .zip(right.iter().rev())
                        .take_while(|(a, b)| a == b)
                        .count()
                } else {
                    left.iter().zip(right).take_while(|(a, b)| a == b).count()
                };
        }
        count += n;
        if reverse {
            x = &x[..x.len() - n];
            y = &y[..y.len() - n];
        } else {
            x = &x[n..];
            y = &y[n..];
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bounding_changes_reconstruct_unicode_documents_across_chunks() {
        let source = format!("{}é👩🏽‍💻世界\n{}", "a".repeat(5000), "b".repeat(5000));
        let before = Rope::from_str(&source);
        for start in [0, 3, 4999, 5000, 5001, 5004, before.len_chars()] {
            for removed in [0, 1, 4] {
                if start + removed > before.len_chars() {
                    continue;
                }
                for insert in ["", "é", "è", "👨‍💻", "\nnew\n"] {
                    let mut after = before.clone();
                    after.remove(start..start + removed);
                    after.insert(start, insert);
                    if let Some((range, text)) = difference(&before, &after) {
                        let mut reconstructed = before.clone();
                        reconstructed.remove(range.clone());
                        reconstructed.insert(range.start, &text);
                        assert_eq!(reconstructed, after);
                    } else {
                        assert_eq!(before, after);
                    }
                }
            }
        }
        assert!(difference(&before, &before.clone()).is_none());
        for (a, b) in [
            ("é", "è"),
            ("👩", "👨"),
            ("", "x"),
            ("x", ""),
            ("ab", "acb"),
        ] {
            let before = Rope::from_str(a);
            let after = Rope::from_str(b);
            let (range, text) = difference(&before, &after).unwrap();
            let mut applied = before;
            applied.remove(range.clone());
            applied.insert(range.start, &text);
            assert_eq!(applied, after);
        }
    }
}
