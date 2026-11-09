//! NPR's betty dialect of ArchieML, enabled by
//! [`Options::BETTY`](crate::Options::BETTY).
//!
//! Betty (<https://github.com/nprapps/betty>) adds `key::` … `::key` multi-line values,
//! `{/name}` and `[/name]` to close back to a named block, and list items that start at
//! any repeated key. It also differs from the spec in many smaller ways. This module
//! reproduces betty's output exactly, checked against betty itself, rather than adding
//! its syntax to standard ArchieML.

mod assembler;
mod parser;
mod pattern;

pub(crate) use assembler::assemble;
pub(crate) use parser::BettyParser;

#[cfg(test)]
mod tests {
    use std::borrow::Cow;

    use crate::{Loader, Options, Value};

    /// Converts a document to JSON-like text for comparing with betty's output.
    fn json(value: &Value<'_>) -> String {
        match value {
            Value::String(s) => format!("{s:?}"),
            Value::Array(items) => {
                let items: Vec<_> = items.iter().map(json).collect();
                format!("[{}]", items.join(","))
            }
            Value::Object(map) => {
                let entries: Vec<_> = map
                    .iter()
                    .map(|(k, v)| format!("{k:?}:{}", json(v)))
                    .collect();
                format!("{{{}}}", entries.join(","))
            }
        }
    }

    const DOCUMENT: &str = "Headline: Betty
note::
Any text, even
key: lines and [brackets]
::NOTE
{story.meta}
by: Me
{/story}
after: top level
[people]
name: Ann
role: editor
name: Bo
[/people]
[+body]
Some text.
h2: A heading
[]
empty:
last: one";

    /// The output of betty 1.0.11 for `DOCUMENT`.
    const EXPECTED: &str = r#"{"Headline":"Betty","note":"Any text, even\nkey: lines and [brackets]","story":{"meta":{"by":"Me"}},"after":"top level","people":[{"name":"Ann","role":"editor"},{"name":"Bo"}],"body":[{"type":"text","value":"Some text."},{"type":"h2","value":"A heading"}],"empty":"","last":"one"}"#;

    #[test]
    fn matches_betty() {
        let doc = Loader::new(Options::BETTY).load(DOCUMENT);
        assert_eq!(json(&Value::Object(doc)), EXPECTED);
    }

    #[test]
    fn hooks_rename_fields() {
        let loader = Loader::new(Options::BETTY).on_field_name(|name| {
            let mut chars = name.chars();
            match chars.next() {
                Some(first) => Cow::Owned(first.to_lowercase().chain(chars).collect()),
                None => name,
            }
        });
        let doc = loader.load(DOCUMENT);
        assert_eq!(
            json(&Value::Object(doc)),
            EXPECTED.replace(r#""Headline""#, r#""headline""#)
        );
    }

    #[test]
    fn skips_what_betty_throws_on() {
        // Betty throws on each of these lines; archier's choices are documented in the
        // crate docs.
        let text = "{/missing}\nlost: yes\n{reset}\nkept: yes\n[+.]\na(::\nnever closed\n::a(";
        let doc = Loader::new(Options::BETTY).load(text);
        assert_eq!(
            json(&Value::Object(doc)),
            r#"{"reset":{"kept":"yes","a(":"never closed"}}"#
        );
    }

    #[test]
    fn borrows_from_the_source() {
        let doc = Loader::new(Options::BETTY).load("key: value\nlist::\nmany\nlines\n::list");
        assert!(matches!(&doc["key"], Value::String(Cow::Borrowed("value"))));
        assert!(matches!(
            &doc["list"],
            Value::String(Cow::Borrowed("many\nlines"))
        ));
    }
}
