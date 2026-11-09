//! An in-memory document built from parser events.
//!
//! This is the ArchieML equivalent of pulldown-cmark's HTML renderer: a consumer of the
//! [`Event`] stream that applies ArchieML's semantics (dot-notation, overwriting
//! duplicate keys, merging reopened object blocks) to produce a tree of [`Value`]s.
//!
//! The tree borrows from the source text: keys and strings are only allocated when they
//! differ from the source (multi-line values with escapes, for example). Use
//! [`Map::into_owned`] for a document that outlives the source.

use std::borrow::Cow;

use crate::options::Options;
use crate::parser::{Event, Key, Parser, Tag, TagEnd};

/// An ArchieML value, borrowing from the source text where possible. All scalar values
/// are strings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value<'a> {
    String(Cow<'a, str>),
    Array(Vec<Value<'a>>),
    Object(Map<'a>),
}

impl<'a> Value<'a> {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value<'a>]> {
        match self {
            Value::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&Map<'a>> {
        match self {
            Value::Object(map) => Some(map),
            _ => None,
        }
    }

    /// Copies everything still borrowed from the source.
    pub fn into_owned(self) -> Value<'static> {
        match self {
            Value::String(s) => Value::String(Cow::Owned(s.into_owned())),
            Value::Array(items) => Value::Array(items.into_iter().map(Value::into_owned).collect()),
            Value::Object(map) => Value::Object(map.into_owned()),
        }
    }
}

/// An object whose keys keep the order in which they were first defined. Keys borrow
/// from the source text where possible.
///
/// Equality ignores key order.
#[derive(Debug, Clone, Default, Eq)]
pub struct Map<'a> {
    entries: Vec<(Cow<'a, str>, Value<'a>)>,
}

impl<'a> Map<'a> {
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

    pub fn get(&self, key: &str) -> Option<&Value<'a>> {
        self.position(key).map(|i| &self.entries[i].1)
    }

    pub fn get_mut(&mut self, key: &str) -> Option<&mut Value<'a>> {
        self.position(key).map(|i| &mut self.entries[i].1)
    }

    /// Sets `key` to `value`, returning the previous value. A replaced key keeps its
    /// original position.
    pub fn insert(&mut self, key: impl Into<Cow<'a, str>>, value: Value<'a>) -> Option<Value<'a>> {
        let key = key.into();
        match self.position(&key) {
            Some(i) => Some(std::mem::replace(&mut self.entries[i].1, value)),
            None => {
                self.entries.push((key, value));
                None
            }
        }
    }

    pub fn remove(&mut self, key: &str) -> Option<Value<'a>> {
        self.position(key).map(|i| self.entries.remove(i).1)
    }

    pub fn iter(&self) -> impl ExactSizeIterator<Item = (&str, &Value<'a>)> {
        self.entries.iter().map(|(k, v)| (k.as_ref(), v))
    }

    pub fn keys(&self) -> impl ExactSizeIterator<Item = &str> {
        self.entries.iter().map(|(k, _)| k.as_ref())
    }

    /// Copies everything still borrowed from the source.
    pub fn into_owned(self) -> Map<'static> {
        Map {
            entries: self
                .entries
                .into_iter()
                .map(|(k, v)| (Cow::Owned(k.into_owned()), v.into_owned()))
                .collect(),
        }
    }

    /// The object at `key`, replacing whatever non-object value is there.
    fn object_at(&mut self, key: Cow<'a, str>) -> &mut Map<'a> {
        let i = match self.position(&key) {
            Some(i) => {
                if !matches!(self.entries[i].1, Value::Object(_)) {
                    self.entries[i].1 = Value::Object(Map::new());
                }
                i
            }
            None => {
                self.entries.push((key, Value::Object(Map::new())));
                self.entries.len() - 1
            }
        };
        match &mut self.entries[i].1 {
            Value::Object(map) => map,
            _ => unreachable!(),
        }
    }

    /// The object holding the last of a dotted key's `segments`, creating (or
    /// overwriting) intermediate objects as needed. Returns it along with that segment.
    fn parent_of(
        &mut self,
        mut segments: impl Iterator<Item = Cow<'a, str>>,
    ) -> (&mut Map<'a>, Cow<'a, str>) {
        let mut last = segments.next().unwrap_or_default();
        let mut map = self;
        for segment in segments {
            map = map.object_at(last);
            last = segment;
        }
        (map, last)
    }
}

