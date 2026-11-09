//! The pull parser and the events it produces.

use std::borrow::Cow;
use std::collections::VecDeque;
use std::fmt;
use std::iter::FusedIterator;
use std::ops::Range;

use crate::scanners::{self, Bracket, Command, Context, Line};

/// A key exactly as written in the source, which may use dot-notation (`a.b.c`).
///
/// Outside of freeform arrays, each dot introduces a level of nesting; see
/// [`Key::segments`]. Inside freeform arrays the whole key is used verbatim as the
/// element's `type`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Key<'a>(&'a str);

impl<'a> Key<'a> {
    /// The key as written, including any dots.
    pub fn as_str(&self) -> &'a str {
        self.0
    }

    /// The dot-separated segments of the key. The first and last segments are never
    /// empty, but inner ones can be: `a..b` has the segments `a`, `` and `b`.
    pub fn segments(&self) -> std::str::Split<'a, char> {
        self.0.split('.')
    }
}

impl fmt::Display for Key<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}

impl PartialEq<str> for Key<'_> {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for Key<'_> {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

/// A container opened by an [`Event::Start`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tag<'a> {
    /// An object block: `{key}`, or `{.key}` when `nested`.
    ///
    /// A top-level block is located relative to the document root and merges into any
    /// object already at that key. A nested block is located relative to the current
    /// scope (the root, if no block is open) and replaces any existing value. Subsequent
    /// keys are relative to the object until the matching [`TagEnd::Object`].
    Object { key: Key<'a>, nested: bool },
    /// An array: `[key]`, `[.key]` when `nested`, and `[+key]` or `[.+key]` when
    /// `freeform`. Replaces any value already at that key.
    ///
    /// Whether a non-freeform array holds strings or objects is decided by its first
    /// entry: [`Event::Item`]s for strings, [`Tag::Element`]s for objects.
    Array {
        key: Key<'a>,
        nested: bool,
        freeform: bool,
    },
    /// One object inside an object array. A new element begins every time the array's
    /// first key (its delimiter) is encountered again.
    Element,
}

impl Tag<'_> {
    /// The [`TagEnd`] that closes this tag.
    pub fn to_end(&self) -> TagEnd {
        match self {
            Tag::Object { .. } => TagEnd::Object,
            Tag::Array { .. } => TagEnd::Array,
            Tag::Element => TagEnd::Element,
        }
    }
}

/// The end of a container opened by [`Event::Start`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TagEnd {
    Object,
    Array,
    Element,
}

