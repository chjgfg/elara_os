//! Structured expansion boundaries shared by lexer validation and runtime.
use super::lexer::{self, Error};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Command,
    Arithmetic,
    Parameter,
}
pub struct Capture<'a> {
    pub kind: Kind,
    pub body: &'a str,
    pub end: usize,
}

/// Read a balanced expansion without erasing the quotes inside its program.
pub fn capture(text: &str, start: usize) -> Result<Capture<'_>, Error> {
    capture_inner(text, start, 0)
}
fn capture_inner(text: &str, start: usize, call_depth: usize) -> Result<Capture<'_>, Error> {
    if call_depth >= 32 {
        return Err(Error::new(start, "expansion nesting exceeds 32"));
    }
    let tail = &text[start..];
    let (kind, open, close, body_start) = if tail.starts_with("$((") {
        (Kind::Arithmetic, '(', ')', start + 3)
    } else if tail.starts_with("$(") {
        (Kind::Command, '(', ')', start + 2)
    } else if tail.starts_with("${") {
        (Kind::Parameter, '{', '}', start + 2)
    } else {
        return Err(Error::new(start, "expansion opener expected"));
    };
    let mut depth = if kind == Kind::Arithmetic { 2usize } else { 1 };
    let mut quote = None;
    let mut chars = text[body_start..].char_indices().peekable();
    while let Some((relative, c)) = chars.next() {
        let byte = body_start + relative;
        if c == '\\' && quote != Some('\'') {
            chars.next();
            continue;
        }
        if c == '$'
            && quote != Some('\'')
            && chars.peek().is_some_and(|(_, c)| ['(', '{'].contains(c))
        {
            let nested = capture_inner(text, byte, call_depth + 1)?;
            while chars
                .peek()
                .is_some_and(|(relative, _)| body_start + relative < nested.end)
            {
                chars.next();
            }
            continue;
        }
        if let Some(delimiter) = quote {
            if c == delimiter {
                quote = None;
            }
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
            continue;
        }
        if kind == Kind::Command
            && c == '#'
            && (byte == body_start
                || text[..byte]
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_whitespace() || [';', '|', '&', '(', ')'].contains(&c)))
        {
            while chars.peek().is_some_and(|(_, c)| *c != '\n') {
                chars.next();
            }
            continue;
        }
        if kind == Kind::Command && depth == 1 && c == ')' {
            let prefix = &text[body_start..byte];
            // In a case clause, an unparenthesized arm's ')' is not the
            // substitution terminator. Ask the same grammar, not a regex.
            if prefix.split_whitespace().any(|word| word == "case")
                && super::parser::parse_with_depth(prefix, call_depth + 1).is_err()
            {
                continue;
            }
        }
        if c == open {
            depth += 1;
            if depth > 32 {
                return Err(Error::new(byte, "expansion nesting exceeds 32"));
            }
        } else if c == close {
            depth -= 1;
            if depth == 0 {
                let end_body = if kind == Kind::Arithmetic {
                    byte - 1
                } else {
                    byte
                };
                if kind == Kind::Arithmetic && !text[..byte].ends_with(')') {
                    return Err(Error::new(byte, "arithmetic expansion requires ))"));
                }
                return Ok(Capture {
                    kind,
                    body: &text[body_start..end_body],
                    end: byte + 1,
                });
            }
        }
    }
    Err(Error::new(start, "unclosed expansion"))
}

