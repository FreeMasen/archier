//! A pull parser for [ArchieML](https://archieml.org), modeled on
//! [pulldown-cmark](https://docs.rs/pulldown-cmark).
//!
//! [`Parser`] is an iterator over [`Event`]s describing the structure of a document:
//! balanced [`Event::Start`]/[`Event::End`] pairs for object blocks, arrays and array
//! elements, with values in between. Values borrow from the source whenever possible.
//!
//! ```
//! use archier::{Event, Parser, Tag};
//!
//! let text = "
//! headline: Hello
//! [quotes]
//! * One
//! * Two,
//! on two lines
//! :end
//! []
//! ";
//! for event in Parser::new(text) {
//!     match event {
//!         Event::Pair(key, value) => println!("{key} = {value:?}"),
//!         Event::Start(Tag::Array { key, .. }) => println!("array {key}"),
//!         Event::Item(value) => println!("  - {value:?}"),
//!         _ => {}
//!     }
//! }
//! ```
//!
//! To get the parsed document as a tree, use [`load`], which builds a [`Map`] from the
//! events. The tree borrows from the source text too; [`Map::into_owned`] detaches it.
//!
//! ```
//! let doc = archier::load("{person}\nname: Archie\n{}\n");
//! let person = doc["person"].as_object().unwrap();
//! assert_eq!(person["name"].as_str(), Some("Archie"));
//! ```
//!
//! # Compatibility
//!
//! The parser targets the [ArchieML 1.0 candidate
//! recommendation](https://archieml.org/spec/1.0/CR-20151015.html) and passes the
//! official conformance suite, which also covers behavior adopted after that draft:
//! Unicode keys, nested object blocks (`{.key}`) and blocks nested in freeform arrays.
//!
//! Where neither the spec nor the suite defines the outcome, the parser mirrors the
//! reference implementation, [archieml-js](https://github.com/newsdev/archieml-js), with
//! one exception: plain text inside a `{+key}` block, which makes archieml-js throw, is
//! ignored. archieml-js behavior that contradicts the spec is not reproduced.
//!
//! # Options
//!
//! [`Parser::new_ext`] and [`load_ext`] take [`Options`] for the spec's optional and
//! deprecated features. Each is off unless requested, and also needs its Cargo feature:
//!
//! | Option | Spec | Cargo feature | Feature on by default |
//! |---|---|---|---|
//! | `Options::CASE_INSENSITIVE_KEYS` | §1.3, optional | `case-insensitive` | yes |
//! | `Options::ENABLE_INLINE_COMMENTS` | §7, deprecated | `inline-comments` | no |

#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(feature = "inline-comments")]
mod comments;
mod options;
mod parser;
mod scanners;
pub mod value;

pub use options::Options;
pub use parser::{Event, Key, OffsetIter, Parser, Tag, TagEnd};
pub use value::{Map, Value, load, load_ext};

/// The version of the ArchieML spec this crate implements, which (as the spec asks of
/// parsers written before 1.0 is final) names the candidate recommendation followed.
pub const SPEC_VERSION: &str = "1.0 CR 2015-10-15";
