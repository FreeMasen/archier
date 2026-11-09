//! An in-memory document built from parser events.
//!
//! This is the ArchieML equivalent of pulldown-cmark's HTML renderer: a consumer of the
//! [`Event`] stream that applies ArchieML's semantics (dot-notation, overwriting
//! duplicate keys, merging reopened object blocks) to produce a tree of [`Value`]s.

use std::borrow::Cow;

use crate::parser::{Event, Key, Options, Parser, Tag, TagEnd};

/// An ArchieML value. All scalar values are strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    String(String),
    Array(Vec<Value>),
    Object(Map),
}

impl Value {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Map> {
        match self {
            Value::Object(map) => Some(map),
            _ => None,
        }
    }
}

/// An object whose keys keep the order in which they were first defined.
///
/// Equality ignores key order.
#[derive(Debug, Clone, Default, Eq)]
pub struct Map {
    entries: Vec<(String, Value)>,
}

impl Map {
    pub fn new() -> Self {
        Map::default()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn position(&self, key: &str) -> Option<usize> {
        self.entries.iter().position(|(k, _)| k == key)
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.position(key).map(|i| &self.entries[i].1)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value> {
        self.position(key).map(|i| &mut self.entries[i].1)
    }

    /// Sets `key` to `value`, returning the previous value. A replaced key keeps its
    /// original position.
    pub fn insert(&mut self, key: impl Into<String>, value: Value) -> Option<Value> {
        let key = key.into();
        match self.position(&key) {
            Some(i) => Some(std::mem::replace(&mut self.entries[i].1, value)),
            None => {
                self.entries.push((key, value));
                None
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Value> {
        self.position(key).map(|i| self.entries.remove(i).1)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &Value)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn keys(&self) -> impl ExactSizeIterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_str())
    }

    /// The object at `key`, replacing whatever non-object value is there.
    fn object_at(&mut self, key: &str) -> &mut Map {
        let i = match self.position(key) {
            Some(i) => {
                if !matches!(self.entries[i].1, Value::Object(_)) {
                    self.entries[i].1 = Value::Object(Map::new());
                }
                i
            }
            None => {
                self.entries
                    .push((key.to_owned(), Value::Object(Map::new())));
                self.entries.len() - 1
            }
        };
        match &mut self.entries[i].1 {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    /// The object holding the last segment of a dotted key, creating (or overwriting)
    /// intermediate objects as needed. Returns it along with that last segment.
    fn parent_of<'k>(&mut self, key: &'k str) -> (&mut Map, &'k str) {
        let mut segments = key.split('.');
        let mut last = segments.next().unwrap_or_default();
        let mut map = self;
        for segment in segments {
            map = map.object_at(last);
            last = segment;
        }
        (map, last)
    }
}

impl PartialEq for Map {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl<'a> IntoIterator for &'a Map {
    type Item = (&'a str, &'a Value);
    type IntoIter = Box<dyn Iterator<Item = (&'a str, &'a Value)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl<K: Into<String>> FromIterator<(K, Value)> for Map {
    fn from_iter<I: IntoIterator<Item = (K, Value)>>(iter: I) -> Self {
        let mut map = Map::new();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

impl std::ops::Index<&str> for Map {
    type Output = Value;

    fn index(&self, key: &str) -> &Value {
        self.get(key)
            .unwrap_or_else(|| panic!("no entry for key {key:?}"))
    }
}

/// Parses an ArchieML document into a [`Map`].
///
/// ```
/// let doc = archier::load("title: Middlemarch\n[authors]\n* George Eliot\n[]");
/// assert_eq!(doc["title"].as_str(), Some("Middlemarch"));
/// assert_eq!(doc["authors"].as_array().unwrap().len(), 1);
/// ```
pub fn load(text: &str) -> Map {
    load_ext(text, Options::empty())
}

/// Parses an ArchieML document into a [`Map`] with the given [`Options`].
///
/// ```
/// use archier::Options;
///
/// let doc = archier::load_ext("Title: First\ntitle: Second", Options::CASE_INSENSITIVE_KEYS);
/// assert_eq!(doc["title"].as_str(), Some("Second"));
/// ```
pub fn load_ext(text: &str, options: Options) -> Map {
    from_events_ext(Parser::new_ext(text, options), options)
}

/// Builds a document from a stream of events, as produced by a [`Parser`].
pub fn from_events<'a>(events: impl IntoIterator<Item = Event<'a>>) -> Map {
    from_events_ext(events, Options::empty())
}

/// Builds a document from a stream of events. With [`Options::CASE_INSENSITIVE_KEYS`],
/// every key is lowercased; the `type` of freeform array elements is kept as written.
/// Other options only affect parsing.
pub fn from_events_ext<'a>(events: impl IntoIterator<Item = Event<'a>>, options: Options) -> Map {
    let mut builder = Builder {
        root: Value::Object(Map::new()),
        scope: Vec::new(),
        stack: Vec::new(),
        lowercase_keys: options.contains(Options::CASE_INSENSITIVE_KEYS),
    };
    for event in events {
        builder.push(event);
    }
    match builder.root {
        Value::Object(map) => map,
        _ => unreachable!(),
    }
}

#[derive(Debug, Clone)]
enum Segment {
    Key(String),
    Index(usize),
}

type Path = Vec<Segment>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerKind {
    Object,
    Array,
    Freeform,
}

/// An open object block or array.
#[derive(Debug)]
struct Container {
    path: Path,
    kind: ContainerKind,
    /// The scope to return to when the container closes.
    outer_scope: Path,
}

/// Applies events to a document, following the reference implementation's model: keys
/// and nested blocks are placed relative to the current `scope` object, which opening an
/// object, starting an array element and adding a freeform element all change.
struct Builder {
    root: Value,
    scope: Path,
    stack: Vec<Container>,
    lowercase_keys: bool,
}

fn resolve<'v>(root: &'v mut Value, path: &[Segment]) -> Option<&'v mut Value> {
    path.iter()
        .try_fold(root, |value, segment| match (value, segment) {
            (Value::Object(map), Segment::Key(key)) => map.get_mut(key),
            (Value::Array(items), Segment::Index(i)) => items.get_mut(*i),
            _ => None,
        })
}

/// A freeform array element.
fn typed(ty: &str, value: Value) -> Value {
    Value::Object(Map::from_iter([
        ("type", Value::String(ty.to_owned())),
        ("value", value),
    ]))
}

impl Builder {
    fn key<'k>(&self, key: Key<'k>) -> Cow<'k, str> {
        if self.lowercase_keys {
            Cow::Owned(key.as_str().to_lowercase())
        } else {
            Cow::Borrowed(key.as_str())
        }
    }

    fn top_array(&mut self) -> Option<(&mut Vec<Value>, ContainerKind, &Path)> {
        let container = self.stack.last()?;
        match resolve(&mut self.root, &container.path)? {
            Value::Array(items) => Some((items, container.kind, &container.path)),
            _ => None,
        }
    }

    /// Appends to the innermost array, returning the new element's path.
    fn append(&mut self, value: Value) -> Option<Path> {
        let (items, _, path) = self.top_array()?;
        items.push(value);
        let mut path = path.clone();
        path.push(Segment::Index(items.len() - 1));
        Some(path)
    }

    fn scope_object(&mut self) -> Option<&mut Map> {
        match resolve(&mut self.root, &self.scope)? {
            Value::Object(map) => Some(map),
            _ => None,
        }
    }

    fn in_freeform(&self) -> bool {
        self.stack
            .last()
            .is_some_and(|container| container.kind == ContainerKind::Freeform)
    }

    fn push(&mut self, event: Event<'_>) {
        match event {
            Event::Start(Tag::Object { key, nested }) => self.open(
                key,
                nested,
                Value::Object(Map::new()),
                ContainerKind::Object,
            ),
            Event::Start(Tag::Array {
                key,
                nested,
                freeform,
            }) => {
                let kind = if freeform {
                    ContainerKind::Freeform
                } else {
                    ContainerKind::Array
                };
                self.open(key, nested, Value::Array(Vec::new()), kind)
            }
            Event::Start(Tag::Element) => {
                if let Some(path) = self.append(Value::Object(Map::new())) {
                    self.scope = path;
                }
            }
            // An element ends either when the next one starts or when its array closes.
            Event::End(TagEnd::Element) => {}
            Event::End(_) => {
                if let Some(container) = self.stack.pop() {
                    self.scope = container.outer_scope;
                }
            }
            Event::Pair(key, value) => {
                let value = Value::String(value.into_owned());
                if self.in_freeform() {
                    if let Some(path) = self.append(typed(key.as_str(), value)) {
                        self.scope = path;
                    }
                } else {
                    let key = self.key(key);
                    let Some(map) = self.scope_object() else {
                        return;
                    };
                    let (parent, last) = map.parent_of(&key);
                    parent.insert(last, value);
                }
            }
            Event::Item(value) => {
                self.append(Value::String(value.into_owned()));
            }
            Event::Text(value) => {
                if self.in_freeform() {
                    self.append(typed("text", Value::String(value.into_owned())));
                }
            }
        }
    }

    fn open(&mut self, key: Key<'_>, nested: bool, empty: Value, kind: ContainerKind) {
        if !nested {
            self.stack.clear();
            self.scope.clear();
        }
        let is_object = kind == ContainerKind::Object;
        let path = if nested && self.in_freeform() {
            // Blocks nested in freeform arrays become `{type, value}` elements.
            let Some(element) = self.append(typed(key.as_str(), empty)) else {
                return;
            };
            self.scope = element.clone();
            let mut path = element;
            path.push(Segment::Key("value".to_owned()));
            path
        } else {
            let key = self.key(key);
            let Some(map) = self.scope_object() else {
                return;
            };
            let (parent, last) = map.parent_of(&key);
            // Reopened top-level object blocks merge into the existing object. Nested
            // blocks and arrays always replace what was there.
            let reopen = !nested && is_object && matches!(parent.get(last), Some(Value::Object(_)));
            if !reopen {
                parent.insert(last, empty);
            }
            let mut path = self.scope.clone();
            path.extend(key.split('.').map(|s| Segment::Key(s.to_owned())));
            path
        };
        let outer_scope = self.scope.clone();
        if is_object {
            self.scope = path.clone();
        }
        self.stack.push(Container {
            path,
            kind,
            outer_scope,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(value: &str) -> Value {
        Value::String(value.to_owned())
    }

    #[test]
    fn dot_notation_overwrites_strings() {
        let doc = load("a: 1\na.b: 2\n{a.c}\nd: 3");
        assert_eq!(
            doc,
            Map::from_iter([(
                "a",
                Value::Object(Map::from_iter([
                    ("b", s("2")),
                    ("c", Value::Object(Map::from_iter([("d", s("3"))]))),
                ]))
            )])
        );
    }

    #[test]
    fn reopened_objects_merge_and_arrays_replace() {
        let doc = load("{o}\na: 1\n{}\n{o}\nb: 2\n[l]\n* x\n[]\n[l]\n* y\n");
        assert_eq!(
            doc["o"],
            Value::Object(Map::from_iter([("a", s("1")), ("b", s("2"))]))
        );
        assert_eq!(doc["l"], Value::Array(vec![s("y")]));
    }

    #[test]
    fn nested_objects_replace() {
        let doc = load("{o}\n{.p}\na: 1\n{}\n{.p}\nb: 2\n{}\n{}\nq.r: 3\n{.q}\n");
        assert_eq!(
            doc["o"],
            Value::Object(Map::from_iter([(
                "p",
                Value::Object(Map::from_iter([("b", s("2"))]))
            )]))
        );
        assert_eq!(doc["q"], Value::Object(Map::new()));
    }

    #[test]
    fn blocks_nested_in_string_arrays_attach_to_the_enclosing_scope() {
        let doc = load("{o}\n[.list]\n* a\n[.sub]\n* b\n[]\n* c\n");
        assert_eq!(
            doc["o"],
            Value::Object(Map::from_iter([
                ("list", Value::Array(vec![s("a"), s("c")])),
                ("sub", Value::Array(vec![s("b")])),
            ]))
        );
    }

    #[test]
    fn loose_syntax() {
        let doc = load("{o]\na..b: 1\n[}\n[l]\n*k: v\n");
        assert_eq!(
            doc["o"],
            Value::Object(Map::from_iter([(
                "a",
                Value::Object(Map::from_iter([(
                    "",
                    Value::Object(Map::from_iter([("b", s("1"))]))
                )]))
            )]))
        );
        assert_eq!(
            doc["l"],
            Value::Array(vec![Value::Object(Map::from_iter([("*k", s("v"))]))])
        );
    }

    #[test]
    fn case_insensitive_keys_are_lowercased() {
        let text =
            "Title: one\nTITLE: two\n{Meta.Info}\nA: 1\n{}\n{meta.info}\nb: 2\n[+Body]\nH1: x\n";
        let doc = load_ext(text, Options::CASE_INSENSITIVE_KEYS);
        assert_eq!(doc.keys().collect::<Vec<_>>(), ["title", "meta", "body"]);
        assert_eq!(doc["title"], s("two"));
        assert_eq!(
            doc["meta"],
            Value::Object(Map::from_iter([(
                "info",
                Value::Object(Map::from_iter([("a", s("1")), ("b", s("2"))]))
            )]))
        );
        // Freeform types are values, not keys, so they keep their case.
        assert_eq!(doc["body"], Value::Array(vec![typed("H1", s("x"))]));

        let doc = load(text);
        assert_eq!(
            doc.keys().collect::<Vec<_>>(),
            ["Title", "TITLE", "Meta", "meta", "Body"]
        );
    }

    #[test]
    fn keys_keep_definition_order() {
        let doc = load("b: 1\na: 2\nb: 3");
        assert_eq!(doc.keys().collect::<Vec<_>>(), ["b", "a"]);
        assert_eq!(doc["b"], s("3"));
    }
}
