//! One syntax frontend for terminal lines and scripts. Session controls are
//! deliberately yielded to the outer UI; they never consume a script's stdin.
use super::lexer::{Error, Quote, Word};
use super::parser::{self, List, Node};
use std::matches;

pub enum PromptInput {
    End,
    Program(String),
    Rejected(&'static str),
}
/// Drain one whole line with bounded storage, including malformed/oversized input.
pub fn read_prompt<R: std::io::BufRead>(input: &mut R) -> std::io::Result<PromptInput> {
    let mut bytes = Vec::new();
    let mut seen = false;
    let mut oversized = false;
    loop {
        let available = input.fill_buf()?;
        if available.is_empty() {
            break;
        }
        seen = true;
        let newline = available.iter().position(|b| *b == b'\n');
        let count = newline.map_or(available.len(), |at| at + 1);
        if !oversized
            && count
                <= super::lexer::MAX_BYTES
                    .saturating_add(2)
                    .saturating_sub(bytes.len())
        {
            bytes.extend_from_slice(&available[..count]);
        } else {
            oversized = true;
        }
        input.consume(count);
        if newline.is_some() {
            break;
        }
    }
    if !seen {
        return Ok(PromptInput::End);
    }
    if oversized {
        return Ok(PromptInput::Rejected("prompt line exceeds 64 KiB"));
    }
    if bytes.last() == Some(&b'\n') {
        bytes.pop();
        if bytes.last() == Some(&b'\r') {
            bytes.pop();
        }
    }
    if bytes.len() > super::lexer::MAX_BYTES {
        return Ok(PromptInput::Rejected("prompt line exceeds 64 KiB"));
    }
    Ok(match String::from_utf8(bytes) {
        Ok(text) => PromptInput::Program(text),
        Err(_) => PromptInput::Rejected("prompt input must be UTF-8"),
    })
}

pub enum Line {
    Empty,
    Session { name: String, words: Vec<Word> },
    Program(List),
}
const CONTROLS: &[&str] = &["help", "info", "clear", "su", "passwd", "useradd"];
fn static_name(word: &Word) -> Option<String> {
    let mut value = String::new();
    for part in &word.parts {
        if matches!(part.quote, Quote::Bare | Quote::Double) && part.text.contains('$') {
            return None;
        }
        value.push_str(&part.text);
    }
    Some(value)
}
/// Audit only allowlisted static operation identities, never expanded values.
pub fn audit_label(ast: &[Node]) -> String {
    let Some(Node::Chain { first, .. }) = ast.first() else {
        return "program".into();
    };
    let Some(command) = first.commands.first() else {
        return "program".into();
    };
    let Some(name) = command
        .words
        .first()
        .and_then(static_name)
        .filter(|n| super::lexer::valid_name(n) || n == ".")
    else {
        return "program".into();
    };
    let operation = command.words.get(1).and_then(Word::bare);
    let safe = match name.as_str() {
        "fd" => operation.filter(|op| {
            [
                "open", "read", "readat", "write", "writeat", "seek", "truncate", "stat", "dup",
                "sync", "close", "closeall", "help",
            ]
            .contains(op)
        }),
        "sh" | "source" | "." => operation.filter(|op| ["-c", "-n"].contains(op)),
        _ => None,
    };
    match safe {
        Some(op) => std::format!("{name} {op}"),
        None => name,
    }
}

pub fn prepare(source: &str) -> Result<Line, Error> {
    let ast = parser::parse(source)?;
    if ast.is_empty() {
        return Ok(Line::Empty);
    }
    if let [Node::Chain { first, rest }] = ast.as_slice() {
        if rest.is_empty() && first.commands.len() == 1 {
            let command = &first.commands[0];
            if command.redirects.is_empty() {
                if let Some(name) = command.words.first().and_then(static_name) {
                    if CONTROLS.contains(&name.as_str()) {
                        return Ok(Line::Session {
                            name,
                            words: command.words.clone(),
                        });
                    }
                }
            }
        }
    }
    reject_compound_controls(&ast)?;
    Ok(Line::Program(ast))
}

// A password/session UI cannot be redirected or embedded in a compound command.
// Reject the entire list before any preceding command can have side effects.
fn reject_compound_controls(list: &[Node]) -> Result<(), Error> {
    for node in list {
        match node {
            Node::Background { body } => {
                reject_compound_controls(std::slice::from_ref(body.as_ref()))?
            }
            Node::Chain { first, rest } => {
                for pipe in std::iter::once(first).chain(rest.iter().map(|(_, p)| p)) {
                    for command in &pipe.commands {
                        if command
                            .words
                            .first()
                            .and_then(static_name)
                            .is_some_and(|name| CONTROLS.contains(&name.as_str()))
                        {
                            return Err(Error::new(
                                0,
                                "session controls require a standalone command without redirection",
                            ));
                        }
                    }
                }
            }
            Node::If {
                branches,
                otherwise,
            } => {
                for (condition, body) in branches {
                    reject_compound_controls(condition)?;
                    reject_compound_controls(body)?;
                }
                reject_compound_controls(otherwise)?;
            }
            Node::Case { branches, .. } => {
                for (_, body) in branches {
                    reject_compound_controls(body)?;
                }
            }
            Node::For { body, .. } | Node::Function { body, .. } => reject_compound_controls(body)?,
            Node::While {
                condition, body, ..
            } => {
                reject_compound_controls(condition)?;
                reject_compound_controls(body)?;
            }
        }
    }
    Ok(())
}
