//! Classification of individual source lines.
//!
//! ArchieML is parsed line by line: every line is either a command line or plain text.
//! Whether a line counts as a command depends on the context it appears in (for
//! example, `* item` is only a command inside a string array), so classification takes
//! a [`Context`] describing what the enclosing scope accepts.
//!
//! Where the spec is silent, the rules here mirror the reference implementation,
//! [archieml-js](https://github.com/newsdev/archieml-js).

use std::ops::Range;

/// A `:command` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Command {
    End,
    Ignore,
    Skip,
    EndSkip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Bracket {
    /// `{object}`
    Curly,
    /// `[array]`
    Square,
}

/// The meaning of a single line. Ranges are byte offsets relative to the line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Line<'a> {
    Command(Command),
    /// `key: value`
    Key {
        key: &'a str,
        value: Range<usize>,
    },
    /// `* value`
    Bullet {
        value: Range<usize>,
    },
    /// `{key}` or `[key]`, optionally with the `.` (nested) and `+` (freeform) flags.
    Open {
        bracket: Bracket,
        nested: bool,
        freeform: bool,
        key: &'a str,
    },
    /// `{}` or `[]`
    Close,
    Text,
}

/// Which kinds of lines the enclosing scope treats as commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Context {
    /// `key: value` lines.
    pub keys: bool,
    /// `* value` lines.
    pub bullets: bool,
}

/// Whitespace as matched by JavaScript's `\s`, which the reference implementation uses
/// for trimming.
pub(crate) fn is_whitespace(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{B}' | '\u{C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
    )
}

/// Characters that may appear in a key: anything except the reference implementation's
/// list of whitespace and the punctuation used by ArchieML itself. This follows the
/// conformance suite, which (unlike the written spec) allows arbitrary Unicode in keys.
fn is_key_char(c: char) -> bool {
    !matches!(
        c,
        '\0' | '\t' | '\n' | '\u{B}' | '\u{C}' | '\r' | ' ' | '\u{A0}' | '\u{2000}'
            ..='\u{200B}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202F}'
                | '\u{205F}'
                | '\u{3000}'
                | '\u{FEFF}'
                | '['
                | '\\'
                | ']'
                | '{'
                | '}'
                | ':'
    )
}

/// The spec forbids periods at either end of a key. It doesn't address consecutive
/// periods, so `a..b` is accepted, as in the reference implementation, and has an empty
/// segment.
fn is_valid_key(key: &str) -> bool {
    !key.starts_with('.') && !key.ends_with('.')
}

pub(crate) fn trim(s: &str) -> &str {
    s.trim_matches(is_whitespace)
}

fn trim_start(s: &str) -> &str {
    s.trim_start_matches(is_whitespace)
}

/// A forward-only position within a line. Scanners consume the line through it, so the
/// byte offset of anything they find is simply where the cursor stood.
#[derive(Debug, Clone, Copy)]
struct Cursor<'a> {
    line: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(line: &'a str) -> Self {
        Cursor { line, pos: 0 }
    }

    fn rest(&self) -> &'a str {
        self.line.get(self.pos..).unwrap_or_default()
    }

    /// Consumes and returns the longest prefix of characters matching `pred`.
    fn take_while(&mut self, mut pred: impl FnMut(char) -> bool) -> &'a str {
        let rest = self.rest();
        let len = rest.find(|c| !pred(c)).unwrap_or(rest.len());
        self.pos += len;
        rest.get(..len).unwrap_or_default()
    }

    fn skip_whitespace(&mut self) {
        self.take_while(is_whitespace);
    }

    fn skip_inline_whitespace(&mut self) {
        self.take_while(|c| c == ' ' || c == '\t');
    }

    /// Consumes `c` if it comes next.
    fn eat(&mut self, c: char) -> bool {
        let found = self.rest().starts_with(c);
        if found {
            self.pos += c.len_utf8();
        }
        found
    }

    /// The range from the cursor to the end of the line.
    fn remaining(&self) -> Range<usize> {
        self.pos..self.line.len()
    }
}

/// Whether a plain-text line starts (after whitespace) with an escaping backslash.
pub(crate) fn is_escaped(line: &str) -> bool {
    trim_start(line).starts_with('\\')
}

pub(crate) fn classify<'a>(line: &'a str, ctx: Context) -> Line<'a> {
    if let Some(command) = scan_command(line) {
        return Line::Command(command);
    }
    // Keys take precedence over bullets, so `*key: value` is a key.
    if ctx.keys
        && let Some((key, value)) = scan_key(line)
    {
        return Line::Key { key, value };
    }
    if ctx.bullets
        && let Some(value) = scan_bullet(line)
    {
        return Line::Bullet { value };
    }
    scan_scope(line).unwrap_or(Line::Text)
}