/// An event produced by the [`Parser`].
///
/// `Start` and `End` events are always balanced, even when the source closes blocks
/// implicitly or ends abruptly. Values have already had surrounding whitespace removed,
/// and multi-line values have been joined and unescaped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event<'a> {
    Start(Tag<'a>),
    End(TagEnd),
    /// A `key: value` line, possibly continued over several lines up to an `:end`.
    ///
    /// The key is relative to the innermost open object or element. Inside a freeform
    /// array this is an element whose `type` is the key.
    Pair(Key<'a>, Cow<'a, str>),
    /// A `* value` entry of a string array.
    Item(Cow<'a, str>),
    /// A line of plain text inside a freeform array: an element whose `type` is `text`.
    Text(Cow<'a, str>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArrayKind {
    /// Nothing has been added yet.
    Undetermined,
    Objects,
    Strings,
    Freeform,
}

#[derive(Debug)]
struct ArrayFrame<'a> {
    kind: ArrayKind,
    /// The first key used in an object array; seeing it again starts a new element.
    delimiter: Option<&'a str>,
    in_element: bool,
}

#[derive(Debug)]
enum Frame<'a> {
    /// `freeform` objects (`{+key}`) store every key, and every nested block, under
    /// `value`, as the reference implementation does.
    Object {
        freeform: bool,
    },
    Array(ArrayFrame<'a>),
}

/// The key that keys and nested blocks inside a `{+key}` object are stored under.
const FREEFORM_OBJECT_KEY: Key<'static> = Key("value");

/// A pull parser for ArchieML, yielding [`Event`]s.
///
/// The parser never fails: lines that aren't valid commands in their context are treated
/// as plain text, which is ignored outside of multi-line values and freeform arrays.
///
/// ```
/// use archier::{Event, Parser, Tag, TagEnd};
///
/// let events: Vec<_> = Parser::new("[colors]\n* red\n* blue\n[]").collect();
/// assert!(matches!(&events[0], Event::Start(Tag::Array { key, .. }) if *key == "colors"));
/// assert_eq!(events[1], Event::Item("red".into()));
/// assert_eq!(events[2], Event::Item("blue".into()));
/// assert_eq!(events[3], Event::End(TagEnd::Array));
/// ```
#[derive(Debug)]
pub struct Parser<'a> {
    text: &'a str,
    /// Start of the next unread line.
    pos: usize,
    stack: Vec<Frame<'a>>,
    skipping: bool,
    finished: bool,
    queue: VecDeque<(Event<'a>, Range<usize>)>,
}

impl<'a> Parser<'a> {
    pub fn new(text: &'a str) -> Self {
        Parser {
            text,
            pos: 0,
            stack: Vec::new(),
            skipping: false,
            finished: false,
            queue: VecDeque::new(),
        }
    }

    /// Converts into an iterator that also yields the byte range of the source that
    /// produced each event. Events that the source implies without spelling out (such as
    /// closing an array when another one opens) have an empty range.
    pub fn into_offset_iter(self) -> OffsetIter<'a> {
        OffsetIter { inner: self }
    }

    fn next_event(&mut self) -> Option<(Event<'a>, Range<usize>)> {
        while self.queue.is_empty() && !self.finished {
            self.step();
        }
        self.queue.pop_front()
    }

    fn emit(&mut self, event: Event<'a>, range: Range<usize>) {
        self.queue.push_back((event, range));
    }

    /// Returns the content range of the line starting at `pos` (without its line ending),
    /// and the start of the following line. Lines end with `\n`, `\r\n` or `\r`.
    fn line_at(&self, pos: usize) -> Option<(Range<usize>, usize)> {
        let rest = self.text.get(pos..).filter(|rest| !rest.is_empty())?;
        let Some(i) = rest.find(['\n', '\r']) else {
            return Some((pos..self.text.len(), self.text.len()));
        };
        let terminator = if rest[i..].starts_with("\r\n") { 2 } else { 1 };
        Some((pos..pos + i, pos + i + terminator))
    }

    fn context(&self) -> Context {
        let kind = match self.stack.last() {
            Some(Frame::Array(array)) => array.kind,
            _ => {
                return Context {
                    keys: true,
                    bullets: false,
                };
            }
        };
        Context {
            keys: kind != ArrayKind::Strings,
            bullets: matches!(kind, ArrayKind::Undetermined | ArrayKind::Strings),
        }
    }

    fn step(&mut self) {
        let Some((range, next)) = self.line_at(self.pos) else {
            self.finish(self.text.len());
            return;
        };
        self.pos = next;
        let text = self.text;
        let line = &text[range.clone()];
        let at = |r: Range<usize>| range.start + r.start..range.start + r.end;

        if self.skipping {
            match scanners::scan_command(line) {
                Some(Command::EndSkip) => self.skipping = false,
                Some(Command::Ignore) => self.finish(range.start),
                _ => {}
            }
            return;
        }

        match scanners::classify(line, self.context()) {
            Line::Command(Command::Skip) => self.skipping = true,
            Line::Command(Command::Ignore) => self.finish(range.start),
            // An `:end` that doesn't follow a value has no effect; ones that do are
            // consumed while reading the value.
            Line::Command(Command::End | Command::EndSkip) => {}
            Line::Key { key, value } => self.key(Key(key), at(value), range, next),
            Line::Bullet { value } => self.bullet(at(value), range, next),
            Line::Open {
                bracket,
                nested,
                freeform,
                key,
            } => self.open(bracket, nested, freeform, Key(key), range),
            Line::Close => {
                if !self.stack.is_empty() {
                    self.pop(range);
                }
            }
            Line::Text => {
                if let Some(Frame::Array(ArrayFrame {
                    kind: ArrayKind::Freeform,
                    ..
                })) = self.stack.last()
                {
                    let value = scanners::trim(line);
                    if !value.is_empty() {
                        self.emit(Event::Text(value.into()), range);
                    }
                }
            }
        }
    }

    fn key(&mut self, key: Key<'a>, value: Range<usize>, line: Range<usize>, next: usize) {
        let freeform_key = match self.stack.last_mut() {
            Some(Frame::Array(array)) if array.kind == ArrayKind::Freeform => Some(key),
            Some(Frame::Object { freeform: true }) => Some(FREEFORM_OBJECT_KEY),
            Some(Frame::Array(array)) => {
                array.kind = ArrayKind::Objects;
                self.delimit(key.as_str(), line.start);
                None
            }
            _ => None,
        };
        if let Some(key) = freeform_key {
            // Plain-text lines in freeform blocks don't belong to the preceding value (in
            // freeform arrays they are elements of their own), so it is a single line.
            let value = scanners::trim(&self.text[value]);
            self.emit(Event::Pair(key, value.into()), line);
            return;
        }
        let (value, end) = self.value(value, next);
        self.emit(Event::Pair(key, value), line.start..end);
    }

    fn bullet(&mut self, value: Range<usize>, line: Range<usize>, next: usize) {
        if let Some(Frame::Array(array)) = self.stack.last_mut() {
            array.kind = ArrayKind::Strings;
        }
        let (value, end) = self.value(value, next);
        self.emit(Event::Item(value), line.start..end);
    }

    fn open(
        &mut self,
        bracket: Bracket,
        nested: bool,
        freeform: bool,
        key: Key<'a>,
        line: Range<usize>,
    ) {
        let mut key = key;
        if nested {
            match self.stack.last_mut() {
                Some(Frame::Array(array))
                    if matches!(array.kind, ArrayKind::Undetermined | ArrayKind::Objects) =>
                {
                    array.kind = ArrayKind::Objects;
                    self.delimit(key.as_str(), line.start);
                }
                Some(Frame::Object { freeform: true }) => key = FREEFORM_OBJECT_KEY,
                _ => {}
            }
        } else {
            self.close_all(line.start);
        }

        let (tag, frame) = match bracket {
            Bracket::Curly => (Tag::Object { key, nested }, Frame::Object { freeform }),
            Bracket::Square => (
                Tag::Array {
                    key,
                    nested,
                    freeform,
                },
                Frame::Array(ArrayFrame {
                    kind: if freeform {
                        ArrayKind::Freeform
                    } else {
                        ArrayKind::Undetermined
                    },
                    delimiter: None,
                    in_element: false,
                }),
            ),
        };
        self.emit(Event::Start(tag), line);
        self.stack.push(frame);
    }

    /// Starts a new element in the current object array if `key` is its delimiter (or
    /// the array has no elements yet).
    fn delimit(&mut self, key: &'a str, at: usize) {
        let Some(Frame::Array(array)) = self.stack.last_mut() else {
            return;
        };
        match array.delimiter {
            None => {
                array.delimiter = Some(key);
                array.in_element = true;
            }
            Some(delimiter) if delimiter == key => self.emit(Event::End(TagEnd::Element), at..at),
            Some(_) => return,
        }
        self.emit(Event::Start(Tag::Element), at..at);
    }

    /// Reads the value that starts at `first` (the rest of a key or bullet line). If the
    /// next command line is an `:end`, the plain-text lines up to it are part of the
    /// value; otherwise the value is only the first line. Either way, the parser resumes
    /// at the next command line, since the plain text in between has no other effect.
    ///
    /// Returns the value and the end of the source it was read from.
    fn value(&mut self, first: Range<usize>, next: usize) -> (Cow<'a, str>, usize) {
        let text = self.text;
        let ctx = self.context();
        let mut pos = next;
        let mut end = first.end;
        let mut escaped = false;
        while let Some((line, after)) = self.line_at(pos) {
            let content = &text[line.clone()];
            match scanners::classify(content, ctx) {
                Line::Text => {
                    escaped |= scanners::is_escaped(content);
                    end = line.end;
                    pos = after;
                }
                Line::Command(Command::End) => {
                    self.pos = after;
                    return (multi_line(text, first, next, end, escaped), line.end);
                }
                _ => break,
            }
        }
        self.pos = pos;
        (scanners::trim(&text[first.clone()]).into(), first.end)
    }

    fn pop(&mut self, range: Range<usize>) {
        match self.stack.pop() {
            Some(Frame::Object { .. }) => self.emit(Event::End(TagEnd::Object), range),
            Some(Frame::Array(array)) => {
                if array.in_element {
                    self.emit(Event::End(TagEnd::Element), range.clone());
                }
                self.emit(Event::End(TagEnd::Array), range);
            }
            None => {}
        }
    }

    fn close_all(&mut self, at: usize) {
        while !self.stack.is_empty() {
            self.pop(at..at);
        }
    }

    fn finish(&mut self, at: usize) {
        self.close_all(at);
        self.finished = true;
    }
}

