//! Stable module-command boundary. The interpreter does not implement tasks/users/logging.
use super::runtime::MAX_OUTPUT;
use crate::filesystem::DataRoot;
use crate::logging::{self, Logger};
use crate::tasks::{self, ExitStatus, TaskTable};
use crate::users::{self, Account};
use crate::virtual_path::VirtualPath;
use std::io::Write;
use std::{format, writeln};

pub struct Request<'a> {
    pub name: &'a str,
    pub args: &'a [String],
    pub stdin: &'a [u8],
    pub cwd: &'a VirtualPath,
}
pub enum InputUse {
    Untouched,
    Consumed(usize),
}
pub struct CommandOutput {
    pub status: u8,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub input_use: InputUse,
}
pub enum Dispatch {
    Handled(CommandOutput),
    Unsupported,
}
pub enum ModuleFork<'a> {
    Ready(Box<dyn ModuleCommands + Send + 'a>),
    Unsupported,
}
pub trait ModuleCommands {
    fn execute(&mut self, request: Request<'_>) -> Result<Dispatch, String>;
    /// Nonblocking capability clone, invoked only after scoped-job admission.
    /// Must not run scripts, perform module I/O, or re-enter the job registry.
    fn fork<'a>(
        &self,
        _root: &'a DataRoot,
        _user: &Account,
        _worker: super::control::Worker,
    ) -> Result<ModuleFork<'a>, String> {
        Ok(ModuleFork::Unsupported)
    }
    /// Optional audit sink; never receives the script text or positional values.
    fn script_finished(
        &mut self,
        _name: &str,
        _status: u8,
        _steps: usize,
        _mode: &super::runtime::ExecutionMode,
    ) -> Result<(), String> {
        Ok(())
    }
}

