//! Inline comments, deprecated in the spec (§7) and enabled by
//! [`Options::ENABLE_INLINE_COMMENTS`](crate::Options::ENABLE_INLINE_COMMENTS).

use std::borrow::Cow;

/// Removes inline comments (deprecated in the spec), exactly as archieml-js does: first
/// every `[comment]` on a single line, along with a backslash before it at the start of
/// a line, unless it is part of `[[...]]`; then `[[text]]` becomes `[text]`.
pub(crate) fn strip_comments(s: &str) -> Cow<'_, str> {
    if !s.contains('[') {
        return Cow::Borrowed(s);
    }
    let without_comments = remove_comments(s);
    match unescape_brackets(&without_comments) {
        Cow::Owned(unescaped) => Cow::Owned(unescaped),
        Cow::Borrowed(_) => without_comments,
    }
}

fn is_comment_char(b: u8) -> bool {
    !matches!(b, b'[' | b']' | b'\n' | b'\r')
}

/// The end of a bracketed run starting at `open` (`[^\[\]\n\r]*\]`), just past the `]`.
fn bracketed(bytes: &[u8], open: usize) -> Option<usize> {
    let start = open.checked_add(1)?;
    let run = bytes.get(start..)?;
    let close = start + run.iter().take_while(|&&b| is_comment_char(b)).count();
    (bytes.get(close) == Some(&b']')).then_some(close + 1)
}

/// `/(?:^\\)?\[[^\[\]\n\r]*\](?!\])/mg` → `""`
fn remove_comments(s: &str) -> Cow<'_, str> {
    let bytes = s.as_bytes();
    let mut out = String::new();
    let (mut copied, mut i) = (0, 0);
    while let Some(&byte) = bytes.get(i) {
        // JavaScript's multiline `^` also matches after U+2028 and U+2029.
        let before = bytes.get(..i).unwrap_or_default();
        let line_start = matches!(
            before,
            [] | [.., b'\n' | b'\r'] | [.., 0xE2, 0x80, 0xA8 | 0xA9]
        );
        let open = match byte {
            b'\\' if line_start && bytes.get(i + 1) == Some(&b'[') => i + 1,
            b'[' => i,
            _ => {
                i += 1;
                continue;
            }
        };
        match bracketed(bytes, open) {
            Some(end) if bytes.get(end) != Some(&b']') => {
                out.push_str(s.get(copied..i).unwrap_or_default());
                copied = end;
                i = end;
            }
            _ => i += 1,
        }
    }
    if copied == 0 {
        return Cow::Borrowed(s);
    }
    out.push_str(s.get(copied..).unwrap_or_default());
    Cow::Owned(out)
}

/// `/\[\[([^\[\]\n\r]*)\]\]/g` → `"[$1]"`
fn unescape_brackets(s: &str) -> Cow<'_, str> {
    let bytes = s.as_bytes();
    let mut out = String::new();
    let (mut copied, mut i) = (0, 0);
    while let Some(pair) = bytes.get(i..i + 2) {
        if pair == b"[["
            && let Some(end) = bracketed(bytes, i + 1)
            && bytes.get(end) == Some(&b']')
        {
            out.push_str(s.get(copied..i).unwrap_or_default());
            // `[[text]]` becomes `[text]`.
            out.push_str(s.get(i + 1..end).unwrap_or_default());
            copied = end + 1;
            i = end + 1;
        } else {
            i += 1;
        }
    }
    if copied == 0 {
        return Cow::Borrowed(s);
    }
    out.push_str(s.get(copied..).unwrap_or_default());
    Cow::Owned(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn comments() {
        assert_eq!(strip_comments("value [note] more"), "value  more");
        assert_eq!(strip_comments("a [[b]] c"), "a [b] c");
        assert_eq!(strip_comments("a [b]] c"), "a [b]] c");
        assert_eq!(strip_comments("[[a [b] c]]"), "[a  c]");
        assert_eq!(strip_comments("\\[note] text\n\\[x]"), " text\n");
        assert_eq!(strip_comments("x \\[note]"), "x \\");
        assert_eq!(strip_comments("[open\nclose]"), "[open\nclose]");
        assert!(matches!(strip_comments("plain"), Cow::Borrowed(_)));
        assert_eq!(strip_comments("π [c] π"), "π  π");
        assert_eq!(strip_comments("a\u{2028}\\[c]b"), "a\u{2028}b");
    }
}