/// Builds a multi-line value from the rest of its first line (`first`) through `end`,
/// where `body` is the start of the second line.
///
/// Leading whitespace on the first line and trailing whitespace on the last are removed,
/// and so is one leading backslash on every line after the first.
fn multi_line(
    text: &str,
    first: Range<usize>,
    body: usize,
    end: usize,
    escaped: bool,
) -> Cow<'_, str> {
    let head = &text[first.clone()];
    let lead = head.len() - head.trim_start_matches(scanners::is_whitespace).len();
    let start = if lead == head.len() {
        body
    } else {
        first.start + lead
    };
    if start >= end {
        return Cow::Borrowed("");
    }
    if !escaped {
        return Cow::Borrowed(text[start..end].trim_end_matches(scanners::is_whitespace));
    }

    let body = body.max(start);
    let mut value = String::with_capacity(end - start);
    value.push_str(&text[start..body]);
    for line in lines_inclusive(&text[body..end]) {
        let indent = line.len() - line.trim_start_matches(scanners::is_whitespace).len();
        match line[indent..].strip_prefix('\\') {
            Some(rest) => {
                value.push_str(&line[..indent]);
                value.push_str(rest);
            }
            None => value.push_str(line),
        }
    }
    value.truncate(value.trim_end_matches(scanners::is_whitespace).len());
    Cow::Owned(value)
}

