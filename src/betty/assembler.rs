//! Betty's assembler: builds a document from betty-mode events.
//!
//! Betty builds its output with JavaScript objects and arrays, and its behavior depends
//! on that model: `{/name}` follows parent links, list items split when a key's existing
//! value is truthy, and lookups see inherited properties such as `toString`. This keeps
//! the same model in an arena, then converts the result into a [`Map`]. Where betty
//! would throw, the instruction is skipped instead.

use std::borrow::Cow;

use id_arena::{Arena, Id};

use crate::parser::{Event, Tag, TagEnd};
use crate::scanners::is_whitespace;
use crate::value::{Loader, Map, Value};

type NodeId<'a> = Id<Node<'a>>;

/// What an object or array can hold.
#[derive(Debug, Clone)]
enum Val<'a> {
    Str(Cow<'a, str>),
    Node(NodeId<'a>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArrayType {
    Simple,
    Standard,
    Freeform,
}

type Properties<'a> = Vec<(Cow<'a, str>, Val<'a>)>;

#[derive(Debug)]
enum Kind<'a> {
    Object(Properties<'a>),
    Array {
        ty: Option<ArrayType>,
        items: Vec<Val<'a>>,
        /// Non-index properties, which never appear in the output.
        props: Properties<'a>,
    },
}

/// A JavaScript object or array, with betty's `PARENT` and `NAME` symbols.
#[derive(Debug)]
struct Node<'a> {
    kind: Kind<'a>,
    parent: Option<NodeId<'a>>,
    name: Option<Cow<'a, str>>,
}

/// A JavaScript value, as far as betty's lookups can tell values apart.
#[derive(Debug, Clone, Copy)]
enum Js<'a> {
    Undefined,
    /// A string; only whether it is empty matters.
    Str {
        empty: bool,
        len: usize,
    },
    Node(NodeId<'a>),
    Number(usize),
    Function,
}

impl Js<'_> {
    fn truthy(self) -> bool {
        match self {
            Js::Undefined => false,
            Js::Str { empty, .. } => !empty,
            Js::Node(_) | Js::Function => true,
            Js::Number(n) => n != 0,
        }
    }
}

/// Betty would have thrown a `TypeError`.
#[derive(Debug)]
struct Thrown;

type Result<T> = std::result::Result<T, Thrown>;

/// Methods every object inherits. (`__proto__` is treated as an ordinary key.)
const OBJECT_METHODS: &[&str] = &[
    "constructor",
    "hasOwnProperty",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toLocaleString",
    "toString",
    "valueOf",
    "__defineGetter__",
    "__defineSetter__",
    "__lookupGetter__",
    "__lookupSetter__",
];

const ARRAY_METHODS: &[&str] = &[
    "at",
    "concat",
    "copyWithin",
    "entries",
    "every",
    "fill",
    "filter",
    "find",
    "findIndex",
    "findLast",
    "findLastIndex",
    "flat",
    "flatMap",
    "forEach",
    "includes",
    "indexOf",
    "join",
    "keys",
    "lastIndexOf",
    "map",
    "pop",
    "push",
    "reduce",
    "reduceRight",
    "reverse",
    "shift",
    "slice",
    "some",
    "sort",
    "splice",
    "toReversed",
    "toSorted",
    "toSpliced",
    "unshift",
    "values",
    "with",
];

const STRING_METHODS: &[&str] = &[
    "anchor",
    "at",
    "big",
    "blink",
    "bold",
    "charAt",
    "charCodeAt",
    "codePointAt",
    "concat",
    "endsWith",
    "fixed",
    "fontcolor",
    "fontsize",
    "includes",
    "indexOf",
    "isWellFormed",
    "italics",
    "lastIndexOf",
    "link",
    "localeCompare",
    "match",
    "matchAll",
    "normalize",
    "padEnd",
    "padStart",
    "repeat",
    "replace",
    "replaceAll",
    "search",
    "slice",
    "small",
    "split",
    "startsWith",
    "strike",
    "sub",
    "substr",
    "substring",
    "sup",
    "toLocaleLowerCase",
    "toLocaleUpperCase",
    "toLowerCase",
    "toUpperCase",
    "toWellFormed",
    "trim",
    "trimEnd",
    "trimLeft",
    "trimRight",
    "trimStart",
];

