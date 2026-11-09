//! Betty's parser: turns lines into betty's instructions, and merges buffered text into
//! them, as its `Parser` class and the first pass of its `Assembler` do.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::ops::Range;

use super::pattern::Pattern;
use crate::parser::{Event, Key, Tag, TagEnd};
use crate::scanners::is_whitespace;

/// The document as betty sees it: with every `\r` removed and surrounding whitespace
/// trimmed. Positions are in that text; [`Source::original`] maps them back.
#[derive(Debug)]
struct Source<'a> {
    text: Cow<'a, str>,
    /// Where `\r`s were removed, as positions in `text`.
    removed: Vec<usize>,
}

impl<'a> Source<'a> {
    fn new(text: &'a str) -> Self {
        if !text.contains('\r') {
            return Source {
                text: Cow::Borrowed(text),
                removed: Vec::new(),
            };
        }
        let mut cleaned = String::with_capacity(text.len());
        let mut removed = Vec::new();
        for c in text.chars() {
            if c == '\r' {
                removed.push(cleaned.len());
            } else {
                cleaned.push(c);
            }
        }
        Source {
            text: Cow::Owned(cleaned),
            removed,
        }
    }

    fn str(&self) -> &str {
        &self.text
    }

    /// A piece of the text, borrowing from the original where it is unchanged.
    fn slice(&self, range: Range<usize>) -> Cow<'a, str> {
        match &self.text {
            Cow::Borrowed(text) => Cow::Borrowed(&text[range]),
            Cow::Owned(text) => Cow::Owned(text[range].to_owned()),
        }
    }

    fn original(&self, pos: usize) -> usize {
        pos + self.removed.partition_point(|&removed| removed <= pos)
    }
}

/// Text accumulated from pieces of the source, borrowing while they are contiguous.
#[derive(Debug, Clone)]
enum Text {
    Slice(Range<usize>),
    Owned(String),
}

impl Text {
    fn push_range(&mut self, range: Range<usize>, source: &Source<'_>) {
        match self {
            Text::Slice(slice) if slice.end == range.start || slice.start == slice.end => {
                if slice.start == slice.end {
                    *slice = range;
                } else {
                    slice.end = range.end;
                }
            }
            _ => self.push_str(&source.str()[range], source),
        }
    }

    fn push_str(&mut self, text: &str, source: &Source<'_>) {
        if let Text::Slice(slice) = self {
            *self = Text::Owned(source.str()[slice.clone()].to_owned());
        }
        if let Text::Owned(owned) = self {
            owned.push_str(text);
        }
    }

    fn push(&mut self, other: &Text, source: &Source<'_>) {
        match other {
            Text::Slice(range) => self.push_range(range.clone(), source),
            Text::Owned(text) => self.push_str(text, source),
        }
    }

    fn materialize<'a>(&self, source: &Source<'a>) -> Cow<'a, str> {
        match self {
            Text::Slice(range) => source.slice(range.clone()),
            Text::Owned(text) => Cow::Owned(text.clone()),
        }
    }
}