/// Splits `text` into lines that keep their `\n`, `\r\n` or `\r` terminators.
fn lines_inclusive(text: &str) -> impl Iterator<Item = &str> {
    let mut rest = text;
    std::iter::from_fn(move || {
        if rest.is_empty() {
            return None;
        }
        let end = match rest.find(['\n', '\r']) {
            Some(i) if rest[i..].starts_with("\r\n") => i + 2,
            Some(i) => i + 1,
            None => rest.len(),
        };
        let (line, tail) = rest.split_at(end);
        rest = tail;
        Some(line)
    })
}

impl<'a> Iterator for Parser<'a> {
    type Item = Event<'a>;

    fn next(&mut self) -> Option<Event<'a>> {
        self.next_event().map(|(event, _)| event)
    }
}

impl FusedIterator for Parser<'_> {}

/// An iterator over [`Event`]s and the byte ranges of the source that produced them;
/// see [`Parser::into_offset_iter`].
#[derive(Debug)]
pub struct OffsetIter<'a> {
    inner: Parser<'a>,
}

impl<'a> Iterator for OffsetIter<'a> {
    type Item = (Event<'a>, Range<usize>);

    fn next(&mut self) -> Option<Self::Item> {
        self.inner.next_event()
    }
}

impl FusedIterator for OffsetIter<'_> {}

#[cfg(test)]
mod tests {
    use super::*;

    fn events(text: &str) -> Vec<Event<'_>> {
        Parser::new(text).collect()
    }

    fn pair<'a>(key: &'a str, value: &'a str) -> Event<'a> {
        Event::Pair(Key(key), value.into())
    }

    #[test]
    fn single_line_values_borrow() {
        let events = events("key: value\nignored text\nother:  x  ");
        assert_eq!(events, [pair("key", "value"), pair("other", "x")]);
        assert!(matches!(&events[0], Event::Pair(_, Cow::Borrowed(_))));
    }