/// `:end`, `:ignore`, `:skip` or `:endskip`, case-insensitively, with anything allowed
/// to follow the command word (`:endthis` is an `:end`).
pub(crate) fn scan_command(line: &str) -> Option<Command> {
    let mut cursor = Cursor::new(line);
    cursor.skip_whitespace();
    if !cursor.eat(':') {
        return None;
    }
    cursor.skip_inline_whitespace();
    let rest = cursor.rest().as_bytes();
    let starts_with = |word: &str| {
        rest.get(..word.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(word.as_bytes()))
    };
    // `endskip` must be checked before `end`.
    [
        ("endskip", Command::EndSkip),
        ("ignore", Command::Ignore),
        ("skip", Command::Skip),
        ("end", Command::End),
    ]
    .into_iter()
    .find_map(|(word, command)| starts_with(word).then_some(command))
}

fn scan_key(line: &str) -> Option<(&str, Range<usize>)> {
    let mut cursor = Cursor::new(line);
    cursor.skip_whitespace();
    let key = cursor.take_while(is_key_char);
    cursor.skip_inline_whitespace();
    if !cursor.eat(':') || key.is_empty() || !is_valid_key(key) {
        return None;
    }
    cursor.skip_inline_whitespace();
    Some((key, cursor.remaining()))
}

fn scan_bullet(line: &str) -> Option<Range<usize>> {
    let mut cursor = Cursor::new(line);
    cursor.skip_whitespace();
    if !cursor.eat('*') {
        return None;
    }
    cursor.skip_inline_whitespace();
    Some(cursor.remaining())
}

fn scan_scope(line: &str) -> Option<Line<'_>> {
    let mut cursor = Cursor::new(line);
    cursor.skip_whitespace();
    // The closing bracket needn't match the opening one: `{key]` opens an object.
    let bracket = if cursor.eat('{') {
        Bracket::Curly
    } else if cursor.eat('[') {
        Bracket::Square
    } else {
        return None;
    };
    cursor.skip_inline_whitespace();
    let flags = cursor.take_while(|c| c == '+' || c == '.');
    cursor.skip_inline_whitespace();
    let key = cursor.take_while(is_key_char);
    cursor.skip_inline_whitespace();
    if !cursor.eat(']') && !cursor.eat('}') {
        return None;
    }
    if key.is_empty() {
        return Some(Line::Close);
    }

    if !is_valid_key(key) {
        return None;
    }
    Some(Line::Open {
        bracket,
        nested: flags.contains('.'),
        freeform: flags.contains('+'),
        key,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: Context = Context {
        keys: true,
        bullets: false,
    };

    #[test]
    fn commands() {
        assert_eq!(scan_command(":end"), Some(Command::End));
        assert_eq!(scan_command("  :EnD this"), Some(Command::End));
        assert_eq!(scan_command(":endskip"), Some(Command::EndSkip));
        assert_eq!(scan_command(": skipthis"), Some(Command::Skip));
        assert_eq!(scan_command(":ignore"), Some(Command::Ignore));
        assert_eq!(scan_command(":notacommand"), None);
        assert_eq!(scan_command("\\:end"), None);
    }

    #[test]
    fn keys() {
        assert_eq!(
            classify("  key  :  value ", DEFAULT),
            Line::Key {
                key: "key",
                value: 10..16
            }
        );
        assert_eq!(classify("k ey: value", DEFAULT), Line::Text);
        assert_eq!(classify(".key: value", DEFAULT), Line::Text);
        assert!(matches!(
            classify("a..b: value", DEFAULT),
            Line::Key { key: "a..b", .. }
        ));
        let bullets = Context {
            bullets: true,
            ..DEFAULT
        };
        assert!(matches!(
            classify("*key: value", bullets),
            Line::Key { key: "*key", .. }
        ));
        assert!(matches!(
            classify("* key: value", bullets),
            Line::Bullet { .. }
        ));
        assert!(matches!(
            classify("π: 3.14", DEFAULT),
            Line::Key { key: "π", .. }
        ));
    }

    #[test]
    fn scopes() {
        assert_eq!(
            classify("[ .+ items ] trailing", DEFAULT),
            Line::Open {
                bracket: Bracket::Square,
                nested: true,
                freeform: true,
                key: "items"
            }
        );
        assert_eq!(classify("  {  }  ", DEFAULT), Line::Close);
        assert_eq!(
            classify("{scope]", DEFAULT),
            Line::Open {
                bracket: Bracket::Curly,
                nested: false,
                freeform: false,
                key: "scope"
            }
        );
        assert_eq!(classify("[}", DEFAULT), Line::Close);
        assert_eq!(classify("[scope.]", DEFAULT), Line::Text);
    }
}
