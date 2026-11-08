//! Bounded wildcard matching; quotes are represented by backslash-escaped literals.
use std::format;
#[derive(Clone)]
enum Atom {
    Literal(char),
    Any,
    Star,
    Class {
        negate: bool,
        ranges: Vec<(char, char)>,
    },
}
impl Atom {
    fn accepts(&self, c: char) -> bool {
        match self {
            Self::Literal(literal) => *literal == c,
            Self::Any => true,
            Self::Star => false,
            Self::Class { negate, ranges } => {
                ranges.iter().any(|(a, b)| *a <= c && c <= *b) != *negate
            }
        }
    }
}
fn parse(pattern: &str) -> Result<Vec<Atom>, String> {
    if pattern.len() > 4096 {
        return Err("pattern exceeds 4096 bytes".into());
    }
    let chars: Vec<char> = pattern.chars().collect();
    let mut atoms = Vec::new();
    let mut at = 0;
    while at < chars.len() {
        let c = chars[at];
        at += 1;
        match c {
            '\\' if at < chars.len() => {
                atoms.push(Atom::Literal(chars[at]));
                at += 1;
            }
            '*' => {
                if !std::matches!(atoms.last(), Some(Atom::Star)) {
                    atoms.push(Atom::Star);
                }
            }
            '?' => atoms.push(Atom::Any),
            '[' => {
                let mut end = at;
                while end < chars.len() && chars[end] != ']' {
                    end += 1;
                }
                if end == chars.len() || end == at {
                    atoms.push(Atom::Literal('['));
                    continue;
                }
                let negate = ['!', '^'].contains(&chars[at]);
                let mut index = at + usize::from(negate);
                let mut ranges = Vec::new();
                while index < end {
                    if index + 2 < end && chars[index + 1] == '-' {
                        if chars[index] > chars[index + 2] {
                            return Err(format!(
                                "reversed pattern range: {}-{}",
                                chars[index],
                                chars[index + 2]
                            ));
                        }
                        ranges.push((chars[index], chars[index + 2]));
                        index += 3;
                    } else {
                        ranges.push((chars[index], chars[index]));
                        index += 1;
                    }
                }
                atoms.push(Atom::Class { negate, ranges });
                at = end + 1;
            }
            _ => atoms.push(Atom::Literal(c)),
        }
    }
    Ok(atoms)
}
pub fn has_wildcards(pattern: &str) -> Result<bool, String> {
    let mut chars = pattern.chars();
    let mut magic = false;
    while let Some(c) = chars.next() {
        if c == '\\' {
            chars.next();
        } else if ['*', '?', '['].contains(&c) {
            magic = true;
            break;
        }
    }
    if !magic {
        return Ok(false);
    }
    Ok(parse(pattern)?
        .iter()
        .any(|atom| !std::matches!(atom, Atom::Literal(_))))
}
pub fn literal(pattern: &str) -> Result<String, String> {
    parse(pattern)?
        .iter()
        .map(|atom| match atom {
            Atom::Literal(c) => Ok(*c),
            _ => Err("pattern is not literal".to_owned()),
        })
        .collect()
}
pub fn escape(text: &str) -> String {
    let mut output = String::new();
    for c in text.chars() {
        if ['*', '?', '[', ']', '\\'].contains(&c) {
            output.push('\\');
        }
        output.push(c);
    }
    output
}
pub fn matches(pattern: &str, text: &str) -> Result<bool, String> {
    let atoms = parse(pattern)?;
    if text.len() > 65536 {
        return Err("pattern input exceeds 64 KiB".into());
    }
    let text: Vec<char> = text.chars().collect();
    let (mut p, mut t) = (0usize, 0usize);
    let mut star = None;
    let mut steps = 0;
    while t < text.len() {
        steps += 1;
        if steps > 100000 {
            return Err("pattern matching budget exhausted".into());
        }
        if std::matches!(atoms.get(p), Some(Atom::Star)) {
            star = Some((p + 1, t));
            p += 1;
        } else if atoms.get(p).is_some_and(|atom| atom.accepts(text[t])) {
            p += 1;
            t += 1;
        } else if let Some((next, matched)) = star {
            let matched = matched + 1;
            star = Some((next, matched));
            p = next;
            t = matched;
        } else {
            return Ok(false);
        }
    }
    while std::matches!(atoms.get(p), Some(Atom::Star)) {
        p += 1;
    }
    Ok(p == atoms.len())
}