fn trim(text: Cow<'_, str>) -> Cow<'_, str> {
    match text {
        Cow::Borrowed(text) => Cow::Borrowed(text.trim_matches(is_whitespace)),
        Cow::Owned(text) => Cow::Owned(text.trim_matches(is_whitespace).to_owned()),
    }
}

fn trim_end(text: Cow<'_, str>) -> Cow<'_, str> {
    match text {
        Cow::Borrowed(text) => Cow::Borrowed(text.trim_end_matches(is_whitespace)),
        Cow::Owned(text) => Cow::Owned(text.trim_end_matches(is_whitespace).to_owned()),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    LeftBrace,
    RightBrace,
    LeftBracket,
    RightBracket,
    Colon,
    Star,
    /// The line ending. Betty gives it the type of text.
    Newline,
    Backslash,
    Slash,
    Text,
}

impl Kind {
    fn is_text(self) -> bool {
        matches!(self, Kind::Text | Kind::Newline)
    }
}

#[derive(Debug, Clone)]
struct Token {
    kind: Kind,
    range: Range<usize>,
}

/// Splits a line into betty's tokens: each punctuation character on its own, and runs
/// of anything else as text.
fn tokenize(text: &str, line: Range<usize>) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut run = line.start;
    for (offset, c) in text[line.clone()].char_indices() {
        let kind = match c {
            '{' => Kind::LeftBrace,
            '}' => Kind::RightBrace,
            '[' => Kind::LeftBracket,
            ']' => Kind::RightBracket,
            ':' => Kind::Colon,
            '*' => Kind::Star,
            '\n' => Kind::Newline,
            '\\' => Kind::Backslash,
            '/' => Kind::Slash,
            _ => continue,
        };
        let at = line.start + offset;
        if run < at {
            tokens.push(Token {
                kind: Kind::Text,
                range: run..at,
            });
        }
        tokens.push(Token {
            kind,
            range: at..at + 1,
        });
        run = at + 1;
    }
    if run < line.end {
        tokens.push(Token {
            kind: Kind::Text,
            range: run..line.end,
        });
    }
    tokens
}

/// A betty instruction, as produced by its parser.
#[derive(Debug)]
enum Instruction<'a> {
    Value { key: Cow<'a, str>, value: Text },
    Simple { value: Text },
    Object(Cow<'a, str>),
    CloseObject(Option<Cow<'a, str>>),
    Array { path: Cow<'a, str>, freeform: bool },
    CloseArray(Option<Cow<'a, str>>),
    Buffer(Text),
    Flush,
    Skipped,
}

/// The value betty's assembler holds as `lastValue`: text from later lines can still be
/// appended to it, so it is emitted only once it can't change.
#[derive(Debug)]
struct Pending<'a> {
    key: Option<Cow<'a, str>>,
    value: Text,
    start: usize,
    end: usize,
}

/// Betty's `[\+\.]{0,2}` and `[^+.].*`, searched for anywhere in an array's key.
/// Returns the byte ranges of the flags and the path.
fn array_flags(key: &str) -> Option<(Range<usize>, Range<usize>)> {
    let is_flag = |c: char| c == '+' || c == '.';
    for (start, _) in key.char_indices() {
        // Flags are ASCII, so lengths in characters and bytes agree.
        for flags in (0..=2).rev() {
            let path = start + flags;
            if !key.get(start..path).is_some_and(|f| f.chars().all(is_flag)) {
                continue;
            }
            let Some(first) = key[path..].chars().next().filter(|&c| !is_flag(c)) else {
                continue;
            };
            // `.` stops at line terminators.
            let rest = path + first.len_utf8();
            let end = key[rest..]
                .find(['\u{2028}', '\u{2029}'])
                .map_or(key.len(), |i| rest + i);
            return Some((start..path, path..end));
        }
    }
    None
}

/// A piece of a string, borrowing from the source if the string does.
fn sub_cow<'a>(text: &Cow<'a, str>, range: Range<usize>) -> Cow<'a, str> {
    match text {
        Cow::Borrowed(text) => Cow::Borrowed(&text[range]),
        Cow::Owned(text) => Cow::Owned(text[range].to_owned()),
    }
}

fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && !key
            .chars()
            .any(|c| is_whitespace(c) || matches!(c, '?' | '/' | '=' | '"' | '\''))
}

/// The pull parser's betty mode.
#[derive(Debug)]
pub(crate) struct BettyParser<'a> {
    source: Source<'a>,
    /// The start of the next line, and the end of the document.
    pos: usize,
    end: usize,
    finished: bool,
    pending: Option<Pending<'a>>,
    /// Betty's `buffer`, which is either empty or a list of strings to join.
    buffer: Option<Text>,
    queue: VecDeque<(Event<'a>, Range<usize>)>,
}

impl<'a> BettyParser<'a> {
    pub(crate) fn new(text: &'a str) -> Self {
        let source = Source::new(text);
        let full = source.str();
        let start = full.len() - full.trim_start_matches(is_whitespace).len();
        let end = full.trim_end_matches(is_whitespace).len().max(start);
        BettyParser {
            source,
            pos: start,
            end,
            finished: false,
            pending: None,
            buffer: None,
            queue: VecDeque::new(),
        }
    }

    pub(crate) fn next_event(&mut self) -> Option<(Event<'a>, Range<usize>)> {
        while self.queue.is_empty() && !self.finished {
            self.step();
        }
        self.queue.pop_front()
    }

    fn emit(&mut self, event: Event<'a>, range: Range<usize>) {
        let range = self.source.original(range.start)..self.source.original(range.end);
        self.queue.push_back((event, range));
    }

    /// The line starting at `pos`, including its `\n`.
    fn line_at(&self, pos: usize) -> Option<Range<usize>> {
        if pos >= self.end {
            return None;
        }
        let text = &self.source.str()[pos..self.end];
        Some(pos..text.find('\n').map_or(self.end, |i| pos + i + 1))
    }

    fn tokens(&self, line: Range<usize>) -> Vec<Token> {
        let mut tokens = tokenize(self.source.str(), line);
        // Betty drops blank tokens from the start of a line before matching it.
        let blank = tokens
            .iter()
            .take_while(|t| self.str(t).trim_matches(is_whitespace).is_empty())
            .count();
        tokens.drain(..blank);
        tokens
    }

    fn str(&self, token: &Token) -> &str {
        &self.source.str()[token.range.clone()]
    }

    fn trimmed(&self, token: &Token) -> Cow<'a, str> {
        trim(self.source.slice(token.range.clone()))
    }

    fn kinds_match(tokens: &[Token], kinds: &[Option<Kind>]) -> bool {
        tokens.len() >= kinds.len()
            && tokens.iter().zip(kinds).all(|(token, kind)| match kind {
                None => true,
                Some(Kind::Text) => token.kind.is_text(),
                Some(kind) => token.kind == *kind,
            })
    }

    /// `:word` at the start of the line, where the token after the colon starts with
    /// `word` in any case.
    fn is_command(&self, tokens: &[Token], word: &str) -> bool {
        tokens.len() > 1 && tokens[0].kind == Kind::Colon && {
            let next = self.str(&tokens[1]).as_bytes();
            next.len() >= word.len() && next[..word.len()].eq_ignore_ascii_case(word.as_bytes())
        }
    }

    fn step(&mut self) {
        let Some(line) = self.line_at(self.pos) else {
            self.finish();
            return;
        };
        let tokens = self.tokens(line.clone());
        let next = line.end;

        if self.is_command(&tokens, "ignore") {
            self.finish();
            return;
        }
        if self.is_command(&tokens, "skip") {
            // Skip up to, but not including, the `:endskip` line, which is then read as
            // plain text.
            let mut pos = line.start;
            while let Some(skipped) = self.line_at(pos) {
                if self.is_command(&self.tokens(skipped.clone()), "endskip") {
                    break;
                }
                pos = skipped.end;
            }
            self.pos = pos;
            self.instruction(Instruction::Skipped, line.start..pos);
            return;
        }
        self.pos = next;

        let t = Some(Kind::Text);
        if Self::kinds_match(&tokens, &[t, Some(Kind::Colon), Some(Kind::Colon), None])
            && let Some(key) = self.key(&tokens[0])
        {
            self.multiline_value(key, &tokens, line);
            return;
        }
        if Self::kinds_match(&tokens, &[t, Some(Kind::Colon), None])
            && let Some(key) = self.key(&tokens[0])
        {
            let value = Text::Slice(tokens[2].range.start..line.end);
            self.instruction(Instruction::Value { key, value }, line);
            return;
        }
        let instruction = if Self::kinds_match(&tokens, &[Some(Kind::Star), t]) {
            Instruction::Simple {
                value: Text::Slice(tokens[1].range.start..line.end),
            }
        } else if Self::kinds_match(&tokens, &[Some(Kind::LeftBrace), t, Some(Kind::RightBrace)]) {
            let key = self.trimmed(&tokens[1]);
            if key.is_empty() {
                Instruction::CloseObject(None)
            } else {
                Instruction::Object(key)
            }
        } else if Self::kinds_match(&tokens, &[Some(Kind::LeftBrace), Some(Kind::RightBrace)]) {
            Instruction::CloseObject(None)
        } else if Self::kinds_match(
            &tokens,
            &[
                Some(Kind::LeftBrace),
                Some(Kind::Slash),
                t,
                Some(Kind::RightBrace),
            ],
        ) {
            Instruction::CloseObject(Some(self.trimmed(&tokens[2])).filter(|k| !k.is_empty()))
        } else if Self::kinds_match(
            &tokens,
            &[Some(Kind::LeftBracket), t, Some(Kind::RightBracket)],
        ) {
            match self.array(&tokens[1]) {
                Some(instruction) => instruction,
                // Betty throws when an array's key is only `+` and `.`; read it as text.
                None => self.buffer_line(line.clone()),
            }
        } else if Self::kinds_match(
            &tokens,
            &[Some(Kind::LeftBracket), Some(Kind::RightBracket)],
        ) {
            Instruction::CloseArray(None)
        } else if Self::kinds_match(
            &tokens,
            &[
                Some(Kind::LeftBracket),
                Some(Kind::Slash),
                t,
                Some(Kind::RightBracket),
            ],
        ) {
            Instruction::CloseArray(Some(self.trimmed(&tokens[2])).filter(|k| !k.is_empty()))
        } else if self.is_command(&tokens, "end") && !self.is_command(&tokens, "endskip") {
            Instruction::Flush
        } else {
            self.buffer_line(line.clone())
        };
        self.instruction(instruction, line);
    }

    fn buffer_line(&self, line: Range<usize>) -> Instruction<'a> {
        Instruction::Buffer(Text::Slice(line))
    }

    fn key(&self, token: &Token) -> Option<Cow<'a, str>> {
        Some(self.trimmed(token)).filter(|key| is_valid_key(key))
    }

    fn array(&self, token: &Token) -> Option<Instruction<'a>> {
        let key = self.trimmed(token);
        if key.is_empty() {
            return Some(Instruction::CloseArray(None));
        }
        let (flags, path) = array_flags(&key)?;
        let flags = &key[flags];
        let freeform = flags.contains('+');
        let path = if flags.contains('.') {
            Cow::Owned(format!(".{}", &key[path]))
        } else {
            sub_cow(&key, path)
        };
        Some(Instruction::Array { path, freeform })
    }

    /// `key::` through the next `::key` line, which isn't included.
    fn multiline_value(&mut self, key: Cow<'a, str>, tokens: &[Token], line: Range<usize>) {
        let ender = key.to_lowercase();
        let pattern = Pattern::new(&ender);
        let mut pos = line.end;
        let mut end = self.end;
        while let Some(next) = self.line_at(pos) {
            let next_tokens = self.tokens(next.clone());
            let closes = next_tokens.len() > 2
                && next_tokens[0].kind == Kind::Colon
                && next_tokens[1].kind == Kind::Colon
                && {
                    let name = self.str(&next_tokens[2]);
                    match &pattern {
                        Some(pattern) => pattern.test(name),
                        // Betty throws on keys that aren't valid patterns; match them
                        // literally instead.
                        None => name.to_lowercase().contains(&ender),
                    }
                };
            if closes {
                end = next.start;
                pos = next.end;
                break;
            }
            pos = next.end;
        }
        self.pos = pos;
        let value = Text::Slice(tokens[3].range.start..end);
        self.instruction(Instruction::Value { key, value }, line.start..pos);
    }

    /// Betty's `mergeBuffer`: joins the buffer and removes the first backslash at the
    /// start of a line.
    fn merge_buffer(&self) -> Option<Cow<'a, str>> {
        let text = self.buffer.as_ref()?.materialize(&self.source);
        let mut line_start = true;
        let mut escape = None;
        for (i, c) in text.char_indices() {
            if line_start && c == '\\' {
                escape = Some(i);
                break;
            }
            line_start = matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}');
        }
        Some(match escape {
            Some(i) => Cow::Owned(format!("{}{}", &text[..i], &text[i + 1..])),
            None => text,
        })
    }

    /// Emits the merged buffer as freeform text, one event per non-blank line.
    fn emit_buffer(&mut self, merged: Cow<'a, str>, at: usize) {
        let lines: Vec<Cow<'a, str>> = match merged {
            Cow::Borrowed(text) => text.split('\n').map(Cow::Borrowed).collect(),
            Cow::Owned(text) => text.split('\n').map(|l| Cow::Owned(l.to_owned())).collect(),
        };
        for line in lines {
            let line = trim(line);
            if !line.is_empty() {
                self.emit(Event::Text(line), at..at);
            }
        }
    }

    fn push_buffer(&mut self, text: &Text) {
        match &mut self.buffer {
            Some(buffer) => buffer.push(text, &self.source),
            None => self.buffer = Some(text.clone()),
        }
    }

    fn push_buffer_str(&mut self, text: &str) {
        let owned = Text::Owned(text.to_owned());
        self.push_buffer(&owned);
    }

    /// Emits the pending value, which can no longer change.
    fn emit_pending(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let value = pending.value.materialize(&self.source);
        let event = match pending.key {
            Some(key) => Event::Pair(Key::from_cow(key), trim(value)),
            // Betty puts the text after `*` in freeform arrays as it is, so only the end
            // is trimmed here.
            None => Event::Item(trim_end(value)),
        };
        self.emit(event, pending.start..pending.end);
    }

    /// Emits the merged buffer, if it isn't blank, and empties it.
    fn flush_buffer_as_text(&mut self, at: usize) {
        if let Some(merged) = self.merge_buffer()
            && !merged.trim_matches(is_whitespace).is_empty()
        {
            self.emit_buffer(merged, at);
        }
        self.buffer = None;
    }

    /// The first pass of betty's assembler, which merges buffered text into values.
    fn instruction(&mut self, instruction: Instruction<'a>, range: Range<usize>) {
        let last_is_simple = matches!(&self.pending, Some(Pending { key: None, .. }));
        match instruction {
            Instruction::Simple { value } if self.pending.is_none() || last_is_simple => {
                self.buffer = None;
                self.emit_pending();
                self.pending = Some(Pending {
                    key: None,
                    value,
                    start: range.start,
                    end: range.end,
                });
            }
            // After a value, an item is just text.
            Instruction::Simple { value } => {
                self.push_buffer_str("*");
                self.push_buffer(&value);
            }
            Instruction::Buffer(text) => self.push_buffer(&text),
            // After an item, a value is just text too.
            Instruction::Value { key, value } if last_is_simple => {
                self.push_buffer_str(&format!("{key}:"));
                self.push_buffer(&value);
            }
            Instruction::Value { key, value } => {
                self.emit_pending();
                self.flush_buffer_as_text(range.start);
                self.pending = Some(Pending {
                    key: Some(key),
                    value,
                    start: range.start,
                    end: range.end,
                });
            }
            Instruction::Flush => {
                if let Some(merged) = self.merge_buffer() {
                    let blank = merged.trim_matches(is_whitespace).is_empty();
                    match &mut self.pending {
                        Some(pending) if !blank => {
                            let merged = Text::Owned(merged.into_owned());
                            pending.value.push(&merged, &self.source);
                            pending.end = range.end;
                        }
                        _ if !blank => self.emit_buffer(merged, range.start),
                        _ => {}
                    }
                }
                self.buffer = None;
            }
            Instruction::Skipped => self.emit_pending(),
            structural => {
                self.emit_pending();
                self.flush_buffer_as_text(range.start);
                let event = match structural {
                    Instruction::Object(key) => Event::Start(Tag::Object {
                        nested: key.starts_with('.'),
                        key: Key::from_cow(key),
                    }),
                    Instruction::Array { path, freeform } => Event::Start(Tag::Array {
                        nested: path.starts_with('.'),
                        key: Key::from_cow(path),
                        freeform,
                    }),
                    Instruction::CloseObject(None) => Event::End(TagEnd::Object),
                    Instruction::CloseObject(Some(name)) => {
                        Event::EndNamed(TagEnd::Object, Key::from_cow(name))
                    }
                    Instruction::CloseArray(None) => Event::End(TagEnd::Array),
                    Instruction::CloseArray(Some(name)) => {
                        Event::EndNamed(TagEnd::Array, Key::from_cow(name))
                    }
                    _ => unreachable!("handled above"),
                };
                self.emit(event, range);
            }
        }
    }

    fn finish(&mut self) {
        self.emit_pending();
        self.flush_buffer_as_text(self.end);
        self.finished = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn array_flags_match_bettys_pattern() {
        let flags = |key: &'static str| array_flags(key).map(|(f, p)| (&key[f], &key[p]));
        assert_eq!(flags("list"), Some(("", "list")));
        assert_eq!(flags("+.free.form"), Some(("+.", "free.form")));
        assert_eq!(flags(".+x"), Some((".+", "x")));
        assert_eq!(flags("+++x"), Some(("++", "x")));
        assert_eq!(flags("a+b"), Some(("", "a+b")));
        assert_eq!(flags("π+"), Some(("", "π+")));
        assert_eq!(flags("+."), None);
    }
}
