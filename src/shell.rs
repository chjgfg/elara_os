use crate::command_line;
use crate::filesystem::{DataRoot, EntryKind, FsError};
use crate::tasks::{self, ExitStatus, TaskTable};
use crate::users;
use crate::virtual_path::{PathError, VirtualPath};
use std::io::{self, BufRead, Write};
use std::{env, write, writeln};

const CAT_LIMIT: u64 = 64 * 1024;
const TASK_CAPACITY: usize = 16;

pub fn run<R: BufRead, W: Write>(
    input: &mut R,
    output: &mut W,
    interactive: bool,
    data_root: &DataRoot,
) -> io::Result<()> {
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
            return output.flush();
        }
    };
    writeln!(output, "Type 'help' for commands.")?;
    let mut current = VirtualPath::root();
    let mut table = TaskTable::new(TASK_CAPACITY);
    let mut line = String::new();
    loop {
        if interactive {
            write!(output, "{}@elara:{current}> ", user.name)?;
            output.flush()?;
        }
        line.clear();
        if input.read_line(&mut line)? == 0 {
            if interactive {
                writeln!(output)?;
            }
            break;
        }
        let text = line.trim();
        if text.is_empty() {
            continue;
        }
        let words = match command_line::parse(text) {
            Ok(words) => words,
            Err(error) => {
                writeln!(output, "Error: {error}")?;
                output.flush()?;
                continue;
            }
        };
        let Some((command, arguments)) = words.split_first() else {
            continue;
        };
        let command = command.as_str();
        match command {
            "help" if arguments.is_empty() => {
                writeln!(output, "help   - list commands")?;
                writeln!(output, "info   - show prototype, host and data root")?;
                writeln!(output, "pwd    - show virtual current directory")?;
                writeln!(output, "cd     - change virtual directory (default /)")?;
                writeln!(
                    output,
                    "ls     - list a virtual directory (default current directory)"
                )?;
                writeln!(
                    output,
                    "cat    - print a text file (limit {CAT_LIMIT} bytes)"
                )?;
                writeln!(
                    output,
                    "mkdir  - create one directory inside an existing directory"
                )?;
                writeln!(output, "touch  - create one empty file")?;
                writeln!(output, "rm     - remove one regular file")?;
                writeln!(output, "echo   - display text")?;
                writeln!(
                    output,
                    "sleep  - sleep N seconds; 'sleep N bg' runs as a background task"
                )?;
                writeln!(output, "ps     - list ElaraOS tasks (never host processes)")?;
                writeln!(
                    output,
                    "wait   - wait for a task id and show its exit status"
                )?;
                writeln!(output, "whoami - show the current ElaraOS user")?;
                writeln!(output, "users  - list accounts from /etc/passwd")?;
                writeln!(output, "useradd - create an account (root only)")?;
                writeln!(output, "su     - switch to another ElaraOS account (default root)")?;
                writeln!(output, "clear  - scroll to a blank screen")?;
                writeln!(output, "exit   - close this program")?;
            }
            "info" if arguments.is_empty() => {
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
            "pwd" if arguments.is_empty() => writeln!(output, "{current}")?,
            "cd" => {
                let requested = match arguments {
                    [] => "/",
                    [path] => path.as_str(),
                    _ => {
                        writeln!(output, "Usage: cd [directory]")?;
                        output.flush()?;
                        continue;
                    }
                };
                match current.resolve(requested) {
                    Ok(candidate) => match data_root.validate_directory(&candidate.to_string()) {
                        Ok(()) => current = candidate,
                        Err(error) => writeln!(output, "Error: {error}")?,
                    },
                    Err(error) => writeln!(output, "Error: {error}")?,
                }
            }
            "ls" => {
                let requested = match arguments {
                    [] => "",
                    [path] => path.as_str(),
                    _ => {
                        writeln!(output, "Usage: ls [directory]")?;
                        output.flush()?;
                        continue;
                    }
                };
                match current.resolve(requested) {
                    Ok(path) => match data_root.list(&path.to_string()) {
                        Ok(entries) => {
                            for entry in entries {
                                let suffix = match entry.kind {
                                    EntryKind::Directory => "/",
                                    EntryKind::File => "",
                                    EntryKind::BlockedLink => " [link blocked]",
                                };
                                writeln!(output, "{}{suffix}", entry.name.to_string_lossy())?;
                            }
                        }
                        Err(error) => writeln!(output, "Error: {error}")?,
                    },
                    Err(error) => writeln!(output, "Error: {error}")?,
                }
            }
            "cat" | "mkdir" | "touch" | "rm" => {
                let [path] = arguments else {
                    writeln!(
                        output,
                        "Usage: {command} <path>  (quote names that contain spaces)"
                    )?;
                    output.flush()?;
                    continue;
                };
                let result = current
                    .resolve(path)
                    .map_err(FileCommandError::Path)
                    .and_then(|path| {
                        let path = path.to_string();
                        match command {
                            "cat" => data_root.read_file(&path, CAT_LIMIT).map(Some),
                            "mkdir" => data_root.create_directory(&path).map(|_| None),
                            "touch" => data_root.create_file(&path).map(|_| None),
                            _ => data_root.remove_file(&path).map(|_| None),
                        }
                        .map_err(FileCommandError::Fs)
                    });
                match result {
                    Ok(Some(bytes)) => {
                        output.write_all(&bytes)?;
                        if !bytes.ends_with(b"\n") {
                            writeln!(output)?;
                        }
                    }
                    Ok(None) => {}
                    Err(FileCommandError::Path(error)) => writeln!(output, "Error: {error}")?,
                    Err(FileCommandError::Fs(error)) => writeln!(output, "Error: {error}")?,
                }
            }
            "echo" => writeln!(output, "{}", arguments.join(" "))?,
            "whoami" if arguments.is_empty() => writeln!(output, "{}", user.name)?,
            "users" if arguments.is_empty() => match users::load(data_root) {
                Ok(accounts) => {
                    for account in accounts {
                        writeln!(output, "{:<16} uid {:<6} {}", account.name, account.uid, account.home)?;
                    }
                }
                Err(error) => writeln!(output, "Error: {error}")?,
            },
            "useradd" => {
                let [name] = arguments else {
                    writeln!(output, "Usage: useradd <name>")?;
                    output.flush()?;
                    continue;
                };
                match users::add_user(data_root, &user, name) {
                    Ok(account) => writeln!(output, "Created {} (uid {}) with home {}", account.name, account.uid, account.home)?,
                    Err(error) => writeln!(output, "Error: {error}")?,
                }
            }
            "su" => {
                let name = match arguments {
                    [] => "root",
                    [name] => name.as_str(),
                    _ => {
                        writeln!(output, "Usage: su [name]")?;
                        output.flush()?;
                        continue;
                    }
                };
                match users::load(data_root).and_then(|accounts| users::lookup(&accounts, name).cloned()) {
                    Ok(account) => user = account,
                    Err(error) => writeln!(output, "Error: {error}")?,
                }
            }
            "sleep" => {
                let (seconds, background) = match arguments {
                    [seconds] => (seconds.as_str(), false),
                    [seconds, mode] if mode == "bg" => (seconds.as_str(), true),
                    _ => {
                        writeln!(output, "Usage: sleep <seconds> [bg]")?;
                        output.flush()?;
                        continue;
                    }
                };
                match tasks::parse_seconds(seconds) {
                    Ok(duration) if background => match table.spawn_sleep(duration) {
                        Ok(id) => writeln!(output, "[{id}] started: sleep {seconds}")?,
                        Err(error) => writeln!(output, "Error: {error}")?,
                    },
                    Ok(duration) => {
                        std::thread::sleep(duration);
                        let id =
                            table.record_finished(&format!("sleep {seconds}"), ExitStatus::Success);
                        writeln!(output, "[{id}] exited 0")?;
                    }
                    Err(error) => writeln!(output, "Error: {error}")?,
                }
            }
            "ps" if arguments.is_empty() => {
                writeln!(output, "ID    STATE      COMMAND")?;
                for task in table.snapshot() {
                    writeln!(
                        output,
                        "{:<5} {:<10} {}",
                        task.id,
                        task.state.to_string(),
                        task.command
                    )?;
                }
            }
            "wait" => {
                let [id] = arguments else {
                    writeln!(output, "Usage: wait <task-id>")?;
                    output.flush()?;
                    continue;
                };
                match id.parse::<tasks::TaskId>() {
                    Ok(id) => match table.wait(id) {
                        Ok(ExitStatus::Success) => writeln!(output, "[{id}] exited 0")?,
                        Ok(ExitStatus::Failure(code)) => writeln!(output, "[{id}] exited {code}")?,
                        Err(error) => writeln!(output, "Error: {error}")?,
                    },
                    Err(_) => writeln!(output, "Error: '{id}' is not a task id.")?,
                }
            }
            "clear" if arguments.is_empty() => {
                for _ in 0..40 {
                    writeln!(output)?;
                }
            }
            "exit" if arguments.is_empty() => {
                writeln!(output, "Bye.")?;
                break;
            }
            "help" | "info" | "pwd" | "ps" | "whoami" | "users" | "clear" | "exit" => {
                writeln!(output, "'{command}' takes no arguments.")?;
            }
            _ => writeln!(output, "Unknown command: {command}. Type 'help'.")?,
        }
        output.flush()?;
    }
    output.flush()
}

