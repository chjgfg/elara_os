//! Shared real module resources for scoped script workers; no cloned fake task/FD tables.
use super::control::Worker;
use super::fd::FdSession;
use super::modules::{
    CommandOutput, Dispatch, InputUse, ModuleCommands, ModuleFork, Request, SystemModules,
};
use super::runtime::ExecutionMode;
use crate::filesystem::DataRoot;
use crate::logging::Logger;
use crate::tasks::{self, ExitStatus, TaskTable};
use crate::users::Account;
use std::format;
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Resources {
    tasks: Arc<Mutex<TaskTable>>,
    files: Arc<Mutex<FdSession>>,
    pub logger: Arc<Logger>,
}
impl Resources {
    pub fn new(uid: u32, logger: Arc<Logger>, task_capacity: usize) -> Result<Self, String> {
        Ok(Self {
            tasks: Arc::new(Mutex::new(TaskTable::new(task_capacity))),
            files: Arc::new(Mutex::new(FdSession::new(uid).map_err(|e| e.to_string())?)),
            logger,
        })
    }
    pub fn switch_owner(&self, uid: u32) -> Result<usize, String> {
        Ok(self
            .files
            .lock()
            .map_err(|_| "descriptor resource lock poisoned")?
            .switch_owner(uid))
    }
}
pub struct SharedModules<'a> {
    root: &'a DataRoot,
    user: Account,
    resources: Resources,
    worker: Option<Worker>,
}
impl<'a> SharedModules<'a> {
    pub fn new(root: &'a DataRoot, user: &Account, resources: &Resources) -> Self {
        Self {
            root,
            user: user.clone(),
            resources: resources.clone(),
            worker: None,
        }
    }
    fn result(status: u8, stdout: String) -> Dispatch {
        Dispatch::Handled(CommandOutput {
            status,
            stdout: stdout.into_bytes(),
            stderr: Vec::new(),
            input_use: InputUse::Untouched,
        })
    }
}
impl ModuleCommands for SharedModules<'_> {
    fn fork<'a>(
        &self,
        root: &'a DataRoot,
        user: &Account,
        worker: Worker,
    ) -> Result<ModuleFork<'a>, String> {
        if !std::ptr::eq(self.root, root)
            || self.user.uid != user.uid
            || self.user.name != user.name
            || self.user.home != user.home
        {
            return Err("module fork rejected: root/actor mismatch".into());
        }
        Ok(ModuleFork::Ready(Box::new(SharedModules {
            root,
            user: user.clone(),
            resources: self.resources.clone(),
            worker: Some(worker),
        })))
    }
    fn script_finished(
        &mut self,
        name: &str,
        status: u8,
        steps: usize,
        mode: &ExecutionMode,
    ) -> Result<(), String> {
        self.resources
            .logger
            .record(
                self.root,
                &self.user.name,
                &format!("script: {name}; status {status}; steps {steps}; mode {mode:?}"),
            )
            .map_err(|e| e.to_string())
    }
    fn execute(&mut self, request: Request<'_>) -> Result<Dispatch, String> {
        if let Some(worker) = &self.worker {
            worker.checkpoint().map_err(|e| e.to_string())?;
        }
        if request.name == "sleep" {
            let (seconds, background) = match request.args {
                [seconds] => (seconds, false),
                [seconds, mode] if mode == "bg" => (seconds, true),
                _ => return Err("Usage: sleep <seconds> [bg]".into()),
            };
            let duration = tasks::parse_seconds(seconds).map_err(|e| e.to_string())?;
            if !background {
                if let Some(worker) = &self.worker {
                    worker.sleep(duration).map_err(|e| e.to_string())?;
                } else {
                    std::thread::sleep(duration);
                }
                let id = self
                    .resources
                    .tasks
                    .lock()
                    .map_err(|_| "task resource lock poisoned")?
                    .record_finished(&format!("sleep {seconds}"), ExitStatus::Success);
                return Ok(Self::result(0, format!("[{id}] exited 0\n")));
            }
            if self.worker.is_some() {
                return Err("script jobs cannot create detached legacy timers; use sleep N & in the scoped job environment".into());
            }
        }
        if request.name == "wait" {
            if self.worker.is_some() {
                return Err("legacy task wait is foreground-only; scoped jobs use wait %ID".into());
            }
            let [id] = request.args else {
                return Err("Usage: wait <task-id>".into());
            };
            let id = id.parse::<tasks::TaskId>().map_err(|_| "invalid task id")?;
            let status = self
                .resources
                .tasks
                .lock()
                .map_err(|_| "task resource lock poisoned")?
                .wait(id)
                .map_err(|e| e.to_string())?;
            let (code, raw) = match status {
                ExitStatus::Success => (0, 0),
                ExitStatus::Failure(code) => (u8::try_from(code).unwrap_or(1), code),
            };
            return Ok(Self::result(code, format!("[{id}] exited {raw}\n")));
        }
        // Fixed lock order. Blocking timers/waits above never hold the FD lock.
        let mut tasks = self
            .resources
            .tasks
            .lock()
            .map_err(|_| "task resource lock poisoned")?;
        let mut files = self
            .resources
            .files
            .lock()
            .map_err(|_| "descriptor resource lock poisoned")?;
        SystemModules {
            root: self.root,
            user: &self.user,
            tasks: &mut tasks,
            logger: &self.resources.logger,
            files: &mut files,
        }
        .execute(request)
    }
}
