use crate::filesystem::DataRoot;
use crate::logging::{self, Logger};
use crate::permissions::{self, Access};
use crate::script;
use crate::terminal::{self, NativeSecretReader, SecretInput, SecretReader};
use crate::users;
use crate::virtual_path::VirtualPath;
use std::io::{self, BufRead, Write};
use std::{env, format, matches, write, writeln};

const CAT_LIMIT: u64 = 64 * 1024;
const TASK_CAPACITY: usize = 16;
/// ANSI: clear screen, clear scrollback, cursor home.
const CLEAR_SEQUENCE: &str = "\x1b[2J\x1b[3J\x1b[H";

pub fn run<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    interactive: bool,
    data_root: &DataRoot,
) -> io::Result<()> {
    run_status(input, output, interactive, data_root).map(|_| ())
}

/// Run the actual shell and return its final status, including `exit N`.
pub fn run_status<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    interactive: bool,
    data_root: &DataRoot,
) -> io::Result<u8> {
    run_with_secret_reader(
        input,
        output,
        interactive,
        data_root,
        &mut NativeSecretReader,
    )
}

fn run_with_secret_reader<R: BufRead, W: Write, S: SecretReader>(
    input: &mut R,
    output: &mut W,
    interactive: bool,
    data_root: &DataRoot,
    reader: &mut S,
) -> io::Result<u8> {
    std::thread::scope(|scope| run_in_scope(input, output, interactive, data_root, reader, scope))
}
fn run_in_scope<'scope, 'env, R: BufRead, W: Write, S: SecretReader>(
    input: &mut R,
    output: &mut W,
    interactive: bool,
    data_root: &'env DataRoot,
    reader: &mut S,
    scope: &'scope std::thread::Scope<'scope, 'env>,
) -> io::Result<u8> {
    writeln!(
        output,
        "ElaraOS host prototype v{}",
        env!("CARGO_PKG_VERSION")
    )?;
    writeln!(output, "Hosted mode: not a standalone kernel.")?;
    writeln!(output, "Data root: {}", data_root.root().display())?;
    let mut user = match users::ensure_passwd(data_root)
        .and_then(|()| users::load(data_root))
        .and_then(|accounts| users::lookup(&accounts, "root").cloned())
    {
        Ok(account) => account,
        Err(error) => {
            writeln!(output, "Startup refused: {error}")?;
            return output.flush().map(|_| 2);
        }
    };
    let logger = match Logger::open(data_root) {
        Ok(logger) => logger,
        Err(error) => {
            writeln!(
                output,
                "Startup refused: cannot open {}: {error}",
                logging::LOG_PATH
            )?;
            return output.flush().map(|_| 2);
        }
    };
    let logger = std::sync::Arc::new(logger);
    if let Err(error) = logger.record(data_root, &user.name, "startup") {
        writeln!(output, "Warning: log write failed: {error}")?;
    }
    match users::has_password(data_root, &user.name) {
        Ok(false) => {
            writeln!(output, "root has no password yet. Without one, regular users can never switch back to root.")?;
            writeln!(output, "Enter a new password twice, or press Enter once to skip. After startup you can also use 'passwd root'.")?;
            match prompt_new_password(input, output, reader, interactive, &user.name, true)? {
                PasswordPrompt::Skipped => {
                    writeln!(output, "Skipped; root still has no password.")?;
                }
                PasswordPrompt::Entered(password) => {
                    match users::set_password(data_root, &user, &user.name, &password) {
                        Ok(()) => writeln!(output, "Password set for root.")?,
                        Err(error) => writeln!(output, "Error: {error}")?,
                    }
                }
                PasswordPrompt::EndOfInput | PasswordPrompt::Mismatch => {}
            }
        }
        Ok(true) => {}
        Err(error) => writeln!(output, "Warning: cannot read /etc/shadow: {error}")?,
    }
    writeln!(output, "Type 'help' for commands.")?;
    let mut current = VirtualPath::root();
    let mut script_context = script::Context::new(current.clone());
    let resources = match script::shared::Resources::new(user.uid, logger.clone(), TASK_CAPACITY) {
        Ok(resources) => resources,
        Err(error) => {
            writeln!(output, "Startup refused: shared module resources: {error}")?;
            return output.flush().map(|_| 2);
        }
    };
    let jobs = script::jobs::JobTable::new(scope, user.uid, TASK_CAPACITY);
    let result = (|| -> io::Result<u8> {
        loop {
            if interactive {
                write!(output, "{}@elara:{current}> ", user.name)?;
                output.flush()?;
            }
            let line = match script::interactive::read_prompt(input)? {
                script::interactive::PromptInput::Program(line) => line,
                script::interactive::PromptInput::End => {
                    if interactive {
                        writeln!(output)?;
                    }
                    "exit".into()
                }
                script::interactive::PromptInput::Rejected(message) => {
                    script_context.record_status(2);
                    writeln!(output, "Error: {message}")?;
                    output.flush()?;
                    continue;
                }
            };
            let text = line.trim_matches([' ', '\t']);
            if text.is_empty() {
                continue;
            }
            let words = match script::interactive::prepare(text) {
                Ok(script::interactive::Line::Empty) => continue,
                Ok(script::interactive::Line::Program(ast)) => {
                    let label = script::interactive::audit_label(&ast);
                    if let Err(error) =
                        logger.record(data_root, &user.name, &format!("command: {label}"))
                    {
                        writeln!(output, "Warning: log write failed: {error}")?;
                    }
                    script_context.cwd = current.clone();
                    let mut modules =
                        script::shared::SharedModules::new(data_root, &user, &resources);
                    let report = script::runtime::execute_in_session(
                        &ast,
                        data_root,
                        &user,
                        &mut script_context,
                        &mut modules,
                        jobs.clone(),
                    );
                    current = script_context.cwd.clone();
                    output.write_all(&report.stdout)?;
                    output.write_all(&report.stderr)?;
                    output.flush()?;
                    if report.completion == script::runtime::Completion::ExitRequested {
                        writeln!(output, "Bye.")?;
                        break;
                    }
                    continue;
                }
                Ok(script::interactive::Line::Session { words, .. }) => {
                    script_context.cwd = current.clone();
                    let mut modules =
                        script::shared::SharedModules::new(data_root, &user, &resources);
                    match script::runtime::session_words(
                        data_root,
                        &user,
                        &mut script_context,
                        &words,
                        &mut modules,
                    ) {
                        Ok(words) => words,
                        Err(error) => {
                            script_context.record_status(2);
                            writeln!(output, "Error: {error}")?;
                            output.flush()?;
                            continue;
                        }
                    }
                }
                Err(error) => {
                    script_context.record_status(2);
                    writeln!(output, "Error: {error}")?;
                    output.flush()?;
                    continue;
                }
            };
            let Some((command, arguments)) = words.split_first() else {
                continue;
            };
            let command = command.as_str();
            let logged = if matches!(
                command,
                "su" | "passwd" | "useradd" | "sh" | "source" | "." | "fd"
            ) {
                format!(
                    "command: {command} {}",
                    arguments.first().map_or("", String::as_str)
                )
            } else {
                format!("command: {text}")
            };
            if let Err(error) = logger.record(data_root, &user.name, &logged) {
                writeln!(output, "Warning: log write failed: {error}")?;
            }
            // Set failure before prompting: cancellation/EOF cannot retain a stale success.
            script_context.record_status(1);
            match command {
                "help" if arguments.is_empty() => {
                    script_context.record_status(0);
                    writeln!(output, "help   - list commands")?;
                    writeln!(output, "info   - show prototype, host and data root")?;
                    writeln!(output, "pwd    - show virtual current directory")?;
                    writeln!(output, "cd     - change virtual directory (default $HOME)")?;
                    writeln!(
                        output,
                        "ls     - list a virtual directory (default current directory)"
                    )?;
                    writeln!(
                        output,
                        "cat    - copy exact bytes; final/buffered limit {CAT_LIMIT}, eligible pipes stream"
                    )?;
                    writeln!(
                        output,
                        "mkdir  - create directories inside existing parents (no -p)"
                    )?;
                    writeln!(
                        output,
                        "touch  - create empty files (existing files are refused)"
                    )?;
                    writeln!(
                        output,
                        "rm     - remove regular files (no recursive delete)"
                    )?;
                    writeln!(output, "rmdir  - remove empty directories")?;
                    writeln!(
                        output,
                        "mv     - atomically move a file/tree to a new name (no overwrite)"
                    )?;
                    writeln!(
                        output,
                        "cp     - atomically copy one file to a new name (limit 1 MiB)"
                    )?;
                    writeln!(
                        output,
                        "stat   - inspect host-backed metadata for one virtual path"
                    )?;
                    writeln!(
                        output,
                        "find   - list a bounded virtual tree (default current directory)"
                    )?;
                    writeln!(
                        output,
                        "du     - report logical bytes/counts; physical allocation is unknown"
                    )?;
                    writeln!(
                        output,
                        "echo   - display expanded arguments; -n omits newline"
                    )?;
                    writeln!(
                        output,
                        "printf - formats %s/%d/%% and repeats over arguments"
                    )?;
                    writeln!(output, "wc     - count bytes (-c), lines (-l), words (-w)")?;
                    writeln!(
                        output,
                        "test/[ - string/file/integer predicates; true/false/: status commands"
                    )?;
                    writeln!(
                        output,
                        "export/unset/local - variable declarations; local requires a function"
                    )?;
                    writeln!(output, "set    - enable -e/-u or disable +e/+u")?;
                    writeln!(output, "trap   - trap <commands|-> EXIT (not host signals)")?;
                    writeln!(
                        output,
                        "sleep  - sleep N seconds; legacy 'sleep N bg'; scoped 'sleep N &'"
                    )?;
                    writeln!(output, "ps     - list ElaraOS tasks (never host processes)")?;
                    writeln!(
                        output,
                        "wait   - wait ID for legacy tasks; wait %ID joins/reaps a scoped job"
                    )?;
                    writeln!(
                        output,
                        "jobs/fg/bg - scoped jobs; fg %ID resumes and waits, bg %ID resumes"
                    )?;
                    writeln!(output, "kill   - [-STOP|-CONT|-TERM|-INT] %ID; cooperative requests, no host signals")?;
                    writeln!(output, "whoami - show the current ElaraOS user")?;
                    writeln!(output, "users  - list accounts from /etc/passwd")?;
                    writeln!(
                    output,
                    "useradd - create an account: useradd <name> [password] (root only; prompts when omitted)"
                )?;
                    writeln!(
                        output,
                        "passwd - set a password: passwd [name] [password] (prompts when omitted)"
                    )?;
                    writeln!(
                    output,
                    "su     - switch account: su [name] [password] (default root; prompts when omitted)"
                )?;
                    writeln!(
                        output,
                        "log    - show the last N log entries from {} (default {})",
                        logging::LOG_PATH,
                        logging::DEFAULT_TAIL
                    )?;
                    writeln!(
                        output,
                        "sh     - run an ElaraOS script: sh [-n] <file> [args] | sh -c <text>"
                    )?;
                    writeln!(
                        output,
                        "source - run a script retaining its variables/functions/directory"
                    )?;
                    writeln!(
                        output,
                        "fd     - owner-bound file descriptors; use 'fd help'"
                    )?;
                    writeln!(output, "clear  - clear the terminal screen")?;
                    writeln!(
                        output,
                        "exit   - close this program with 0..255 status (default $?)"
                    )?;
                    writeln!(output, "syntax - variables, $(...), $((...)), defaults/trim/literal replacement, glob/~, if/for/while/case/functions")?;
                    writeln!(
                        output,
                        "pipes  - eligible builtins stream concurrently; other stages use bounded buffers"
                    )?;
                    writeln!(output, "&      - isolated scoped script worker; EOF stdin, output collected by wait/fg")?;
                }
                "info" if arguments.is_empty() => {
                    script_context.record_status(0);
                    writeln!(output, "version: {}", env!("CARGO_PKG_VERSION"))?;
                    writeln!(output, "mode: hosted prototype")?;
                    writeln!(output, "host: {} / {}", env::consts::OS, env::consts::ARCH)?;
                    writeln!(output, "data root: {}", data_root.root().display())?;
                    writeln!(output, "virtual directory: {current}")?;
                    writeln!(output, "user: {} (uid {})", user.name, user.uid)?;
                    writeln!(output, "host commands: disabled")?;
                    writeln!(
                        output,
                        "virtual filesystem: directories, listing, text files"
                    )?;
                }
                "useradd" => {
                    let name = match arguments {
                        [name] | [name, _] => name.as_str(),
                        _ => {
                            writeln!(
                            output,
                            "Usage: useradd <name> [password]  (omit the password to be prompted)"
                        )?;
                            output.flush()?;
                            continue;
                        }
                    };
                    if !user.is_admin() {
                        writeln!(output, "Error: {}", users::UserError::PermissionDenied)?;
                        output.flush()?;
                        continue;
                    }
                    let password = match arguments {
                        [_, password] => password.clone(),
                        _ => {
                            match prompt_new_password(
                                input,
                                output,
                                reader,
                                interactive,
                                name,
                                false,
                            )? {
                                PasswordPrompt::Entered(password) => password,
                                _ => continue,
                            }
                        }
                    };
                    match users::add_user(data_root, &user, name, &password) {
                        Ok(account) => {
                            script_context.record_status(0);
                            writeln!(
                                output,
                                "Created {} (uid {}) with home {}",
                                account.name, account.uid, account.home
                            )?;
                        }
                        Err(error) => writeln!(output, "Error: {error}")?,
                    }
                }
                "passwd" => {
                    let name = match arguments {
                        [] => user.name.as_str(),
                        [name] | [name, _] => name.as_str(),
                        _ => {
                            writeln!(
                            output,
                            "Usage: passwd [name] [password]  (omit the password to be prompted)"
                        )?;
                            output.flush()?;
                            continue;
                        }
                    };
                    if !user.is_admin() && user.name != name {
                        writeln!(output, "Error: {}", users::UserError::PermissionDenied)?;
                        output.flush()?;
                        continue;
                    }
                    let password = match arguments {
                        [_, password] => password.clone(),
                        _ => {
                            match prompt_new_password(
                                input,
                                output,
                                reader,
                                interactive,
                                name,
                                false,
                            )? {
                                PasswordPrompt::Entered(password) => password,
                                _ => continue,
                            }
                        }
                    };
                    match users::set_password(data_root, &user, name, &password) {
                        Ok(()) => {
                            script_context.record_status(0);
                            writeln!(output, "Password updated for {name}.")?;
                        }
                        Err(error) => writeln!(output, "Error: {error}")?,
                    }
                }
                "su" => {
                    let name = match arguments {
                        [] => "root",
                        [name] | [name, _] => name.as_str(),
                        _ => {
                            writeln!(
                                output,
                                "Usage: su [name] [password]  (omit the password to be prompted)"
                            )?;
                            output.flush()?;
                            continue;
                        }
                    };
                    if name == user.name {
                        script_context.record_status(0);
                        writeln!(output, "Already {name}; nothing to do.")?;
                        output.flush()?;
                        continue;
                    }
                    let result = if user.is_admin() {
                        users::load(data_root)
                            .and_then(|accounts| users::lookup(&accounts, name).cloned())
                    } else {
                        let password = match arguments {
                            [_, password] => password.clone(),
                            _ => {
                                let (secret, _) = terminal::read_secret(
                                    input,
                                    output,
                                    reader,
                                    interactive,
                                    &format!("Password for {name}: "),
                                )?;
                                match secret {
                                    SecretInput::Entered(secret) => secret,
                                    SecretInput::EndOfInput => {
                                        writeln!(output, "Password input ended; nothing changed.")?;
                                        output.flush()?;
                                        continue;
                                    }
                                }
                            }
                        };
                        users::authenticate(data_root, name, &password)
                    };
                    match result {
                        Ok(account) => {
                            let old_uid = user.uid;
                            let reaped = jobs.shutdown().map_err(std::io::Error::other)?;
                            let closed = match resources.switch_owner(account.uid) {
                                Ok(closed) => closed,
                                Err(error) => {
                                    jobs.complete_owner_switch(old_uid)
                                        .map_err(std::io::Error::other)?;
                                    script_context.record_status(1);
                                    writeln!(output, "Error: identity change refused: {error}")?;
                                    continue;
                                }
                            };
                            if reaped != 0 {
                                writeln!(
                                    output,
                                    "Joined {reaped} script jobs before identity change."
                                )?;
                            }
                            let summary = format!(
                                "fd owner uid {old_uid} -> {}; closed {closed}",
                                account.uid
                            );
                            if let Err(error) = logger.record(data_root, &user.name, &summary) {
                                writeln!(output, "Warning: log write failed: {error}")?;
                            }
                            if closed != 0 {
                                writeln!(
                                    output,
                                    "Closed {closed} file handles on identity change."
                                )?;
                            }
                            user = account;
                            if permissions::check(&user, &current, Access::Read).is_err() {
                                current = VirtualPath::root();
                            }
                            // Script variables/functions must not cross an account boundary.
                            script_context = script::Context::new(current.clone());
                            jobs.complete_owner_switch(user.uid)
                                .map_err(std::io::Error::other)?;
                        }
                        Err(error) => writeln!(output, "Error: {error}")?,
                    }
                }
                "clear" if arguments.is_empty() => {
                    script_context.record_status(0);
                    if interactive {
                        write!(output, "{CLEAR_SEQUENCE}")?;
                    } else {
                        writeln!(output, "[screen cleared]")?;
                    }
                }
                "help" | "info" | "clear" => {
                    script_context.record_status(2);
                    writeln!(output, "'{command}' takes no arguments.")?;
                }
                _ => {
                    script_context.record_status(127);
                    writeln!(output, "Unknown command: {command}. Type 'help'.")?;
                }
            }
            output.flush()?;
        }
        output.flush()?;
        Ok(script_context.status())
    })();
    jobs.shutdown().map_err(std::io::Error::other)?;
    result
}