    #[test]
    fn multi_line_values() {
        let events = events("key: one  \n two\n\n:end\n");
        assert_eq!(events, [pair("key", "one  \n two")]);
        assert!(matches!(&events[0], Event::Pair(_, Cow::Borrowed(_))));

        let events = Parser::new("key: one\n  \\:end\n\\\\x\n:end").collect::<Vec<_>>();
        assert_eq!(events, [pair("key", "one\n  :end\n\\x")]);

        assert_eq!(
            Parser::new("key:\nsecond\n:end").collect::<Vec<_>>(),
            [pair("key", "second")]
        );
        assert_eq!(
            Parser::new("key: \n:end").collect::<Vec<_>>(),
            [pair("key", "")]
        );
    }

    #[test]
    fn crlf_line_endings() {
        let events = events("key: one\r\ntwo\r\n:end\r\n");
        assert_eq!(events, [pair("key", "one\r\ntwo")]);
    }

    #[test]
    fn object_arrays_split_on_delimiter() {
        let events = events("[list]\na: 1\nb: 2\na: 3");
        assert_eq!(
            events,
            [
                Event::Start(Tag::Array {
                    key: Key("list"),
                    nested: false,
                    freeform: false
                }),
                Event::Start(Tag::Element),
                pair("a", "1"),
                pair("b", "2"),
                Event::End(TagEnd::Element),
                Event::Start(Tag::Element),
                pair("a", "3"),
                Event::End(TagEnd::Element),
                Event::End(TagEnd::Array),
            ]
        );
    }

    #[test]
    fn opening_a_block_closes_open_ones() {
        let events = events("[a]\n[.b]\n* x\n{c}\nk: v\n:ignore\nmore: stuff");
        assert_eq!(
            events,
            [
                Event::Start(Tag::Array {
                    key: Key("a"),
                    nested: false,
                    freeform: false
                }),
                Event::Start(Tag::Element),
                Event::Start(Tag::Array {
                    key: Key("b"),
                    nested: true,
                    freeform: false
                }),
                Event::Item("x".into()),
                Event::End(TagEnd::Array),
                Event::End(TagEnd::Element),
                Event::End(TagEnd::Array),
                Event::Start(Tag::Object {
                    key: Key("c"),
                    nested: false
                }),
                pair("k", "v"),
                Event::End(TagEnd::Object),
            ]
        );
    }

    #[test]
    fn line_endings() {
        assert_eq!(
            events("a: 1\rb: 2\r\nc: 3\nd: x\r\\:end\r:end"),
            [
                pair("a", "1"),
                pair("b", "2"),
                pair("c", "3"),
                pair("d", "x\r:end")
            ]
        );
    }

    #[test]
    fn freeform_objects_store_everything_under_value() {
        assert_eq!(
            events("{+obj}\nname: x\nignored text\n[.list]\n[]"),
            [
                Event::Start(Tag::Object {
                    key: Key("obj"),
                    nested: false
                }),
                pair("value", "x"),
                Event::Start(Tag::Array {
                    key: Key("value"),
                    nested: true,
                    freeform: false
                }),
                Event::End(TagEnd::Array),
                Event::End(TagEnd::Object),
            ]
        );
    }

    #[test]
    fn freeform_arrays() {
        let events = events("[+body]\nh1: Title\n  Some text.  \n* bullet\n\n[]");
        assert_eq!(
            events,
            [
                Event::Start(Tag::Array {
                    key: Key("body"),
                    nested: false,
                    freeform: true
                }),
                pair("h1", "Title"),
                Event::Text("Some text.".into()),
                Event::Text("* bullet".into()),
                Event::End(TagEnd::Array),
            ]
        );
    }

    #[test]
    fn skip_blocks() {
        assert_eq!(
            events("a: 1\n:skip\nb: 2\n:endskip\nc: 3"),
            [pair("a", "1"), pair("c", "3")]
        );
    }

    #[test]
    fn offsets() {
        let text = "{obj}\nkey: one\ntwo\n:end\n";
        let ranges: Vec<_> = Parser::new(text)
            .into_offset_iter()
            .map(|(_, range)| range)
            .collect();
        assert_eq!(ranges, [0..5, 6..23, 24..24]);
    }
}