const NUMBER_METHODS: &[&str] = &["toExponential", "toFixed", "toPrecision"];

const FUNCTION_PROPERTIES: &[&str] = &["apply", "bind", "call", "length", "name"];

fn inherits(methods: &[&str], key: &str) -> bool {
    methods.contains(&key) || OBJECT_METHODS.contains(&key)
}

/// A canonical array index: `0`, or digits without a leading zero, below 2³² − 1.
fn array_index(key: &str) -> Option<usize> {
    let canonical =
        key == "0" || (!key.starts_with('0') && key.bytes().all(|b| b.is_ascii_digit()));
    let index: u32 = key.parse().ok().filter(|_| canonical)?;
    (index != u32::MAX).then_some(index as usize)
}

/// JavaScript's `ToNumber` for strings: decimal literals (with `Infinity`), `0x`, `0o`
/// and `0b` integers, or empty for zero. `None` is `NaN`.
fn to_number(text: &str) -> Option<f64> {
    let text = text.trim_matches(is_whitespace);
    if text.is_empty() {
        return Some(0.0);
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = text.strip_prefix(prefix) {
            return u128::from_str_radix(digits, radix).ok().map(|n| n as f64);
        }
    }
    let unsigned = text.strip_prefix(['+', '-']).unwrap_or(text);
    if unsigned == "Infinity" {
        return Some(if text.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    // Rust also accepts `inf` and `NaN`, which JavaScript doesn't.
    let decimal = unsigned
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'.' | b'e' | b'E' | b'+' | b'-'));
    decimal.then(|| text.parse().ok()).flatten()
}

fn trim(text: Cow<'_, str>) -> Cow<'_, str> {
    match text {
        Cow::Borrowed(text) => Cow::Borrowed(text.trim_matches(is_whitespace)),
        Cow::Owned(text) => Cow::Owned(text.trim_matches(is_whitespace).to_owned()),
    }
}

fn find<'p, 'a>(props: &'p Properties<'a>, key: &str) -> Option<&'p Val<'a>> {
    props.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

fn set<'a>(props: &mut Properties<'a>, key: Cow<'a, str>, value: Val<'a>) {
    match props.iter_mut().find(|(k, _)| *k == key) {
        Some((_, existing)) => *existing = value,
        None => props.push((key, value)),
    }
}

struct Assembler<'a, 'l> {
    nodes: Arena<Node<'a>>,
    root: NodeId<'a>,
    /// The context stack; `None` is JavaScript's `undefined`, which betty can push.
    stack: Vec<Option<NodeId<'a>>>,
    loader: &'l Loader<'l>,
}

/// Builds a document from betty-mode events, as betty's assembler would.
pub(crate) fn assemble<'a>(
    events: impl IntoIterator<Item = Event<'a>>,
    loader: &Loader<'_>,
) -> Map<'a> {
    let mut nodes = Arena::new();
    let root = nodes.alloc(Node {
        kind: Kind::Object(Vec::new()),
        parent: None,
        name: None,
    });
    let mut assembler = Assembler {
        nodes,
        root,
        stack: vec![Some(root)],
        loader,
    };
    for event in events {
        // An instruction betty would have thrown on has no effect.
        let _ = assembler.instruction(event);
    }
    match assembler.to_value(Val::Node(root), &mut Vec::new()) {
        Some(Value::Object(map)) => map,
        _ => Map::new(),
    }
}