#[derive(Debug, PartialEq, Eq)]
enum PasswordPrompt {
    Entered(String),
    Skipped,
    EndOfInput,
    Mismatch,
}

/// Root setup may be skipped with one empty line. Other password changes
/// require confirmation. EOF and mismatch are explicit cancellation outcomes.
fn prompt_new_password<R: BufRead, W: Write, S: SecretReader>(
    input: &mut R,
    output: &mut W,
    reader: &mut S,
    interactive: bool,
    name: &str,
    allow_skip: bool,
) -> io::Result<PasswordPrompt> {
    let (first, _) = terminal::read_secret(
        input,
        output,
        reader,
        interactive,
        &format!("New password for {name}: "),
    )?;
    let first = match first {
        SecretInput::Entered(first) => first,
        SecretInput::EndOfInput => {
            writeln!(output, "Password input ended; nothing changed.")?;
            return Ok(PasswordPrompt::EndOfInput);
        }
    };
    if allow_skip && first.is_empty() {
        return Ok(PasswordPrompt::Skipped);
    }
    let (second, _) =
        terminal::read_secret(input, output, reader, interactive, "Retype new password: ")?;
    let second = match second {
        SecretInput::Entered(second) => second,
        SecretInput::EndOfInput => {
            writeln!(output, "Password input ended; nothing changed.")?;
            return Ok(PasswordPrompt::EndOfInput);
        }
    };
    if first != second {
        writeln!(
            output,
            "Error: the two passwords do not match; nothing changed."
        )?;
        return Ok(PasswordPrompt::Mismatch);
    }
    Ok(PasswordPrompt::Entered(first))
}

