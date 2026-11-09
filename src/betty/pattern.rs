//! The JavaScript regular expressions betty compiles from keys.
//!
//! Betty closes a `key::` value at the first `::name` line where `name` matches
//! `new RegExp(key.toLowerCase(), "i")`. Keys can't contain `\`, `[`, `{`, `*`, `?` or
//! `/`, so the patterns only ever use literals, `.`, `+`, `^`, `$`, `|` and groups. This
//! implements exactly that subset with JavaScript semantics: case-insensitive matching
//! of UTF-16 code units, searching anywhere in the input.

/// A compiled pattern, or `None` where JavaScript would throw a `SyntaxError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pattern(Disjunction);

type Disjunction = Vec<Vec<Term>>;

#[derive(Debug, Clone, PartialEq, Eq)]
struct Term {
    atom: Atom,
    repeat: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Atom {
    Unit(u16),
    Any,
    Start,
    End,
    Group(Disjunction),
}

impl Pattern {
    pub(crate) fn new(source: &str) -> Option<Pattern> {
        let units: Vec<u16> = source.encode_utf16().collect();
        let mut pos = 0;
        let pattern = disjunction(&units, &mut pos)?;
        // A `)` that closes nothing stops `disjunction` early.
        (pos == units.len()).then_some(Pattern(pattern))
    }

    /// Whether the pattern matches anywhere in `text`, as `RegExp.prototype.test`.
    pub(crate) fn test(&self, text: &str) -> bool {
        let units: Vec<u16> = text.encode_utf16().map(canonicalize).collect();
        (0..=units.len()).any(|start| match_disjunction(&self.0, &units, start, &mut |_| true))
    }
}

fn disjunction(units: &[u16], pos: &mut usize) -> Option<Disjunction> {
    let mut alternatives: Disjunction = vec![Vec::new()];
    while let Some(&unit) = units.get(*pos) {
        let atom = match unit {
            b if b == b'|' as u16 => {
                *pos += 1;
                alternatives.push(Vec::new());
                continue;
            }
            b if b == b')' as u16 => break,
            b if b == b'+' as u16 => {
                // Quantifiers need something repeatable before them, and can't stack.
                let term = alternatives.last_mut()?.last_mut()?;
                if term.repeat || matches!(term.atom, Atom::Start | Atom::End) {
                    return None;
                }
                term.repeat = true;
                *pos += 1;
                continue;
            }
            b if b == b'(' as u16 => {
                *pos += 1;
                let group = disjunction(units, pos)?;
                if units.get(*pos) != Some(&(b')' as u16)) {
                    return None;
                }
                Atom::Group(group)
            }
            b if b == b'.' as u16 => Atom::Any,
            b if b == b'^' as u16 => Atom::Start,
            b if b == b'$' as u16 => Atom::End,
            unit => Atom::Unit(canonicalize(unit)),
        };
        *pos += 1;
        alternatives.last_mut()?.push(Term {
            atom,
            repeat: false,
        });
    }
    Some(alternatives)
}

/// JavaScript's case-insensitive `Canonicalize` for non-Unicode patterns: a code unit
/// stands for its uppercase form, unless that isn't a single code unit, or would map a
/// non-ASCII unit to ASCII.
fn canonicalize(unit: u16) -> u16 {
    let Some(c) = char::from_u32(unit.into()) else {
        return unit;
    };
    let mut upper = c.to_uppercase();
    match (upper.next(), upper.next()) {
        (Some(u), None) if u as u32 <= 0xFFFF && !(unit >= 128 && (u as u32) < 128) => u as u16,
        _ => unit,
    }
}

fn is_line_terminator(unit: u16) -> bool {
    matches!(unit, 0x0A | 0x0D | 0x2028 | 0x2029)
}

type Continuation<'k> = dyn FnMut(usize) -> bool + 'k;

fn match_disjunction(
    alternatives: &Disjunction,
    input: &[u16],
    pos: usize,
    k: &mut Continuation<'_>,
) -> bool {
    alternatives
        .iter()
        .any(|terms| match_terms(terms, input, pos, k))
}

fn match_terms(terms: &[Term], input: &[u16], pos: usize, k: &mut Continuation<'_>) -> bool {
    let Some((term, rest)) = terms.split_first() else {
        return k(pos);
    };
    let mut next = |p: usize| match_terms(rest, input, p, k);
    if term.repeat {
        match_repeat(&term.atom, input, pos, &mut next)
    } else {
        match_atom(&term.atom, input, pos, &mut next)
    }
}

/// One or more matches of `atom`, as many as possible first.
fn match_repeat(atom: &Atom, input: &[u16], pos: usize, k: &mut Continuation<'_>) -> bool {
    match_atom(atom, input, pos, &mut |p| {
        // An iteration that matched nothing can't be repeated.
        (p != pos && match_repeat(atom, input, p, k)) || k(p)
    })
}

fn match_atom(atom: &Atom, input: &[u16], pos: usize, k: &mut Continuation<'_>) -> bool {
    match atom {
        Atom::Unit(unit) => input.get(pos) == Some(unit) && k(pos + 1),
        Atom::Any => input.get(pos).is_some_and(|&u| !is_line_terminator(u)) && k(pos + 1),
        Atom::Start => pos == 0 && k(pos),
        Atom::End => pos == input.len() && k(pos),
        Atom::Group(alternatives) => match_disjunction(alternatives, input, pos, k),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test(pattern: &str, text: &str) -> Option<bool> {
        Pattern::new(pattern).map(|p| p.test(text))
    }

    #[test]
    fn matches_like_javascript() {
        assert_eq!(test("longer", "  LONGER stuff"), Some(true));
        assert_eq!(test("a.b", "axb"), Some(true));
        assert_eq!(test("a.b", "ab"), Some(false));
        assert_eq!(test("^ab$", "ab"), Some(true));
        assert_eq!(test("^ab$", "xab"), Some(false));
        assert_eq!(test("a|b", "b"), Some(true));
        assert_eq!(test("(ab)+c", "ababc"), Some(true));
        assert_eq!(test("a+b", "aaab"), Some(true));
        assert_eq!(test("()+x", "x"), Some(true));
        assert_eq!(test("π", "Π"), Some(true));
        // `.` matches one UTF-16 code unit, so not a whole astral character.
        assert_eq!(test("^.$", "🐮"), Some(false));
        assert_eq!(test("^..$", "🐮"), Some(true));
    }

    #[test]
    fn rejects_what_javascript_rejects() {
        for pattern in ["+a", "a++", "^+", "(+)", "(a", "a)", "|+"] {
            assert_eq!(Pattern::new(pattern), None, "{pattern}");
        }
    }
}