enum FileCommandError {
    Path(PathError),
    Fs(FsError),
}

#[cfg(test)]
mod tests {
    use super::run;
    use crate::filesystem::DataRoot;
    use std::io::Cursor;
    use std::path::PathBuf;
    use std::{assert, assert_eq, env, fs};

    fn fixture() -> (PathBuf, DataRoot) {
        let root = crate::test_support::marked_root("shell");
        let data = DataRoot::open(&root).unwrap();
        (root, data)
    }

    fn transcript_in(root: &DataRoot, commands: &str, interactive: bool) -> String {
        let mut input = Cursor::new(commands.as_bytes());
        let mut output = Vec::new();
        run(&mut input, &mut output, interactive, root).unwrap();
        String::from_utf8(output).unwrap()
    }

    fn transcript(commands: &str, interactive: bool) -> String {
        let (_, root) = fixture();
        transcript_in(&root, commands, interactive)
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
            "cat /home/hello.txt\ncat /home/big.txt\nexit\n",
            false,
        );
        assert!(text.contains("你好，星澜\n"));
        assert!(text.contains("exceeds the 65536-byte display limit"));
    }

    #[test]
    fn file_commands_refuse_escapes_protected_paths_and_multiple_arguments() {
        let (root, data) = fixture();
        let text = transcript_in(
            &data,
            "rm /\nrm ..\nrm /usr\ntouch C:/Windows/x.txt\ntouch ../escape.txt\nmkdir a b\ntouch\nexit\n",
            false,
        );
        assert!(text.contains("cannot be removed or replaced"));
        assert!(text.contains("Cannot move above the virtual root."));
        assert!(text.contains("Host path syntax and control characters are refused."));
        assert!(text.contains("Usage: mkdir <path>"));
        assert!(text.contains("Usage: touch <path>"));
        assert!(root.join("usr").is_dir());
        assert!(!root.parent().unwrap().join("escape.txt").exists());
    }

    #[test]
    fn quoted_arguments_allow_spaces_and_metacharacters_are_refused() {
        let (root, data) = fixture();
        let text = transcript_in(
            &data,
            "cd /home\ntouch \"a b.txt\"\ntouch c\\ d.txt\nls\necho \"x  y\" z\necho hi > out.txt\nls | cat\nrm *\necho \"open\nexit\n",
            false,
        );
        assert!(root.join("home/a b.txt").is_file());
        assert!(root.join("home/c d.txt").is_file());
        assert!(text.contains("a b.txt\nc d.txt\n"));
        assert!(text.contains("x  y z\n"));
        assert!(text.contains("'>' is not supported"));
        assert!(text.contains("'|' is not supported"));
        assert!(text.contains("'*' is not supported"));
        assert!(text.contains("Unterminated \" quote."));
        assert!(!root.join("home/out.txt").exists());
        assert!(
            root.join("home/a b.txt").is_file(),
            "rm * must not delete anything"
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
}