#[cfg(test)]
mod tests {
    use super::run_with_secret_reader;
    use crate::filesystem::DataRoot;
    use crate::terminal::BufferedSecretReader;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::{assert, assert_eq, env, fs, vec};

    fn fixture() -> (PathBuf, DataRoot) {
        let root = crate::test_support::marked_root("shell");
        let data = DataRoot::open(&root).unwrap();
        (root, data)
    }

    /// Run with the startup root-password prompt skipped (one empty line).
    fn transcript_in(root: &DataRoot, commands: &str, interactive: bool) -> String {
        transcript_raw(root, &format!("\n{commands}"), interactive)
    }

    /// Run with the exact input, including whatever the startup prompt consumes.
    fn transcript_raw(root: &DataRoot, commands: &str, interactive: bool) -> String {
        let mut input = Cursor::new(commands.as_bytes());
        let mut output = Vec::new();
        run_with_secret_reader(
            &mut input,
            &mut output,
            interactive,
            root,
            &mut BufferedSecretReader,
        )
        .unwrap();
        String::from_utf8(output).unwrap()
    }

    fn transcript(commands: &str, interactive: bool) -> String {
        let (_, root) = fixture();
        transcript_in(&root, commands, interactive)
    }

    #[test]
    fn startup_offers_a_root_password_once_and_skipping_is_remembered_as_missing() {
        let (root, data) = fixture();
        let text = transcript_raw(&data, "\nwhoami\nexit\n", false);
        assert!(text.contains("root has no password yet."));
        assert!(text.contains("Skipped; root still has no password."));
        assert!(
            !text.contains("Retype new password:"),
            "one Enter must skip without consuming whoami"
        );
        assert!(text.ends_with("root\nBye.\n"));
        assert!(!fs::read_to_string(root.join("etc/shadow"))
            .unwrap()
            .contains("root:"));
        let text = transcript_raw(&data, "rootpw1\nrootpw1\nwhoami\nexit\n", false);
        assert!(text.contains("Password set for root."));
        assert!(fs::read_to_string(root.join("etc/shadow"))
            .unwrap()
            .starts_with("root:"));
        let text = transcript_raw(&data, "whoami\nexit\n", false);
        assert!(
            !text.contains("root has no password yet."),
            "prompt must not repeat once set"
        );
        assert!(text.contains("root\n"));
    }

