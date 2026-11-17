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
use std::{format, matches};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
pub struct Resources {
    tasks: Arc<Mutex<TaskTable>>,
    files: Arc<Mutex<FdSession>>,
    pub logger: Arc<Logger>,
    domain: Option<crate::filesystem::ApplicationDomain>,
}
impl Resources {
    pub fn new(uid: u32, logger: Arc<Logger>, task_capacity: usize) -> Result<Self, String> {
        Ok(Self {
            tasks: Arc::new(Mutex::new(TaskTable::new(task_capacity))),
            files: Arc::new(Mutex::new(FdSession::new(uid).map_err(|e| e.to_string())?)),
            logger,
            domain: None,
        })
    }
    pub(crate) fn for_application(user: &Account, logger: Arc<Logger>) -> Result<Self, String> {
        let mut resources = Self::new(user.session_uid(), logger, 16)?;
        resources.domain = Some(user.application_domain().ok_or("missing application domain")?.clone());
        Ok(resources)
    }
    pub fn switch_owner(&self, uid: u32) -> Result<usize, String> {
        // Same task -> descriptors lock order as command dispatch. Acquire both
        // before changing either resource so a poisoned lock cannot half-switch.
        let mut tasks = self.tasks.lock().map_err(|_| "task resource lock poisoned")?;
        let mut files = self.files.lock().map_err(|_| "descriptor resource lock poisoned")?;
        tasks.retire_session();
        Ok(files.switch_owner(uid))
    }
}
pub struct SharedModules<'a> {
    root: &'a DataRoot,
    user: Account,
    resources: Resources,
    worker: Option<Worker>,
    active_task: Option<crate::filesystem::TaskScopeId>,
}
impl<'a> SharedModules<'a> {
    pub fn new(root: &'a DataRoot, user: &Account, resources: &Resources) -> Self {
        Self {
            root,
            user: user.clone(),
            resources: resources.clone(),
            worker: None,
            active_task: None,
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
    fn execute_as(&mut self, actor: &Account, request: Request<'_>) -> Result<Dispatch, String> {
        if !actor.same_execution_domain(&self.user) { return Err("module session mismatch".into()); }
        let old = std::mem::replace(&mut self.user, actor.clone());
        let result = self.execute(request);
        self.user = old;
        result
    }

    fn task_finished(&mut self, id: crate::filesystem::TaskScopeId) -> Result<(), String> {
        self.resources.files.lock().map_err(|_| "descriptor resource lock poisoned")?.finish_scope(id);
        self.active_task = None; Ok(())
    }
    fn fork<'a>(
        &self,
        root: &'a DataRoot,
        user: &Account,
        worker: Worker,
    ) -> Result<ModuleFork<'a>, String> {
        if !std::ptr::eq(self.root, root)
            || !self.user.same_execution_domain(user)
            || user.application_domain() != self.resources.domain.as_ref()
            || (matches!(user.identity, crate::users::Identity::Login) && self.user != *user)
        {
            return Err("module fork rejected: root/actor mismatch".into());
        }
        Ok(ModuleFork::Ready(Box::new(SharedModules {
            root,
            user: user.clone(),
            resources: self.resources.clone(),
            worker: Some(worker),
            active_task: None,
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
        if self.user.application_domain() != self.resources.domain.as_ref() {
            return Err("module resource domain mismatch".into());
        }
        if request.name == "app" && request.args.first().is_some_and(|s| s == "run") && self.worker.is_some() {
            return Err("app run currently requires a foreground session".into());
        }
        self.root.validate_application(&self.user).map_err(|e| e.to_string())?;
        if let Some(scope) = request.creation.scope { self.active_task = Some(scope.id()); }
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
                let id = if let Some(worker) = &self.worker {
                    worker.sleep(duration).map_err(|e| e.to_string())?;
                    self.resources.tasks.lock().map_err(|_| "task resource lock poisoned")?
                        .record_finished(&format!("sleep {seconds}"), ExitStatus::Success).map_err(|e| e.to_string())?
                } else {
                    let task = self.resources.tasks.lock().map_err(|_| "task resource lock poisoned")?
                        .begin_observed(self.root, self.user.session_uid(), &format!("sleep {seconds}"))
                        .map_err(|e| e.to_string())?;
                    let id = task.id;
                    std::thread::sleep(duration);
                    task.finish(ExitStatus::Success);
                    id
                };
                return Ok(Self::result(0, format!("[{id}] exited 0
")));
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

impl Drop for SharedModules<'_> {
    fn drop(&mut self) {
        if self.worker.is_some() { if let Some(id) = self.active_task.take() { if let Ok(mut files) = self.resources.files.lock() { files.finish_scope(id); } } }
    }
}
