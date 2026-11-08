//! Script entry point; filesystem and other system modules are used through their interfaces.
pub mod arithmetic;
pub mod control;
mod count;
mod expansion;
pub mod fd;
mod fs_commands;
mod globbing;
pub mod interactive;
pub mod jobs;
pub mod lexer;
pub mod modules;
mod owned;
pub mod parser;
pub mod pattern;
pub mod pipe;
pub mod runtime;
pub mod shared;
pub mod streaming;
use crate::filesystem::DataRoot;
use crate::permissions::{self, Access};
use crate::users::Account;
pub use runtime::Context;
use std::format;

pub(super) struct Prepared {
    pub ast: parser::List,
    pub parameters: Vec<String>,
    pub check: bool,
    pub explicit_parameters: bool,
}

/// Parse and authorize the complete child script before executing it. The runtime
/// reuses its engine for nested calls; this entrypoint is only for API callers.
pub fn run_with_modules(
    root: &DataRoot,
    user: &Account,
    context: &mut Context,
    args: &[String],
    modules: &mut dyn modules::ModuleCommands,
) -> Result<runtime::Report, lexer::Error> {
    let prepared = prepare(root, user, context, args)?;
    if prepared.check {
        return Ok(runtime::Report {
            completion: runtime::Completion::Finished,
            pipelines: Vec::new(),
            status: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
            steps: 0,
            mode: runtime::ExecutionMode::InProcessBuffered {
                byte_limit: runtime::MAX_OUTPUT,
            },
        });
    }
    Ok(runtime::execute(
        &prepared.ast,
        root,
        user,
        context,
        &prepared.parameters,
        modules,
    ))
}

pub(super) fn prepare(
    root: &DataRoot,
    user: &Account,
    context: &Context,
    args: &[String],
) -> Result<Prepared, lexer::Error> {
    if args.len() > 1024 || args.iter().map(String::len).sum::<usize>() > lexer::MAX_BYTES {
        return Err(lexer::Error::new(0, "script argument limit exceeded"));
    }
    let (source, parameters, check, explicit_parameters) = match args {
        [option, source, rest @ ..] if option == "-c" => {
            let mut parameters = rest.to_vec();
            if parameters.is_empty() {
                parameters.push("sh".into());
            }
            (source.clone(), parameters, false, !rest.is_empty())
        }
        [option, path, rest @ ..] if option == "-n" => (
            read(root, user, context, path)?,
            parameters(path, rest),
            true,
            !rest.is_empty(),
        ),
        [path, rest @ ..] if !path.starts_with('-') => (
            read(root, user, context, path)?,
            parameters(path, rest),
            false,
            !rest.is_empty(),
        ),
        _ => {
            return Err(lexer::Error::new(
                0,
                "Usage: sh [-n] <script> [args...] | sh -c <text> [name [args...]]",
            ))
        }
    };
    Ok(Prepared {
        ast: parser::parse(&source)?,
        parameters,
        check,
        explicit_parameters,
    })
}
fn parameters(path: &str, args: &[String]) -> Vec<String> {
    let mut parameters = Vec::new();
    parameters.push(path.into());
    parameters.extend_from_slice(args);
    parameters
}
fn read(
    root: &DataRoot,
    user: &Account,
    context: &Context,
    path: &str,
) -> Result<String, lexer::Error> {
    let path = context
        .cwd
        .resolve(path)
        .map_err(|e| lexer::Error::new(0, e.to_string()))?;
    permissions::check(user, &path, Access::Read)
        .map_err(|e| lexer::Error::new(0, e.to_string()))?;
    let bytes = root
        .read_file(&path.to_string(), lexer::MAX_BYTES as u64)
        .map_err(|e| lexer::Error::new(0, e.to_string()))?;
    if bytes.len() > lexer::MAX_BYTES {
        return Err(lexer::Error::new(0, "script exceeds 64 KiB"));
    }
    String::from_utf8(bytes).map_err(|e| lexer::Error::new(0, format!("script must be UTF-8: {e}")))
}