    #[test]
    fn mismatched_startup_passwords_change_nothing() {
        let (root, data) = fixture();
        let text = transcript_raw(&data, "one1\ntwo2\nexit\n", false);
        assert!(text.contains("the two passwords do not match; nothing changed."));
        assert!(!fs::read_to_string(root.join("etc/shadow"))
            .unwrap()
            .contains("root:"));
    }

    #[test]
    fn interactive_password_prompts_consume_their_own_lines() {
        let (root, data) = fixture();
        let text = transcript_in(
            &data,
            "useradd alice\nsecret1\nsecret1\nsu alice\nsu alice\npasswd\nnew-pass1\nnew-pass1\nsu root\nwrong\nsu root bad\nsu root\n\nwhoami\nexit\n",
            false,
        );
        assert!(text.contains("New password for alice: Retype new password: Created alice"));
        assert!(text.contains("Already alice; nothing to do."));
        assert!(text.contains("Password updated for alice."));
        assert!(text.contains("Password for root: Error: Authentication failed."));
        assert!(text.matches("Authentication failed.").count() == 3);
        assert!(text.ends_with("alice\nBye.\n"));
        let shadow = fs::read_to_string(root.join("etc/shadow")).unwrap();
        assert!(!shadow.contains("secret1") && !shadow.contains("new-pass1"));
    }