/// The dot-separated segments of a key, borrowing from the source if the key does.
fn segments<'k, 'a>(key: &'k Cow<'a, str>) -> impl Iterator<Item = Cow<'a, str>> + 'k {
    let (borrowed, owned) = match key {
        Cow::Borrowed(key) => (Some(key.split('.').map(Cow::Borrowed)), None),
        Cow::Owned(key) => (None, Some(key.split('.').map(|s| Cow::Owned(s.to_owned())))),
    };
    borrowed
        .into_iter()
        .flatten()
        .chain(owned.into_iter().flatten())
}

impl PartialEq for Map<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().all(|(k, v)| other.get(k) == Some(v))
    }
}

impl<'m, 'a> IntoIterator for &'m Map<'a> {
    type Item = (&'m str, &'m Value<'a>);
    type IntoIter = Box<dyn Iterator<Item = (&'m str, &'m Value<'a>)> + 'm>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl<'a, K: Into<Cow<'a, str>>> FromIterator<(K, Value<'a>)> for Map<'a> {
    fn from_iter<I: IntoIterator<Item = (K, Value<'a>)>>(iter: I) -> Self {
        let mut map = Map::new();
        for (key, value) in iter {
            map.insert(key, value);
        }
        map
    }
}

impl<'a> std::ops::Index<&str> for Map<'a> {
    type Output = Value<'a>;

    fn index(&self, key: &str) -> &Value<'a> {
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
pub fn load(text: &str) -> Map<'_> {
    load_ext(text, Options::empty())
}

/// Parses an ArchieML document into a [`Map`] with the given [`Options`].
///
/// ```
/// # #[cfg(feature = "case-insensitive")] {
/// use archier::Options;
///
/// let doc = archier::load_ext("Title: First\ntitle: Second", Options::CASE_INSENSITIVE_KEYS);
/// assert_eq!(doc["title"].as_str(), Some("Second"));
/// # }
/// ```
pub fn load_ext(text: &str, options: Options) -> Map<'_> {
    from_events_ext(Parser::new_ext(text, options), options)
}

/// Builds a document from a stream of events, as produced by a [`Parser`].
pub fn from_events<'a>(events: impl IntoIterator<Item = Event<'a>>) -> Map<'a> {
    from_events_ext(events, Options::empty())
}

/// Builds a document from a stream of events. With [`Options::CASE_INSENSITIVE_KEYS`],
/// every key is lowercased; the `type` of freeform array elements is kept as written.
/// Other options only affect parsing.
pub fn from_events_ext<'a>(
    events: impl IntoIterator<Item = Event<'a>>,
    options: Options,
) -> Map<'a> {
    let mut builder = Builder {
        root: Value::Object(Map::new()),
        scope: Some(Vec::new()),
        stack: Vec::new(),
        options,
    };
    for event in events {
        builder.push(event);
    }
    match builder.root {
        Value::Object(map) => map,
        _ => unreachable!(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment<'a> {
    Key(Cow<'a, str>),
    Index(usize),
}

type Path<'a> = Vec<Segment<'a>>;

/// Where an open container or scope lives in the document, or `None` once the value
/// there has been replaced. archieml-js holds references to objects, so it goes on
/// writing to a replaced (and no longer reachable) container; writes to a detached
/// location are dropped to match.
type Location<'a> = Option<Path<'a>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContainerKind {
    Object,
    Array,
    Freeform,
}

/// An open object block or array.
#[derive(Debug)]
struct Container<'a> {
    path: Location<'a>,
    kind: ContainerKind,
    /// The scope to return to when the container closes.
    outer_scope: Location<'a>,
}

/// Applies events to a document, following the reference implementation's model: keys
/// and nested blocks are placed relative to the current `scope` object, which opening an
/// object, starting an array element and adding a freeform element all change.
struct Builder<'a> {
    root: Value<'a>,
    scope: Location<'a>,
    stack: Vec<Container<'a>>,
    options: Options,
}

