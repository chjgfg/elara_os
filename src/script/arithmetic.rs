//! Checked integer arithmetic for `$((...))`. No host evaluator or wrapping math.
use std::format;

#[derive(Clone, Debug, PartialEq)]
enum Token {
    Number(i64),
    Name(String),
    Op(String),
    End,
}
struct Parser<'a> {
    tokens: Vec<Token>,
    at: usize,
    depth: usize,
    lookup: &'a dyn Fn(&str) -> Result<i64, String>,
}

pub fn evaluate(text: &str, lookup: &dyn Fn(&str) -> Result<i64, String>) -> Result<i64, String> {
    evaluate_mode(text, lookup, true)
}
fn evaluate_mode(
    text: &str,
    lookup: &dyn Fn(&str) -> Result<i64, String>,
    active: bool,
) -> Result<i64, String> {
    let mut chars = text.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(c) = chars.next() {
        if c.is_ascii_whitespace() {
            continue;
        }
        if c.is_ascii_digit() {
            let mut value = c.to_string();
            while chars.peek().is_some_and(char::is_ascii_digit) {
                value.push(chars.next().unwrap());
            }
            tokens.push(Token::Number(
                value.parse().map_err(|_| "arithmetic integer overflow")?,
            ));
        } else if c == '_' || c.is_ascii_alphabetic() {
            let mut name = c.to_string();
            while chars
                .peek()
                .is_some_and(|c| *c == '_' || c.is_ascii_alphanumeric())
            {
                name.push(chars.next().unwrap());
            }
            tokens.push(Token::Name(name));
        } else {
            let mut op = c.to_string();
            if chars.peek().is_some_and(|next| {
                ["<=", ">=", "==", "!=", "&&", "||"].contains(&format!("{c}{next}").as_str())
            }) {
                op.push(chars.next().unwrap());
            }
            if ![
                "+", "-", "*", "/", "%", "(", ")", "!", "<", ">", "<=", ">=", "==", "!=", "&&",
                "||",
            ]
            .contains(&op.as_str())
            {
                return Err(format!("unsupported arithmetic operator: {op}"));
            }
            tokens.push(Token::Op(op));
        }
        if tokens.len() > 4096 {
            return Err("arithmetic token limit exceeded".into());
        }
    }
    tokens.push(Token::End);
    let mut parser = Parser {
        tokens,
        at: 0,
        depth: 0,
        lookup,
    };
    let value = parser.expr(0, active)?;
    if parser.tokens[parser.at] != Token::End {
        return Err("unexpected arithmetic token".into());
    }
    Ok(value)
}
impl Parser<'_> {
    fn expr(&mut self, precedence: u8, active: bool) -> Result<i64, String> {
        if self.depth >= 32 {
            return Err("arithmetic nesting exceeds 32".into());
        }
        self.depth += 1;
        let result = self.expr_inner(precedence, active);
        self.depth -= 1;
        result
    }
    fn expr_inner(&mut self, precedence: u8, active: bool) -> Result<i64, String> {
        let token = self.tokens.get(self.at).cloned().unwrap_or(Token::End);
        self.at += 1;
        let mut value = match token {
            Token::Number(value) => {
                if active {
                    value
                } else {
                    0
                }
            }
            Token::Name(name) => {
                if active {
                    (self.lookup)(&name)?
                } else {
                    0
                }
            }
            Token::Op(op) if op == "(" => {
                let value = self.expr(0, active)?;
                if self.tokens.get(self.at) != Some(&Token::Op(")".into())) {
                    return Err("missing arithmetic closing parenthesis".into());
                }
                self.at += 1;
                value
            }
            Token::Op(op) if ["+", "-", "!"].contains(&op.as_str()) => {
                let value = self.expr(7, active)?;
                if !active {
                    0
                } else {
                    match op.as_str() {
                        "+" => value,
                        "-" => value.checked_neg().ok_or("arithmetic overflow")?,
                        _ => i64::from(value == 0),
                    }
                }
            }
            _ => return Err("arithmetic value expected".into()),
        };
        while let Some(Token::Op(op)) = self.tokens.get(self.at).cloned() {
            let level = match op.as_str() {
                "||" => 1,
                "&&" => 2,
                "==" | "!=" => 3,
                "<" | ">" | "<=" | ">=" => 4,
                "+" | "-" => 5,
                "*" | "/" | "%" => 6,
                _ => break,
            };
            if level < precedence {
                break;
            }
            self.at += 1;
            let evaluate_rhs = active && !(op == "&&" && value == 0 || op == "||" && value != 0);
            let rhs = self.expr(level + 1, evaluate_rhs)?;
            if !active {
                continue;
            }
            value = match op.as_str() {
                "+" => value.checked_add(rhs).ok_or("arithmetic overflow")?,
                "-" => value.checked_sub(rhs).ok_or("arithmetic overflow")?,
                "*" => value.checked_mul(rhs).ok_or("arithmetic overflow")?,
                "/" => value
                    .checked_div(rhs)
                    .ok_or("arithmetic division by zero or overflow")?,
                "%" => value
                    .checked_rem(rhs)
                    .ok_or("arithmetic remainder by zero or overflow")?,
                "==" => i64::from(value == rhs),
                "!=" => i64::from(value != rhs),
                "<" => i64::from(value < rhs),
                ">" => i64::from(value > rhs),
                "<=" => i64::from(value <= rhs),
                ">=" => i64::from(value >= rhs),
                "&&" => i64::from(value != 0 && rhs != 0),
                "||" => i64::from(value != 0 || rhs != 0),
                _ => unreachable!(),
            };
        }
        Ok(value)
    }
}

/// Syntax-only evaluation avoids inspecting variable values or dividing by zero.
pub fn validate(text: &str) -> Result<(), String> {
    evaluate_mode(text, &|_| Ok(0), false).map(|_| ())
}
