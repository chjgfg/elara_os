//! Bounded, UTF-8 shell lexer. Quotes and escapes remain structured until expansion.
use std::{fmt, matches, write};

pub const MAX_BYTES: usize = 64 * 1024;
pub const MAX_TOKENS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quote {
    Bare,
    Single,
    Double,
    Literal,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Part {
    pub quote: Quote,
    pub text: String,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Word {
    pub parts: Vec<Part>,
}
impl Word {
    pub fn bare(&self) -> Option<&str> {
        match self.parts.as_slice() {
            [Part {
                quote: Quote::Bare,
                text,
            }] => Some(text),
            _ => None,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Newline,
    Semi,
    DoubleSemi,
    Background,
    Pipe,
    And,
    Or,
    Input,
    Output,
    Append,
    Left,
    Right,
    Open,
    Close,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Kind {
    Word(Word),
    Operator(Op),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    pub byte: usize,
    pub end: usize,
    pub kind: Kind,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Error {
    pub byte: usize,
    pub message: String,
}
impl Error {
    pub fn new(byte: usize, message: impl Into<String>) -> Self {
        Self {
            byte,
            message: message.into(),
        }
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "script byte {}: {}", self.byte, self.message)
    }
}
impl std::error::Error for Error {}

fn push(word: &mut Vec<Part>, quote: Quote, c: char) {
    if let Some(last) = word.last_mut() {
        if last.quote == quote {
            last.text.push(c);
            return;
        }
    }
    word.push(Part {
        quote,
        text: c.to_string(),
    });
}
fn emit(out: &mut Vec<Token>, token: Token) -> Result<(), Error> {
    if out.len() >= MAX_TOKENS {
        return Err(Error::new(token.byte, "token limit exceeded"));
    }
    out.push(token);
    Ok(())
}
fn flush(
    out: &mut Vec<Token>,
    start: &mut Option<usize>,
    end: usize,
    word: &mut Vec<Part>,
) -> Result<(), Error> {
    if let Some(byte) = start.take() {
        emit(
            out,
            Token {
                byte,
                end,
                kind: Kind::Word(Word {
                    parts: std::mem::take(word),
                }),
            },
        )?;
    }
    Ok(())
}

pub fn lex(source: &str) -> Result<Vec<Token>, Error> {
    if source.len() > MAX_BYTES {
        return Err(Error::new(MAX_BYTES, "script exceeds 64 KiB"));
    }
    if let Some((byte, _)) = source
        .char_indices()
        .find(|(_, c)| c.is_control() && !['\n', '\t'].contains(c))
    {
        return Err(Error::new(byte, "control character refused"));
    }
    let mut chars = source.char_indices().peekable();
    let mut out = Vec::new();
    let mut word = Vec::new();
    let mut start = None;
    let mut quote = Quote::Bare;
    let mut opened = 0;
    while let Some((byte, c)) = chars.next() {
        if quote == Quote::Single {
            if c == '\'' {
                quote = Quote::Bare;
            } else {
                push(&mut word, quote, c);
            }
            continue;
        }
        if c == '\\' {
            let (_, escaped) = chars
                .next()
                .ok_or_else(|| Error::new(byte, "trailing escape"))?;
            if escaped == '\n' {
                continue;
            }
            start.get_or_insert(byte);
            if quote == Quote::Double && !['$', '`', '"', '\\'].contains(&escaped) {
                push(&mut word, quote, '\\');
                push(&mut word, quote, escaped);
            } else {
                push(&mut word, Quote::Literal, escaped);
            }
            continue;
        }
        if c == '`' {
            return Err(Error::new(
                byte,
                "backtick substitution is not supported; use $(...)",
            ));
        }
        if c == '$' && chars.peek().is_some_and(|(_, c)| ['(', '{'].contains(c)) {
            let expansion = super::expansion::capture(source, byte)?;
            start.get_or_insert(byte);
            for character in source[byte..expansion.end].chars() {
                push(&mut word, quote, character);
            }
            while chars.peek().is_some_and(|(at, _)| *at < expansion.end) {
                chars.next();
            }
            continue;
        }
        if quote == Quote::Double {
            if c == '"' {
                quote = Quote::Bare;
            } else {
                push(&mut word, quote, c);
            }
            continue;
        }
        match c {
            ' ' | '\t' => flush(&mut out, &mut start, byte, &mut word)?,
            '#' if start.is_none() => {
                while chars.peek().is_some_and(|(_, c)| *c != '\n') {
                    chars.next();
                }
            }
            '\'' | '"' => {
                start.get_or_insert(byte);
                quote = if c == '\'' {
                    Quote::Single
                } else {
                    Quote::Double
                };
                opened = byte;
                word.push(Part {
                    quote,
                    text: String::new(),
                });
            }
            '\n' | ';' | '|' | '&' | '<' | '>' | '(' | ')' | '{' | '}'
                if !['{', '}'].contains(&c) || start.is_none() =>
            {
                let next = chars.peek().map(|(_, c)| *c);
                let mut end = byte + c.len_utf8();
                let op = match (c, next) {
                    (';', Some(';')) => {
                        chars.next();
                        end += 1;
                        Op::DoubleSemi
                    }
                    ('<', Some('<')) | ('|', Some('&')) => {
                        return Err(Error::new(
                            byte,
                            "heredoc/case/stderr-pipe syntax is not supported",
                        ))
                    }
                    ('&', Some('&')) => {
                        chars.next();
                        end += 1;
                        Op::And
                    }
                    ('&', _) => Op::Background,
                    ('|', Some('|')) => {
                        chars.next();
                        end += 1;
                        Op::Or
                    }
                    ('>', Some('>')) => {
                        chars.next();
                        end += 1;
                        Op::Append
                    }
                    ('\n', _) => Op::Newline,
                    (';', _) => Op::Semi,
                    ('|', _) => Op::Pipe,
                    ('<', _) => Op::Input,
                    ('>', _) => Op::Output,
                    ('(', _) => Op::Left,
                    (')', _) => Op::Right,
                    ('{', _) => Op::Open,
                    _ => Op::Close,
                };
                flush(&mut out, &mut start, byte, &mut word)?;
                emit(
                    &mut out,
                    Token {
                        byte,
                        end,
                        kind: Kind::Operator(op),
                    },
                )?;
            }
            _ => {
                start.get_or_insert(byte);
                push(&mut word, Quote::Bare, c);
            }
        }
    }
    if quote != Quote::Bare {
        return Err(Error::new(opened, "unclosed quote"));
    }
    flush(&mut out, &mut start, source.len(), &mut word)?;
    Ok(out)
}

pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c == '_' || c.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// Validate all expansion syntax before any command can run.
pub fn validate(word: &Word) -> Result<(), Error> {
    validate_at_depth(word, 0)
}
pub(super) fn validate_at_depth(word: &Word, depth: usize) -> Result<(), Error> {
    if depth >= 32 {
        return Err(Error::new(0, "expansion nesting exceeds 32"));
    }
    use super::expansion::{self, Kind as ExpansionKind, Parameter};
    let whole_at = word.parts.len() == 1
        && matches!(word.parts[0].quote, Quote::Bare | Quote::Double)
        && matches!(word.parts[0].text.as_str(), "$@" | "${@}");
    for (part_index, part) in word.parts.iter().enumerate() {
        if matches!(part.quote, Quote::Single | Quote::Literal) {
            continue;
        }
        let mut chars = part.text.char_indices().peekable();
        while let Some((at, c)) = chars.next() {
            if c == '$' {
                match chars.peek().map(|(_, c)| *c) {
                    Some('(' | '{') => {
                        let capture = expansion::capture(&part.text, at)?;
                        match capture.kind {
                            ExpansionKind::Command => {
                                super::parser::parse_with_depth(capture.body, depth + 1)?;
                            }
                            ExpansionKind::Arithmetic => {
                                super::arithmetic::validate(capture.body)
                                    .map_err(|e| Error::new(at, e))?;
                            }
                            ExpansionKind::Parameter => match expansion::parameter(capture.body)? {
                                Parameter::Value("@") if !whole_at => {
                                    return Err(Error::new(at, "$@ must occupy a whole word"))
                                }
                                Parameter::Replace {
                                    pattern,
                                    replacement,
                                    ..
                                } => {
                                    expansion::validate_literal_pattern(pattern, depth + 1)?;
                                    expansion::validate_default(replacement, depth + 1)?;
                                }
                                Parameter::Modify { word, .. } | Parameter::Trim { word, .. } => {
                                    expansion::validate_default(word, depth + 1)?
                                }
                                _ => {}
                            },
                        }
                        while chars.peek().is_some_and(|(at, _)| *at < capture.end) {
                            chars.next();
                        }
                    }
                    Some('@') if !whole_at => {
                        return Err(Error::new(at, "$@ must occupy a whole word"))
                    }
                    Some('?' | '#' | '@') => {
                        chars.next();
                    }
                    Some(c) if c.is_ascii_digit() => {
                        chars.next();
                    }
                    Some(c) if c == '_' || c.is_ascii_alphabetic() => {
                        while chars
                            .peek()
                            .is_some_and(|(_, c)| *c == '_' || c.is_ascii_alphanumeric())
                        {
                            chars.next();
                        }
                    }
                    Some('$' | '!' | '*' | '-') => {
                        return Err(Error::new(at, "unsupported special parameter"))
                    }
                    _ => {}
                }
            } else if part.quote == Quote::Bare
                && c == '~'
                && part_index == 0
                && at == 0
                && !part.text[1..].is_empty()
                && !part.text[1..].starts_with('/')
            {
                return Err(Error::new(
                    at,
                    "named-user tilde expansion is not supported",
                ));
            }
        }
    }
    Ok(())
}