fn get<'v, 'a>(root: &'v Value<'a>, path: &[Segment<'a>]) -> Option<&'v Value<'a>> {
    path.iter()
        .try_fold(root, |value, segment| match (value, segment) {
            (Value::Object(map), Segment::Key(key)) => map.get(key),
            (Value::Array(items), Segment::Index(i)) => items.get(*i),
            _ => None,
        })
}

fn resolve<'v, 'a>(root: &'v mut Value<'a>, path: &[Segment<'a>]) -> Option<&'v mut Value<'a>> {
    path.iter()
        .try_fold(root, |value, segment| match (value, segment) {
            (Value::Object(map), Segment::Key(key)) => map.get_mut(key),
            (Value::Array(items), Segment::Index(i)) => items.get_mut(*i),
            _ => None,
        })
}

/// A freeform array element.
fn typed<'a>(ty: &'a str, value: Value<'a>) -> Value<'a> {
    Value::Object(Map::from_iter([
        ("type", Value::String(Cow::Borrowed(ty))),
        ("value", value),
    ]))
}

impl<'a> Builder<'a> {
    fn key(&self, key: Key<'a>) -> Vec<Cow<'a, str>> {
        segments(&self.options.document_key(key.as_str())).collect()
    }

    fn top_array(&mut self) -> Option<(&mut Vec<Value<'a>>, &Path<'a>)> {
        let path = self.stack.last()?.path.as_ref()?;
        match resolve(&mut self.root, path)? {
            Value::Array(items) => Some((items, path)),
            _ => None,
        }
    }

    /// Appends to the innermost array, returning the new element's location.
    fn append(&mut self, value: Value<'a>) -> Location<'a> {
        let (items, path) = self.top_array()?;
        items.push(value);
        let mut path = path.clone();
        path.push(Segment::Index(items.len() - 1));
        Some(path)
    }

    fn scope_object(&mut self) -> Option<&mut Map<'a>> {
        match resolve(&mut self.root, self.scope.as_ref()?)? {
            Value::Object(map) => Some(map),
            _ => None,
        }
    }

    fn in_freeform(&self) -> bool {
        self.stack
            .last()
            .is_some_and(|container| container.kind == ContainerKind::Freeform)
    }

    /// Sets the dotted key `segments`, relative to the scope, to `value`, returning its
    /// location. With `merge`, an object already there is kept instead.
    fn set(&mut self, segments: Vec<Cow<'a, str>>, value: Value<'a>, merge: bool) -> Location<'a> {
        let mut path = self.scope.clone()?;
        // Detach anything open inside the value this replaces: an existing value at the
        // key, or a non-object in the way of the key's intermediate objects.
        let start = path.len();
        for (i, segment) in segments.iter().enumerate() {
            path.push(Segment::Key(segment.clone()));
            let kept = i + 1 < segments.len() || merge;
            match get(&self.root, &path) {
                Some(Value::Object(_)) if kept => continue,
                Some(_) => self.detach(&path),
                None => {}
            }
            break;
        }
        path.truncate(start);

        let (parent, last) = self.scope_object()?.parent_of(segments.iter().cloned());
        if !(merge && matches!(parent.get(&last), Some(Value::Object(_)))) {
            parent.insert(last, value);
        }
        path.extend(segments.into_iter().map(Segment::Key));
        Some(path)
    }

    /// Marks every location at or below `replaced` as detached.
    fn detach(&mut self, replaced: &[Segment<'a>]) {
        let check = |location: &mut Location<'a>| {
            if location
                .as_ref()
                .is_some_and(|path| path.starts_with(replaced))
            {
                *location = None;
            }
        };
        check(&mut self.scope);
        for container in &mut self.stack {
            check(&mut container.path);
            check(&mut container.outer_scope);
        }
    }

    fn push(&mut self, event: Event<'a>) {
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
                self.scope = self.append(Value::Object(Map::new()));
            }
            // An element ends either when the next one starts or when its array closes.
            Event::End(TagEnd::Element) => {}
            Event::End(_) => {
                if let Some(container) = self.stack.pop() {
                    self.scope = container.outer_scope;
                }
            }
            Event::Pair(key, value) => {
                let value = Value::String(value);
                if self.in_freeform() {
                    self.scope = self.append(typed(key.as_str(), value));
                } else {
                    let segments = self.key(key);
                    self.set(segments, value, false);
                }
            }
            Event::Item(value) => {
                self.append(Value::String(value));
            }
            Event::Text(value) => {
                if self.in_freeform() {
                    self.append(typed("text", Value::String(value)));
                }
            }
        }
    }

    fn open(&mut self, key: Key<'a>, nested: bool, empty: Value<'a>, kind: ContainerKind) {
        if !nested {
            self.stack.clear();
            self.scope = Some(Vec::new());
        }
        let is_object = kind == ContainerKind::Object;
        let path = if nested && self.in_freeform() {
            // Blocks nested in freeform arrays become `{type, value}` elements.
            let element = self.append(typed(key.as_str(), empty));
            self.scope = element.clone();
            element.map(|mut path| {
                path.push(Segment::Key(Cow::Borrowed("value")));
                path
            })
        } else {
            // Reopened top-level object blocks merge into the existing object. Nested
            // blocks and arrays always replace what was there.
            let segments = self.key(key);
            self.set(segments, empty, !nested && is_object)
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

    fn s(value: &'static str) -> Value<'static> {
        Value::String(Cow::Borrowed(value))
    }

    fn is_borrowed(value: &Value<'_>) -> bool {
        match value {
            Value::String(s) => matches!(s, Cow::Borrowed(_)),
            Value::Array(items) => items.iter().all(is_borrowed),
            Value::Object(map) => map
                .entries
                .iter()
                .all(|(k, v)| matches!(k, Cow::Borrowed(_)) && is_borrowed(v)),
        }
    }

    #[test]
    fn documents_borrow_from_the_source() {
        let text = "title: Hello\n{meta.info}\nby: Me\n{}\n[list]\n* one\n* two,\nthree\n:end\n[]\n[+body]\nh1: Head\nSome text\n";
        let doc = load(text);
        assert!(is_borrowed(&Value::Object(doc.clone())));
        assert_eq!(doc["list"], Value::Array(vec![s("one"), s("two,\nthree")]));

        // Escapes have to be removed, so that value is copied; the rest still borrows.
        let doc = load("a: x\n\\:end\n:end\nb: y");
        assert!(matches!(&doc["a"], Value::String(Cow::Owned(v)) if v == "x\n:end"));
        assert!(matches!(&doc["b"], Value::String(Cow::Borrowed(_))));

        let owned: Map<'static> = load(&String::from("k: v")).into_owned();
        assert_eq!(owned["k"], s("v"));
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
    fn writes_to_replaced_containers_are_dropped() {
        // `[.list]` replaces the array that is still open, as `{.list}` would; archieml-js
        // keeps appending to the replaced array, so `* z` never reaches the document.
        let doc = load("[list]\n* x\n[.list]\n* y\n[]\n* z\n");
        assert_eq!(doc["list"], Value::Array(vec![s("y")]));
        let doc = load("[list]\n* x\n{.list}\nk: v\n{}\n* z\n[]\nafter: w\n");
        assert_eq!(doc["list"], Value::Object(Map::from_iter([("k", s("v"))])));
        assert_eq!(doc["after"], s("w"));
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
    #[cfg(feature = "case-insensitive")]
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