/// Borrow the same task table, logger, identity and filesystem used by the outer shell.
pub struct SystemModules<'a> {
    pub root: &'a DataRoot,
    pub user: &'a Account,
    pub tasks: &'a mut TaskTable,
    pub logger: &'a Logger,
    pub files: &'a mut super::fd::FdSession,
}
impl ModuleCommands for SystemModules<'_> {
    fn script_finished(
        &mut self,
        name: &str,
        status: u8,
        steps: usize,
        mode: &super::runtime::ExecutionMode,
    ) -> Result<(), String> {
        self.logger
            .record(
                self.root,
                &self.user.name,
                &format!("script: {name}; status {status}; steps {steps}; mode {mode:?}"),
            )
            .map_err(|e| e.to_string())
    }
    fn execute(&mut self, request: Request<'_>) -> Result<Dispatch, String> {
        let Request {
            name,
            args,
            stdin,
            cwd,
        } = request;
        let mut out = Vec::new();
        let mut status = 0;
        match name {
            "owned" => {
                return super::owned::execute(self.root, self.user, cwd, args)
                    .map(Dispatch::Handled)
            }
            "mv" | "cp" | "stat" | "find" | "du" => {
                return super::fs_commands::execute(self.root, self.user, cwd, name, args)
                    .map(Dispatch::Handled);
            }
            "fd" => {
                return self
                    .files
                    .execute(self.root, self.user, cwd, args, stdin)
                    .map(Dispatch::Handled)
            }
            "wc" => {
                let (mode, paths) = super::count::options(args);
                let mut bytes = Vec::new();
                let input_use = if paths.is_empty() {
                    if stdin.len() > MAX_OUTPUT {
                        return Err("module input exceeds 64 KiB".into());
                    }
                    bytes.extend_from_slice(stdin);
                    InputUse::Consumed(stdin.len())
                } else {
                    for path in paths {
                        let path = cwd.resolve(path).map_err(|e| e.to_string())?;
                        crate::permissions::check(
                            self.user,
                            &path,
                            crate::permissions::Access::Read,
                        )
                        .map_err(|e| e.to_string())?;
                        let data = self
                            .root
                            .read_file(&path.to_string(), MAX_OUTPUT as u64)
                            .map_err(|e| e.to_string())?;
                        if data.len() > MAX_OUTPUT.saturating_sub(bytes.len()) {
                            return Err("module input exceeds 64 KiB".into());
                        }
                        bytes.extend_from_slice(&data);
                    }
                    InputUse::Untouched
                };
                let mut counter = super::count::Counter::new(mode);
                counter.feed(&bytes)?;
                let count = counter.value();
                writeln!(out, "{count}").map_err(|e| e.to_string())?;
                return Ok(Dispatch::Handled(CommandOutput {
                    status: 0,
                    stdout: out,
                    stderr: Vec::new(),
                    input_use,
                }));
            }
            "ps" => {
                if !args.is_empty() {
                    return Err("ps takes no arguments".into());
                }
                writeln!(out, "ID    STATE      COMMAND").map_err(|e| e.to_string())?;
                for task in self.tasks.snapshot() {
                    writeln!(
                        out,
                        "{:<5} {:<10} {}",
                        task.id,
                        task.state.to_string(),
                        task.command
                    )
                    .map_err(|e| e.to_string())?;
                }
            }
            "sleep" => {
                let (seconds, background) = match args {
                    [seconds] => (seconds, false),
                    [seconds, mode] if mode == "bg" => (seconds, true),
                    _ => return Err("Usage: sleep <seconds> [bg]".into()),
                };
                let duration = tasks::parse_seconds(seconds).map_err(|e| e.to_string())?;
                if background {
                    let id = self
                        .tasks
                        .spawn_sleep(duration)
                        .map_err(|e| e.to_string())?;
                    writeln!(out, "[{id}] started: sleep {seconds}").map_err(|e| e.to_string())?;
                } else {
                    std::thread::sleep(duration);
                    let id = self
                        .tasks
                        .record_finished(&format!("sleep {seconds}"), ExitStatus::Success);
                    writeln!(out, "[{id}] exited 0").map_err(|e| e.to_string())?;
                }
            }
            "wait" => {
                let [id] = args else {
                    return Err("Usage: wait <task-id>".into());
                };
                let id = id.parse().map_err(|_| "invalid task id")?;
                match self.tasks.wait(id).map_err(|e| e.to_string())? {
                    ExitStatus::Success => {
                        writeln!(out, "[{id}] exited 0").map_err(|e| e.to_string())?
                    }
                    ExitStatus::Failure(code) => {
                        status = u8::try_from(code).unwrap_or(1);
                        writeln!(out, "[{id}] exited {code}").map_err(|e| e.to_string())?;
                    }
                }
            }
            "users" => {
                if !args.is_empty() {
                    return Err("users takes no arguments".into());
                }
                for user in users::load(self.root).map_err(|e| e.to_string())? {
                    writeln!(out, "{:<16} uid {:<6} {}", user.name, user.uid, user.home)
                        .map_err(|e| e.to_string())?;
                    if out.len() > MAX_OUTPUT {
                        return Err("module output exceeds 64 KiB".into());
                    }
                }
            }
            "log" => {
                if !self.user.is_admin() {
                    return Err(format!(
                        "Permission denied: cannot read {}.",
                        logging::LOG_PATH
                    ));
                }
                let count = match args {
                    [] => logging::DEFAULT_TAIL,
                    [count] => count
                        .parse::<usize>()
                        .ok()
                        .filter(|n| (1..=1000).contains(n))
                        .ok_or("Usage: log [1-1000]")?,
                    _ => return Err("Usage: log [1-1000]".into()),
                };
                for entry in self
                    .logger
                    .tail(self.root, count)
                    .map_err(|e| e.to_string())?
                {
                    writeln!(out, "{entry}").map_err(|e| e.to_string())?;
                    if out.len() > MAX_OUTPUT {
                        return Err("module output exceeds 64 KiB".into());
                    }
                }
            }
            _ => return Ok(Dispatch::Unsupported),
        }
        if out.len() > MAX_OUTPUT {
            return Err("module output exceeds 64 KiB".into());
        }
        Ok(Dispatch::Handled(CommandOutput {
            status,
            stdout: out,
            stderr: Vec::new(),
            input_use: InputUse::Untouched,
        }))
    }
}