pub enum Parameter<'a> {
    Value(&'a str),
    Length(&'a str),
    Replace {
        name: &'a str,
        all: bool,
        pattern: &'a str,
        replacement: &'a str,
    },
    Trim {
        name: &'a str,
        prefix: bool,
        longest: bool,
        word: &'a str,
    },
    Modify {
        name: &'a str,
        colon: bool,
        op: char,
        word: &'a str,
    },
}
pub fn parameter(text: &str) -> Result<Parameter<'_>, Error> {
    let is_name = |name: &str| {
        lexer::valid_name(name)
            || ["?", "#", "@"].contains(&name)
            || (!name.is_empty() && name.parse::<usize>().is_ok())
    };
    if let Some(name) = text.strip_prefix('#').filter(|name| !name.is_empty()) {
        if is_name(name) && name != "@" {
            return Ok(Parameter::Length(name));
        }
    }
    if is_name(text) {
        return Ok(Parameter::Value(text));
    }
    for (byte, c) in text.char_indices() {
        if c == '/' && is_name(&text[..byte]) && &text[..byte] != "@" {
            let rest = &text[byte + 1..];
            let all = rest.starts_with('/');
            let rest = &rest[usize::from(all)..];
            let at = replacement_separator(rest)?;
            let (pattern, replacement) = at.map_or((rest, ""), |at| (&rest[..at], &rest[at + 1..]));
            return Ok(Parameter::Replace {
                name: &text[..byte],
                all,
                pattern,
                replacement,
            });
        }
        if ['#', '%'].contains(&c) && is_name(&text[..byte]) && &text[..byte] != "@" {
            let rest = &text[byte + 1..];
            let longest = rest.starts_with(c);
            return Ok(Parameter::Trim {
                name: &text[..byte],
                prefix: c == '#',
                longest,
                word: &rest[usize::from(longest)..],
            });
        }
        if c == ':' || ['-', '+', '=', '?'].contains(&c) {
            let name = &text[..byte];
            if !is_name(name) || name == "@" {
                break;
            }
            let colon = c == ':';
            let rest = &text[byte + usize::from(colon)..];
            let op = rest.chars().next().unwrap_or('\0');
            if !['-', '+', '=', '?'].contains(&op) {
                break;
            }
            if op == '=' && !lexer::valid_name(name) {
                return Err(Error::new(
                    0,
                    "cannot assign a positional/special parameter",
                ));
            }
            return Ok(Parameter::Modify {
                name,
                colon,
                op,
                word: &rest[op.len_utf8()..],
            });
        }
    }
    Err(Error::new(0, "invalid or unsupported parameter expansion"))
}

fn replacement_separator(text: &str) -> Result<Option<usize>, Error> {
    let mut chars = text.char_indices().peekable();
    let mut quote = None;
    while let Some((at, c)) = chars.next() {
        if c == '\\' && quote != Some('\'') {
            chars.next();
            continue;
        }
        if c == '$'
            && quote != Some('\'')
            && chars.peek().is_some_and(|(_, c)| ['(', '{'].contains(c))
        {
            let captured = capture(text, at)?;
            while chars.peek().is_some_and(|(at, _)| *at < captured.end) {
                chars.next();
            }
            continue;
        }
        if let Some(delimiter) = quote {
            if c == delimiter {
                quote = None;
            }
            continue;
        }
        if c == '\'' || c == '"' {
            quote = Some(c);
        } else if c == '/' {
            return Ok(Some(at));
        }
    }
    Ok(None)
}
pub fn validate_literal_pattern(text: &str, depth: usize) -> Result<(), Error> {
    validate_default(text, depth)?;
    let mut pattern = String::new();
    for token in lexer::lex(text)? {
        if let lexer::Kind::Word(word) = token.kind {
            for part in word.parts {
                if std::matches!(part.quote, lexer::Quote::Bare | lexer::Quote::Double)
                    && part.text.contains('$')
                {
                    return Ok(());
                }
                if part.quote == lexer::Quote::Bare {
                    pattern.push_str(&part.text);
                } else {
                    pattern.push_str(&super::pattern::escape(&part.text));
                }
            }
        }
    }
    if super::pattern::has_wildcards(&pattern).map_err(|e| Error::new(0, e))? {
        return Err(Error::new(
            0,
            "parameter replacement supports only literal patterns",
        ));
    }
    Ok(())
}

pub fn validate_default(word: &str, depth: usize) -> Result<(), Error> {
    for token in lexer::lex(word)? {
        match token.kind {
            lexer::Kind::Word(word) => lexer::validate_at_depth(&word, depth)?,
            _ => {
                return Err(Error::new(
                    token.byte,
                    "parameter default requires words, not a command list",
                ))
            }
        }
    }
    Ok(())
}