    #[test]
    fn eof_during_password_confirmation_does_not_set_a_password() {
        let (root, data) = fixture();
        let text = transcript_raw(&data, "rootpw1\n", false);
        assert!(text.contains("Password input ended; nothing changed."));
        assert!(!fs::read_to_string(root.join("etc/shadow"))
            .unwrap()
            .contains("root:"));
    }

    #[test]
    fn denied_password_operations_do_not_consume_following_commands() {
        let (root, data) = fixture();
        let text = transcript_in(
            &data,
            "useradd alice secret1\nsu alice\nuseradd bob\nwhoami\npasswd root\nwhoami\nexit\n",
            false,
        );
        assert_eq!(text.lines().filter(|line| *line == "alice").count(), 2);
        assert!(!text.contains("New password for bob:"));
        assert_eq!(
            text.matches("New password for root:").count(),
            1,
            "only startup may prompt, not the denied passwd command"
        );
        assert!(!root.join("home/bob").exists());
    }

    #[test]
    fn directory_changes_and_relative_listing_are_real() {
        let text = transcript("cd /usr\npwd\ncd bin\npwd\ncd ..\npwd\nls\nexit\n", false);
        assert!(text.contains("/usr\n/usr/bin\n/usr\n"));
        assert!(text.contains("bin/\n"));
        assert!(text.contains("lib/\n"));
        assert!(text.ends_with("Bye.\n"));
    }