// Keys are passed as `&Cow` so `normalize` can keep segments borrowed from the source.
#[expect(
    clippy::ptr_arg,
    reason = "the Cow decides whether key segments borrow"
)]
impl<'a> Assembler<'a, '_> {
    fn instruction(&mut self, event: Event<'a>) -> Result<()> {
        match event {
            Event::Pair(key, value) => self.value(key.into_cow(), value),
            Event::Item(text) => {
                self.simple(text);
                Ok(())
            }
            Event::Text(line) => self.buffer(line),
            Event::Start(Tag::Object { key, .. }) => self.object(key.into_cow()),
            Event::Start(Tag::Array { key, freeform, .. }) => self.array(key.into_cow(), freeform),
            Event::Start(Tag::Element) | Event::End(TagEnd::Element) => Ok(()),
            Event::End(TagEnd::Object) => {
                self.close_object(None);
                Ok(())
            }
            Event::End(TagEnd::Array) => {
                self.close_array(None);
                Ok(())
            }
            Event::EndNamed(TagEnd::Array, name) => {
                self.close_array(Some(name.as_str()));
                Ok(())
            }
            Event::EndNamed(_, name) => {
                self.close_object(Some(name.as_str()));
                Ok(())
            }
        }
    }

    fn alloc(
        &mut self,
        kind: Kind<'a>,
        parent: Option<NodeId<'a>>,
        name: Option<Cow<'a, str>>,
    ) -> NodeId<'a> {
        self.nodes.alloc(Node { kind, parent, name })
    }

    fn object_node(&mut self, props: Properties<'a>, parent: Option<NodeId<'a>>) -> NodeId<'a> {
        self.alloc(Kind::Object(props), parent, None)
    }

    /// A freeform array element, `{ type, value }`.
    fn typed(&mut self, ty: Cow<'a, str>, value: Val<'a>, parent: Option<NodeId<'a>>) -> Val<'a> {
        let props = vec![
            (Cow::Borrowed("type"), Val::Str(ty)),
            (Cow::Borrowed("value"), value),
        ];
        Val::Node(self.object_node(props, parent))
    }

    fn top(&self) -> Option<NodeId<'a>> {
        *self.stack.last().expect("the stack is never empty")
    }

    fn array_type(&self, id: NodeId<'a>) -> Option<Option<ArrayType>> {
        match &self.nodes[id].kind {
            Kind::Array { ty, .. } => Some(*ty),
            Kind::Object(_) => None,
        }
    }

    fn set_array_type(&mut self, id: NodeId<'a>, new: ArrayType) {
        if let Kind::Array { ty, .. } = &mut self.nodes[id].kind {
            *ty = Some(new);
        }
    }

    fn push_item(&mut self, id: NodeId<'a>, value: Val<'a>) {
        if let Kind::Array { items, .. } = &mut self.nodes[id].kind {
            items.push(value);
        }
    }

    fn js(&self, value: &Val<'a>) -> Js<'a> {
        match value {
            Val::Str(s) => Js::Str {
                empty: s.is_empty(),
                len: s.encode_utf16().count(),
            },
            Val::Node(id) => Js::Node(*id),
        }
    }

    /// JavaScript's `key in value`.
    fn has(&self, value: Js<'a>, key: &str) -> Result<bool> {
        match value {
            Js::Node(id) => Ok(match &self.nodes[id].kind {
                Kind::Object(props) => find(props, key).is_some() || OBJECT_METHODS.contains(&key),
                Kind::Array { items, props, .. } => {
                    array_index(key).is_some_and(|i| i < items.len())
                        || key == "length"
                        || find(props, key).is_some()
                        || inherits(ARRAY_METHODS, key)
                }
            }),
            Js::Function => Ok(inherits(FUNCTION_PROPERTIES, key)),
            // `in` throws on primitives.
            Js::Undefined | Js::Str { .. } | Js::Number(_) => Err(Thrown),
        }
    }

    /// JavaScript's `value[key]`.
    fn get(&self, value: Js<'a>, key: &str) -> Result<Js<'a>> {
        let method = |methods: &[&str]| {
            if inherits(methods, key) {
                Js::Function
            } else {
                Js::Undefined
            }
        };
        Ok(match value {
            Js::Undefined => return Err(Thrown),
            Js::Node(id) => match &self.nodes[id].kind {
                Kind::Object(props) => find(props, key).map_or_else(|| method(&[]), |v| self.js(v)),
                Kind::Array { items, props, .. } => {
                    if let Some(item) = array_index(key).and_then(|i| items.get(i)) {
                        self.js(item)
                    } else if key == "length" {
                        Js::Number(items.len())
                    } else {
                        find(props, key).map_or_else(|| method(ARRAY_METHODS), |v| self.js(v))
                    }
                }
            },
            Js::Str { len, .. } => match array_index(key) {
                Some(i) if i < len => Js::Str {
                    empty: false,
                    len: 1,
                },
                _ if key == "length" => Js::Number(len),
                _ => method(STRING_METHODS),
            },
            Js::Number(_) => method(NUMBER_METHODS),
            Js::Function => method(FUNCTION_PROPERTIES),
        })
    }

    /// Betty's `normalizeKeypath`: the key's non-empty segments, through `onFieldName`.
    fn normalize(&self, key: &Cow<'a, str>) -> Vec<Cow<'a, str>> {
        let segments: Vec<Cow<'a, str>> = match key {
            Cow::Borrowed(key) => key.split('.').map(Cow::Borrowed).collect(),
            Cow::Owned(key) => key.split('.').map(|s| Cow::Owned(s.to_owned())).collect(),
        };
        segments
            .into_iter()
            .filter(|s| !s.is_empty())
            .map(|s| self.loader.field_name(s))
            .collect()
    }

    /// Betty's `getPath`.
    fn get_path(&self, object: Js<'a>, key: &Cow<'a, str>) -> Result<Js<'a>> {
        let mut path = self.normalize(key);
        let terminal = path.pop();
        let mut branch = object;
        for segment in &path {
            if !self.has(branch, segment)? {
                return Ok(Js::Undefined);
            }
            branch = self.get(branch, segment)?;
        }
        if !branch.truthy() {
            return Ok(branch);
        }
        self.get(branch, terminal.as_deref().unwrap_or("undefined"))
    }

    /// JavaScript's `node[key] = value`.
    ///
    /// Returns whether `value` was stored: assigning an array's `length` converts the
    /// value to a number and resizes the array instead.
    fn assign(&mut self, id: NodeId<'a>, key: Cow<'a, str>, value: Val<'a>) -> Result<bool> {
        let length = match (&self.nodes[id].kind, &*key) {
            (Kind::Array { .. }, "length") => Some(self.to_length(&value)?),
            _ => None,
        };
        match (&mut self.nodes[id].kind, length) {
            (Kind::Object(props), _) => set(props, key, value),
            (Kind::Array { items, .. }, Some(length)) => {
                // Growing an array would add holes, which documents can't hold.
                if length > items.len() {
                    return Err(Thrown);
                }
                items.truncate(length);
                return Ok(false);
            }
            (Kind::Array { items, props, .. }, None) => match array_index(&key) {
                Some(i) if i < items.len() => items[i] = value,
                Some(i) if i == items.len() => items.push(value),
                Some(_) => return Err(Thrown),
                None => set(props, key, value),
            },
        }
        Ok(true)
    }

    /// The array length JavaScript makes of `value`, which must be a whole number.
    fn to_length(&self, value: &Val<'a>) -> Result<usize> {
        let number = to_number(&self.to_js_string(value)).ok_or(Thrown)?;
        let valid = number.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(&number);
        valid.then_some(number as usize).ok_or(Thrown)
    }

    /// JavaScript's `String(value)`.
    fn to_js_string(&self, value: &Val<'a>) -> String {
        match value {
            Val::Str(s) => s.to_string(),
            Val::Node(id) => match &self.nodes[*id].kind {
                // Arrays convert by joining their elements.
                Kind::Array { items, .. } => items
                    .iter()
                    .map(|v| self.to_js_string(v))
                    .collect::<Vec<_>>()
                    .join(","),
                Kind::Object(_) => "[object Object]".to_owned(),
            },
        }
    }

    /// Betty's `setPath`, which creates (or replaces non-objects with) intermediate
    /// objects along the way.
    fn set_path(&mut self, target: NodeId<'a>, key: &Cow<'a, str>, value: Val<'a>) -> Result<()> {
        let mut path = self.normalize(key);
        let terminal = path.pop().unwrap_or(Cow::Borrowed("undefined"));
        let mut branch = target;
        for segment in path {
            if segment.is_empty() {
                continue;
            }
            let here = Js::Node(branch);
            let existing = if self.has(here, &segment)? {
                self.get(here, &segment)?
            } else {
                Js::Undefined
            };
            branch = match existing {
                Js::Node(id) => id,
                _ => {
                    let id = self.alloc(
                        Kind::Object(Vec::new()),
                        Some(branch),
                        Some(segment.clone()),
                    );
                    // An object can't become an array's length.
                    if !self.assign(branch, segment, Val::Node(id))? {
                        return Err(Thrown);
                    }
                    id
                }
            };
        }
        let value = match value {
            Val::Str(s) => Val::Str(self.loader.value(&terminal, trim(s))),
            node => node,
        };
        let stored = self.assign(branch, terminal, value.clone())?;
        if let (true, Val::Node(id)) = (stored, value) {
            // `PARENT` can't be redefined: betty throws after the assignment.
            match self.nodes[id].parent {
                None => self.nodes[id].parent = Some(branch),
                Some(parent) if parent == branch => {}
                Some(_) => return Err(Thrown),
            }
        }
        Ok(())
    }

    fn append(
        &mut self,
        target: Option<NodeId<'a>>,
        key: &Cow<'a, str>,
        value: Val<'a>,
    ) -> Result<()> {
        let target = target.ok_or(Thrown)?;
        if self.array_type(target).is_some() {
            self.add_to_array(target, key, value)
        } else {
            self.set_path(target, key, value)
        }
    }

    fn add_to_array(
        &mut self,
        array: NodeId<'a>,
        key: &Cow<'a, str>,
        value: Val<'a>,
    ) -> Result<()> {
        let value = match value {
            Val::Str(s) => Val::Str(trim(s)),
            node => node,
        };
        match self.array_type(array).flatten() {
            Some(ArrayType::Freeform) => {
                let ty = match key {
                    Cow::Borrowed(key) => Cow::Borrowed(key.strip_prefix('.').unwrap_or(key)),
                    Cow::Owned(key) => Cow::Owned(key.strip_prefix('.').unwrap_or(key).to_owned()),
                };
                let element = self.typed(ty, value, Some(array));
                self.push_item(array, element);
            }
            Some(ArrayType::Simple) => {}
            _ => {
                self.set_array_type(array, ArrayType::Standard);
                let last = match &self.nodes[array].kind {
                    Kind::Array { items, .. } => items.last().cloned(),
                    Kind::Object(_) => None,
                };
                // A new item starts when the last one already has a truthy value there.
                let element = match last {
                    Some(last) if !self.get_path(self.js(&last), key)?.truthy() => last,
                    _ => {
                        let element = Val::Node(self.object_node(Vec::new(), Some(array)));
                        self.push_item(array, element.clone());
                        element
                    }
                };
                match element {
                    Val::Node(id) => self.set_path(id, key, value)?,
                    // Assigning to a string throws in strict mode.
                    Val::Str(_) => return Err(Thrown),
                }
            }
        }
        Ok(())
    }

    fn value(&mut self, key: Cow<'a, str>, value: Cow<'a, str>) -> Result<()> {
        let target = self.top();
        match target.and_then(|id| self.array_type(id).map(|ty| (id, ty))) {
            Some((_, Some(ArrayType::Simple))) => Ok(()),
            Some((id, Some(ArrayType::Freeform))) => {
                let element = self.typed(key, Val::Str(value), None);
                self.push_item(id, element);
                Ok(())
            }
            _ => self.append(target, &key, Val::Str(value)),
        }
    }

    fn simple(&mut self, text: Cow<'a, str>) {
        let Some(id) = self.top() else {
            return;
        };
        match self.array_type(id) {
            Some(None | Some(ArrayType::Simple)) => {
                self.set_array_type(id, ArrayType::Simple);
                let item = match text {
                    Cow::Borrowed(t) => Cow::Borrowed(t.trim_start_matches(is_whitespace)),
                    Cow::Owned(t) => Cow::Owned(t.trim_start_matches(is_whitespace).to_owned()),
                };
                self.push_item(id, Val::Str(item));
            }
            Some(Some(ArrayType::Freeform)) => {
                let element = self.typed(
                    Cow::Borrowed("text"),
                    Val::Str(Cow::Owned(format!("*{text}"))),
                    Some(id),
                );
                self.push_item(id, element);
            }
            _ => {}
        }
    }

    fn buffer(&mut self, line: Cow<'a, str>) -> Result<()> {
        let id = self.top().ok_or(Thrown)?;
        if self.array_type(id) == Some(Some(ArrayType::Freeform)) {
            let element = self.typed(Cow::Borrowed("text"), Val::Str(line), None);
            self.push_item(id, element);
        }
        Ok(())
    }

    /// Betty's `getTarget`: dotted keys are relative to the current context, and other
    /// keys reset to the root.
    fn target(&mut self, key: &str) -> Option<NodeId<'a>> {
        if key.starts_with('.') {
            return self.top();
        }
        self.stack = vec![Some(self.root)];
        Some(self.root)
    }

    fn object(&mut self, key: Cow<'a, str>) -> Result<()> {
        let target = self.target(&key);
        let existing = self.get_path(target.map_or(Js::Undefined, Js::Node), &key)?;
        let node = match existing {
            Js::Node(id) => id,
            _ => {
                let name = self.normalize(&key).pop();
                self.alloc(Kind::Object(Vec::new()), None, name)
            }
        };
        self.append(target, &key, Val::Node(node))?;
        self.stack.push(Some(node));
        Ok(())
    }

    fn array(&mut self, key: Cow<'a, str>, freeform: bool) -> Result<()> {
        let ty = freeform.then_some(ArrayType::Freeform);
        let name = self.normalize(&key).pop();
        let node = self.alloc(
            Kind::Array {
                ty,
                items: Vec::new(),
                props: Vec::new(),
            },
            None,
            name,
        );
        let target = self.target(&key);
        self.append(target, &key, Val::Node(node))?;
        self.stack.push(Some(node));
        Ok(())
    }

    fn pop_context(&mut self) -> NodeId<'a> {
        let popped = self.stack.pop();
        if self.stack.is_empty() {
            self.stack.push(Some(self.root));
        }
        popped.flatten().unwrap_or(self.root)
    }

    /// `{/name}` and `[/name]`: pop the context, follow parents up to the block called
    /// `name` (or the root), and continue in its parent.
    fn close_named(&mut self, name: &str) {
        let mut current = Some(self.pop_context());
        while let Some(id) = current {
            if id == self.root || self.nodes[id].name.as_deref() == Some(name) {
                break;
            }
            current = self.nodes[id].parent;
        }
        // Betty throws on reaching a node without a parent; continue nowhere instead.
        self.stack
            .push(current.and_then(|id| self.nodes[id].parent));
    }

    fn close_object(&mut self, name: Option<&str>) {
        match name {
            Some(name) => self.close_named(name),
            None => {
                self.pop_context();
            }
        }
    }

    fn close_array(&mut self, name: Option<&str>) {
        if let Some(name) = name {
            return self.close_named(name);
        }
        let is_array = |this: &Self| this.top().is_some_and(|id| this.array_type(id).is_some());
        while !is_array(self) && self.top() != Some(self.root) {
            self.pop_context();
        }
        if is_array(self) {
            self.pop_context();
        }
    }

    /// The output, as `structuredClone` and `JSON.stringify` would see it. A node that
    /// contains itself can't be serialized, so it is left out.
    fn to_value(&self, value: Val<'a>, ancestors: &mut Vec<NodeId<'a>>) -> Option<Value<'a>> {
        let id = match value {
            Val::Str(s) => return Some(Value::String(s)),
            Val::Node(id) => id,
        };
        if ancestors.contains(&id) {
            return None;
        }
        ancestors.push(id);
        let value = match &self.nodes[id].kind {
            Kind::Object(props) => Value::Object(
                props
                    .iter()
                    .filter_map(|(k, v)| Some((k.clone(), self.to_value(v.clone(), ancestors)?)))
                    .collect(),
            ),
            Kind::Array { items, .. } => Value::Array(
                items
                    .iter()
                    .filter_map(|v| self.to_value(v.clone(), ancestors))
                    .collect(),
            ),
        };
        ancestors.pop();
        Some(value)
    }
}
