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
//! events:
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
//! The deprecated inline comments are not supported.

mod parser;
mod scanners;
pub mod value;

pub use parser::{Event, Key, OffsetIter, Parser, Tag, TagEnd};
pub use value::{Map, Value, load};