    #[test]
    fn failed_directory_changes_preserve_previous_state() {
        let text = transcript(
            "cd /usr\ncd ../../\npwd\ncd C:/Windows\npwd\ncd missing-directory\npwd\nexit\n",
            false,
        );
        assert!(text.contains("Cannot move above the virtual root."));
        assert!(text.contains("Host path syntax and control characters are refused."));
        assert_eq!(text.lines().filter(|line| *line == "/usr").count(), 3);
    }

    #[test]
    fn virtual_navigation_never_changes_the_host_working_directory() {
        let before = env::current_dir().unwrap();
        let text = transcript("cd /usr\npwd\nexit\n", true);
        assert_eq!(env::current_dir().unwrap(), before);
        assert!(text.contains("elara:/> "));
        assert!(text.contains("elara:/usr> "));
    }

    #[test]
    fn file_commands_operate_relative_to_the_virtual_directory() {
        let (root, data) = fixture();
        let text = transcript_in(
            &data,
            "cd /home\nmkdir elara\ncd elara\ntouch notes.txt\nls\ncat notes.txt\nrm notes.txt\nls\nexit\n",
            false,
        );
        assert!(text.contains("notes.txt\n"));
        assert!(!root.join("home/elara/notes.txt").exists());
        assert!(root.join("home/elara").is_dir());
        assert!(text.ends_with("Bye.\n"));
    }

