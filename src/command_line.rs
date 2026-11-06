use std::fmt;
use std::write;

/// Split one command line into words the way a user expects from a Unix shell:
/// whitespace separates words, quotes group them, backslash escapes one character.
/// No variable expansion, globbing, pipes or redirection is performed.
pub fn parse(line: &str) -> Result<Vec<String>, ParseError> {
    let mut words = Vec::new();
    let mut current = String::new();
    let mut in_word = false;
    let mut chars = line.chars().peekable();
    while let Some(character) = chars.next() {
        match character {
            '\'' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('\'') => break,
                        Some(inner) => current.push(inner),
                        None => return Err(ParseError::UnterminatedQuote('\'')),
                    }
                }
            }
            '"' => {
                in_word = true;
                loop {
                    match chars.next() {
                        Some('"') => break,
                        Some('\\') => match chars.next() {
                            Some(escaped @ ('"' | '\\')) => current.push(escaped),
                            Some(other) => {
                                current.push('\\');
                                current.push(other);
                            }
                            None => return Err(ParseError::TrailingBackslash),
                        },
                        Some(inner) => current.push(inner),
                        None => return Err(ParseError::UnterminatedQuote('"')),
                    }
                }
            }
            '\\' => {
                in_word = true;
                match chars.next() {
                    Some(escaped) => current.push(escaped),
                    None => return Err(ParseError::TrailingBackslash),
                }
            }
            '|' | '<' | '>' | ';' | '&' | '`' | '$' | '*' | '?' => {
                return Err(ParseError::UnsupportedSyntax(character));
            }
            _ if character.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut current));
                    in_word = false;
                }
            }
            _ => {
                in_word = true;
                current.push(character);
            }
        }
    }
    if in_word {
        words.push(current);
    }
    Ok(words)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParseError {
    UnterminatedQuote(char),
    TrailingBackslash,
    UnsupportedSyntax(char),
}

impl fmt::Display for ParseError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnterminatedQuote(quote) => write!(output, "Unterminated {quote} quote."),
            Self::TrailingBackslash => write!(output, "A backslash must be followed by a character."),
            Self::UnsupportedSyntax(character) => write!(
                output,
                "'{character}' is not supported: no pipes, redirection, globbing, variables or command chaining."
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, ParseError};
    use std::{assert, assert_eq, vec};

    fn words(line: &str) -> Vec<String> {
        parse(line).unwrap()
    }

    #[test]
    fn whitespace_separates_words_and_quotes_group_them() {
        assert_eq!(words("touch  a   b"), vec!["touch", "a", "b"]);
        assert_eq!(words("touch \"a b.txt\""), vec!["touch", "a b.txt"]);
        assert_eq!(words("echo 'x  y'"), vec!["echo", "x  y"]);
        assert_eq!(words("echo \"\""), vec!["echo", ""]);
    }

    #[test]
    fn escapes_follow_shell_rules() {
        assert_eq!(words("touch a\\ b"), vec!["touch", "a b"]);
        assert_eq!(words("echo \"say \\\"hi\\\"\""), vec!["echo", "say \"hi\""]);
        assert_eq!(words("echo 'no \\escape'"), vec!["echo", "no \\escape"]);
        assert_eq!(words("echo \"keep \\n\""), vec!["echo", "keep \\n"]);
    }

    #[test]
    fn unicode_words_are_preserved() {
        assert_eq!(words("echo 你好 \"星 澜\""), vec!["echo", "你好", "星 澜"]);
    }

    #[test]
    fn malformed_input_is_an_error_not_a_guess() {
        assert_eq!(
            parse("echo \"open"),
            Err(ParseError::UnterminatedQuote('"'))
        );
        assert_eq!(
            parse("echo 'open"),
            Err(ParseError::UnterminatedQuote('\''))
        );
        assert_eq!(parse("echo end\\"), Err(ParseError::TrailingBackslash));
    }

    #[test]
    fn shell_metacharacters_are_refused_instead_of_executed() {
        for (line, character) in [
            ("echo hi > file", '>'),
            ("ls | cat", '|'),
            ("rm *", '*'),
            ("echo $HOME", '$'),
            ("a; b", ';'),
            ("sleep &", '&'),
        ] {
            assert_eq!(
                parse(line),
                Err(ParseError::UnsupportedSyntax(character)),
                "{line}"
            );
        }
        assert_eq!(words("echo '>' \"$x\""), vec!["echo", ">", "$x"]);
    }

    #[test]
    fn empty_lines_produce_no_words() {
        assert!(words("").is_empty());
        assert!(words("   \t ").is_empty());
    }
}
