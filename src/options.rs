//! Options for the spec's optional and deprecated features.

use std::borrow::Cow;
use std::ops::{BitOr, BitOrAssign};

/// Optional parser behavior, combined with `|`. Everything is off by default.
///
/// Each option also needs its Cargo feature: `case-insensitive` (optional in the spec;
/// enabled by default) or `inline-comments` (deprecated in the spec; disabled by
/// default). Without its feature, an option's constant doesn't exist.
///
/// ```
/// # #[cfg(feature = "case-insensitive")] {
/// use archier::{Options, Parser};
///
/// let parser = Parser::new_ext("Key: value", Options::CASE_INSENSITIVE_KEYS);
/// # drop(parser);
/// # }
/// ```
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Options(u32);

impl Options {
    /// Treat keys that differ only in case as the same key (spec §1.3). Array element
    /// delimiters match in any case, and [`load_ext`](crate::load_ext) lowercases every
    /// key in the document. Events still carry keys as written.
    #[cfg(feature = "case-insensitive")]
    #[cfg_attr(docsrs, doc(cfg(feature = "case-insensitive")))]
    pub const CASE_INSENSITIVE_KEYS: Options = Options(1 << 0);

    /// Remove inline comments from values (spec §7, deprecated): text in single square
    /// brackets on one line is dropped, and `[[text]]` becomes `[text]`. Behaves exactly
    /// like archieml-js's `comments` option.
    #[cfg(feature = "inline-comments")]
    #[cfg_attr(docsrs, doc(cfg(feature = "inline-comments")))]
    pub const ENABLE_INLINE_COMMENTS: Options = Options(1 << 1);

    /// Parse NPR's betty dialect instead of standard ArchieML, reproducing betty's output
    /// exactly (see the [crate documentation](crate#betty)). The other options have no
    /// effect in betty mode; use [`Loader`](crate::Loader) hooks to transform keys.
    #[cfg(feature = "betty")]
    #[cfg_attr(docsrs, doc(cfg(feature = "betty")))]
    pub const BETTY: Options = Options(1 << 2);

    pub const fn empty() -> Options {
        Options(0)
    }

    pub const fn contains(self, other: Options) -> bool {
        self.0 & other.0 == other.0
    }

    pub fn insert(&mut self, other: Options) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Options) {
        self.0 &= !other.0;
    }

    /// Whether two keys name the same key.
    pub(crate) fn same_key(self, a: &str, b: &str) -> bool {
        if a == b {
            return true;
        }
        #[cfg(feature = "case-insensitive")]
        if self.contains(Options::CASE_INSENSITIVE_KEYS) {
            return a
                .chars()
                .flat_map(char::to_lowercase)
                .eq(b.chars().flat_map(char::to_lowercase));
        }
        false
    }

    /// A key as it should appear in a built document.
    pub(crate) fn document_key(self, key: Cow<'_, str>) -> Cow<'_, str> {
        #[cfg(feature = "case-insensitive")]
        if self.contains(Options::CASE_INSENSITIVE_KEYS) {
            return Cow::Owned(key.to_lowercase());
        }
        key
    }

    /// Whether [`format_value`](Options::format_value) can change a value.
    pub(crate) fn formats_values(self) -> bool {
        #[cfg(feature = "inline-comments")]
        if self.contains(Options::ENABLE_INLINE_COMMENTS) {
            return true;
        }
        false
    }

    /// Applies the enabled value formatting (removing inline comments) to raw value text.
    pub(crate) fn format_value(self, value: &str) -> Cow<'_, str> {
        #[cfg(feature = "inline-comments")]
        if self.contains(Options::ENABLE_INLINE_COMMENTS) {
            return crate::comments::strip_comments(value);
        }
        Cow::Borrowed(value)
    }
}

impl BitOr for Options {
    type Output = Options;

    fn bitor(self, other: Options) -> Options {
        Options(self.0 | other.0)
    }
}

impl BitOrAssign for Options {
    fn bitor_assign(&mut self, other: Options) {
        self.insert(other);
    }
}