    #[test]
    fn cat_prints_file_contents_and_reports_limits() {
        let (root, data) = fixture();
        fs::write(root.join("home/hello.txt"), "你好，星澜").unwrap();
        fs::write(root.join("home/big.txt"), vec![b'x'; 64 * 1024 + 1]).unwrap();
        let text = transcript_in(
            &data,
            "cat /home/hello.txt\necho END\ncat /home/hello.txt | wc -c\ncat /home/big.txt\nexit\n",
            false,
        );
        // Unified command semantics: no UI-only LF may enter cat's byte stream.
        assert!(text.contains("你好，星澜END\n15\n"), "{text}");
        assert!(text.contains("exceeds the 65536-byte display limit"));
    }

    #[test]
    fn file_commands_refuse_escapes_and_protected_paths_but_accept_multiple_paths() {
        let (root, data) = fixture();
        let text = transcript_in(
            &data,
            "rm /\nrm ..\nrm /usr\ntouch C:/Windows/x.txt\ntouch ../escape.txt\nmkdir /home/a /home/b\nmkdir\ntouch\nexit\n",
            false,
        );
        assert!(text.contains("cannot be removed or replaced"));
        assert!(text.contains("Cannot move above the virtual root."));
        assert!(text.contains("Host path syntax and control characters are refused."));
        assert!(root.join("home/a").is_dir());
        assert!(root.join("home/b").is_dir());
        assert!(text.contains("Usage: mkdir <path>"));
        assert!(text.contains("Usage: touch <path>"));
        assert!(root.join("usr").is_dir());
        assert!(!root.parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn quoted_arguments_pipelines_and_redirection_use_the_shared_script_frontend() {
        let (root, data) = fixture();
        let text = transcript_in(
            &data,
            "cd /home\ntouch \"a b.txt\"\ntouch c\\ d.txt\nls\necho \"x  y\" z\necho hi > out.txt\nls | cat\ntouch /tmp/forbidden; echo ${X@unknown}\necho \"open\nexit\n",
            false,
        );
        assert!(root.join("home/a b.txt").is_file());
        assert!(root.join("home/c d.txt").is_file());
        assert!(text.contains("a b.txt\nc d.txt\n"));
        assert!(text.contains("x  y z\n"));
        assert_eq!(fs::read(root.join("home/out.txt")).unwrap(), b"hi\n");
        assert!(text.contains("invalid or unsupported parameter expansion"));
        assert!(!root.join("tmp/forbidden").exists());
        assert!(text.contains("unclosed quote"));
        assert!(
            root.join("home/a b.txt").is_file(),
            "invalid syntax must not modify existing files"
        );
        assert!(text.ends_with("Bye.\n"));
    }

    #[test]
    fn tasks_are_listed_waited_and_never_host_processes() {
        let text = transcript(
            "sleep 0\nps\nsleep 1 bg\nps\nwait 2\nps\nwait 99\nsleep abc\nsleep 1 fg\nexit\n",
            false,
        );
        assert!(text.contains("[1] exited 0\n"));
        assert!(text.contains("[2] started: sleep 1\n"));
        assert!(text.contains("ID    STATE      COMMAND\n"));
        assert!(text.contains("2     running    sleep 1\n"));
        assert!(text.contains("[2] exited 0\n"));
        assert!(text.contains("2     exited 0   sleep 1\n"));
        assert!(text.contains("No task with id 99."));
        assert!(text.contains("'abc' is not a duration"));
        assert!(text.contains("Usage: sleep <seconds> [bg]"));
        assert!(
            !text.contains("elara-host"),
            "ps must not list the host process"
        );
    }

    #[test]
    fn user_commands_manage_accounts_only_inside_the_virtual_root() {
        let (root, data) = fixture();
        let text = transcript_in(
            &data,
            "whoami\nusers\nuseradd alice secret1\nuseradd Alice secret1\nuseradd\nsu alice\nwhoami\nuseradd bob secret2\nsu root\n\nsu root wrong\nsu alice secret1\nsu nobody x\nusers\nexit\n",
            true,
        );
        assert!(text.contains("root@elara:/> "));
        assert!(text.contains("Created alice (uid 1000) with home /home/alice"));
        assert!(text.contains("is not a valid user name"));
        assert!(text.contains("Usage: useradd <name> [password]"));
        assert!(text.contains("alice@elara:/> "));
        assert!(text.contains("Only root can manage users."));
        assert!(text.contains("Authentication failed."));
        assert!(text.contains("Already alice; nothing to do."));
        assert!(text.contains("Unknown user 'nobody'."));
        assert!(text.contains("alice            uid 1000   /home/alice\n"));
        assert!(root.join("etc/passwd").is_file());
        assert!(root.join("etc/shadow").is_file());
        assert!(!fs::read_to_string(root.join("etc/shadow"))
            .unwrap()
            .contains("secret1"));
        assert!(root.join("home/alice").is_dir());
        assert!(!root.join("home/bob").exists());
    }

    #[test]
    fn regular_users_are_confined_by_the_permission_model() {
        let (root, data) = fixture();
        fs::write(root.join("root/secret.txt"), "top secret").unwrap();
        let text = transcript_in(
            &data,
            "cd /root\nuseradd alice secret1\nsu alice\npwd\ncd /root\ncat /root/secret.txt\nls /root\nlog\ntouch /etc/evil\nmkdir /usr/evil\ncd /home/alice\ntouch mine.txt\nls\ntouch /tmp/scratch.txt\ncat /etc/passwd\npasswd root hijack\npasswd alice newpass1\nsu root newpass1\nsu bob newpass1\nwhoami\nexit\n",
            true,
        );
        assert!(
            text.contains("alice@elara:/> "),
            "su must move a user out of /root"
        );
        assert!(text.contains("Permission denied: cannot read /root."));
        assert!(text.contains("Permission denied: cannot read /root/secret.txt."));
        assert!(text.contains("Permission denied: cannot read /var/log/elara.log."));
        assert!(text.contains("Permission denied: cannot write /etc/evil."));
        assert!(text.contains("Permission denied: cannot write /usr/evil."));
        assert!(text.contains("mine.txt\n"));
        assert!(text.contains("root:0:/root\n"));
        assert!(text.contains("Only root can manage users."));
        assert!(text.contains("Password updated for alice."));
        assert!(text.contains("Authentication failed."));
        assert!(!text.contains("top secret"));
        assert!(root.join("home/alice/mine.txt").is_file());
        assert!(root.join("tmp/scratch.txt").is_file());
        assert!(!root.join("etc/evil").exists());
        assert!(!root.join("usr/evil").exists());
        assert!(text.ends_with("Bye.\n"));
    }

    #[test]
    fn rmdir_and_rm_report_precise_reasons() {
        let (root, data) = fixture();
        let text = transcript_in(
            &data,
            "cd /home\nmkdir 1.txt\nrm 1.txt\nrmdir 1.txt\nls\nmkdir full\ntouch full/a.txt\nrmdir full\nrm missing\nrmdir /usr\nclear\nexit\n",
            false,
        );
        assert!(text.contains("That path is a directory; use rmdir for empty directories."));
        assert!(text.contains("The directory is not empty."));
        assert!(text.contains("No such file or directory."));
        assert!(text.contains("cannot be removed or replaced"));
        assert!(text.contains("[screen cleared]"));
        assert!(!root.join("home/1.txt").exists());
        assert!(root.join("home/full/a.txt").is_file());
        assert!(root.join("usr").is_dir());
    }

    #[test]
    fn interactive_clear_emits_a_terminal_control_sequence() {
        let text = transcript("clear\nexit\n", true);
        assert!(text.contains("\x1b[2J\x1b[3J\x1b[H"));
    }

    #[test]
    fn startup_and_commands_are_logged_without_passwords() {
        let (root, data) = fixture();
        let text = transcript_in(
            &data,
            "echo one\nls /usr\nlog 0\nuseradd alice secret1\nsu alice secret1\nsu root wrong\nlog 2\nexit\n",
            false,
        );
        assert!(text.contains("Usage: log [1-1000]"));
        assert!(text.contains("Permission denied: cannot read /var/log/elara.log."));
        assert!(text.contains("Authentication failed."));
        let file = fs::read_to_string(root.join("var/log/elara.log")).unwrap();
        assert!(
            !file.contains("secret1"),
            "passwords must never reach the log"
        );
        assert!(file.contains(" root command: useradd alice\n"));
        assert!(file.contains(" root command: su alice\n"));
        let lines: Vec<&str> = file.lines().collect();
        assert!(lines[0].ends_with(" root startup"));
        // Log command identity, not expandable argument values or source text.
        assert!(lines[1].ends_with(" root command: echo"));
        assert!(!file.contains("echo one"));
        assert!(lines.last().unwrap().ends_with(" alice command: exit"));
        assert!(!root.parent().unwrap().join("elara.log").exists());
    }
}
