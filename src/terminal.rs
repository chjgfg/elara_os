//! Safe Rust terminal adapter. No PowerShell, stty or other child commands.
//! Interactive secrets use rpassword's native terminal implementation. Errors
//! fail closed: input is never silently switched to visible terminal reading.
use std::io::{self, BufRead, Write};
use std::{format, write};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EchoMode {
    Hidden,
    Redirected,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SecretInput {
    Entered(String),
    EndOfInput,
}

pub trait SecretReader {
    fn read<R: BufRead>(
        &mut self,
        input: &mut R,
        interactive: bool,
    ) -> io::Result<(SecretInput, EchoMode)>;
}

pub struct NativeSecretReader;

impl SecretReader for NativeSecretReader {
    fn read<R: BufRead>(
        &mut self,
        input: &mut R,
        interactive: bool,
    ) -> io::Result<(SecretInput, EchoMode)> {
        if interactive {
            let secret = rpassword::read_password().map_err(|error| {
                io::Error::new(
                    error.kind(),
                    format!("Hidden password input unavailable; refusing visible input: {error}"),
                )
            })?;
            Ok((SecretInput::Entered(secret), EchoMode::Hidden))
        } else {
            read_redirected(input)
        }
    }
}

fn read_redirected<R: BufRead>(input: &mut R) -> io::Result<(SecretInput, EchoMode)> {
    let mut line = String::new();
    let secret = if input.read_line(&mut line)? == 0 {
        SecretInput::EndOfInput
    } else {
        SecretInput::Entered(line.trim_end_matches(['\r', '\n']).to_owned())
    };
    Ok((secret, EchoMode::Redirected))
}

pub fn read_secret<R: BufRead, W: Write, S: SecretReader>(
    input: &mut R,
    output: &mut W,
    reader: &mut S,
    interactive: bool,
    prompt: &str,
) -> io::Result<(SecretInput, EchoMode)> {
    write!(output, "{prompt}")?;
    output.flush()?;
    reader.read(input, interactive)
}

/// Deterministic tests must not touch a real console, even when checking prompts.
#[cfg(test)]
pub struct BufferedSecretReader;

#[cfg(test)]
impl SecretReader for BufferedSecretReader {
    fn read<R: BufRead>(
        &mut self,
        input: &mut R,
        _interactive: bool,
    ) -> io::Result<(SecretInput, EchoMode)> {
        read_redirected(input)
    }
}

#[cfg(test)]
mod tests {
    use super::{read_secret, BufferedSecretReader, EchoMode, SecretInput, SecretReader};
    use std::io::{self, BufRead, Cursor};
    use std::{assert, assert_eq};

    #[test]
    fn redirected_reads_strip_line_endings_without_consuming_the_next_line() {
        let mut input = Cursor::new(b"s3cret\r\nnext\n".as_slice());
        let mut output = Vec::new();
        let (secret, mode) = read_secret(
            &mut input,
            &mut output,
            &mut BufferedSecretReader,
            false,
            "Password: ",
        )
        .unwrap();
        assert_eq!(secret, SecretInput::Entered("s3cret".into()));
        assert_eq!(mode, EchoMode::Redirected);
        assert_eq!(String::from_utf8(output).unwrap(), "Password: ");
        let mut rest = String::new();
        input.read_line(&mut rest).unwrap();
        assert_eq!(rest, "next\n");
    }

    #[test]
    fn end_of_input_is_distinct_from_entering_an_empty_password() {
        let mut output = Vec::new();
        let (end, _) = read_secret(
            &mut Cursor::new(b""),
            &mut output,
            &mut BufferedSecretReader,
            false,
            "> ",
        )
        .unwrap();
        let (blank, _) = read_secret(
            &mut Cursor::new(b"\n"),
            &mut output,
            &mut BufferedSecretReader,
            false,
            "> ",
        )
        .unwrap();
        assert_eq!(end, SecretInput::EndOfInput);
        assert_eq!(blank, SecretInput::Entered(String::new()));
    }

    struct Unavailable;
    impl SecretReader for Unavailable {
        fn read<R: BufRead>(
            &mut self,
            _input: &mut R,
            _interactive: bool,
        ) -> io::Result<(SecretInput, EchoMode)> {
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "No hidden input available",
            ))
        }
    }

    #[test]
    fn unavailable_hidden_input_never_consumes_a_visible_password() {
        let mut input = Cursor::new(b"must-not-be-read\n");
        let mut output = Vec::new();
        assert!(read_secret(
            &mut input,
            &mut output,
            &mut Unavailable,
            true,
            "Password: "
        )
        .is_err());
        assert_eq!(input.position(), 0);
        assert!(!String::from_utf8(output)
            .unwrap()
            .contains("must-not-be-read"));
    }
}
