# archier

A pull parser for [ArchieML](https://archieml.org), modeled on
[pulldown-cmark](https://docs.rs/pulldown-cmark).

[`Parser`] is an iterator over [`Event`]s describing the structure of a document:
balanced [`Event::Start`]/[`Event::End`] pairs for object blocks, arrays and array
elements, with values in between. Values borrow from the source whenever possible.

```rust
use archier::{Event, Parser, Tag};

let text = "
headline: Hello
[quotes]
* One
* Two,
on two lines
:end
[]
";
for event in Parser::new(text) {
    match event {
        Event::Pair(key, value) => println!("{key} = {value:?}"),
        Event::Start(Tag::Array { key, .. }) => println!("array {key}"),
        Event::Item(value) => println!("  - {value:?}"),
        _ => {}
    }
}
```

To get the parsed document as a tree, use [`load`], which builds a [`Map`] from the
events. The tree borrows from the source text too; [`Map::into_owned`] detaches it.

```rust
let doc = archier::load("{person}\nname: Archie\n{}\n");
let person = doc["person"].as_object().unwrap();
assert_eq!(person["name"].as_str(), Some("Archie"));
```

## Compatibility

The parser targets the [ArchieML 1.0 candidate
recommendation](https://archieml.org/spec/1.0/CR-20151015.html) and passes the
official conformance suite, which also covers behavior adopted after that draft:
Unicode keys, nested object blocks (`{.key}`) and blocks nested in freeform arrays.

Where neither the spec nor the suite defines the outcome, the parser mirrors the
reference implementation, [archieml-js](https://github.com/newsdev/archieml-js), with
one exception: plain text inside a `{+key}` block, which makes archieml-js throw, is
ignored. archieml-js behavior that contradicts the spec is not reproduced.

## Options

[`Parser::new_ext`] and [`load_ext`] take [`Options`] for the spec's optional and
deprecated features. Each is off unless requested, and also needs its Cargo feature:

| Option | Spec | Cargo feature | Feature on by default |
|---|---|---|---|
| `Options::CASE_INSENSITIVE_KEYS` | §1.3, optional | `case-insensitive` | yes |
| `Options::ENABLE_INLINE_COMMENTS` | §7, deprecated | `inline-comments` | no |
| `Options::BETTY` | [betty dialect](#betty) | `betty` | no |

[`Loader`] adds hooks that transform keys and values as documents are built, like
betty's `onFieldName` and `onValue`.

## Betty

[Betty](https://github.com/nprapps/betty) is the ArchieML dialect NPR uses in its
[interactive template](https://github.com/nprapps/interactive-template). It adds:

- `key::` … `::key` multi-line values, which can contain any text;
- `{/name}` and `[/name]`, which close blocks back to the one called `name`;
- list items that start at any repeated key, not only the first.

It also differs from standard ArchieML in smaller ways; for instance, `key:` with no
value on the last line is ignored, and `* item` outside a list hides the keys that
follow it. `Options::BETTY` reproduces betty 1.0.11's output exactly, including those
differences, and is checked against betty itself. The parser's events then describe
betty's instructions: `Start` and `End` aren't balanced, [`Event::EndNamed`] closes
named blocks, and [`Event::Text`] can appear outside freeform arrays.

Where betty throws an error and produces nothing, archier carries on:

- an array key made only of `+` and `.`, such as `[+.]`, is plain text;
- a `key::` whose key isn't a valid JavaScript pattern, such as `a(::`, closes at
  the first `::` line containing the key;
- after a `{/name}` or `[/name]` with no such block, values are dropped until the
  next top-level block;
- any other instruction betty would throw on is skipped.

`__proto__` is an ordinary key, rather than JavaScript's prototype.

License: MIT
