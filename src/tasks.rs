//! ElaraOS task table: tasks are units of work owned by this program.
//! They are never host processes; nothing here can list, signal or spawn
//! anything outside the virtual system.
use crate::filesystem::{DataRoot, FsError, JobObservation, ObservationKind, RuntimeJobState};
use std::fmt;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{
    atomic::{AtomicI64, Ordering},
    Arc,
};
use std::thread;
use std::time::Duration;
use std::{format, write};

pub type TaskId = u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExitStatus {
    Success,
    Failure(i32),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskState {
    Running,
    Exited(ExitStatus),
}

impl fmt::Display for TaskState {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Running => write!(output, "running"),
            Self::Exited(ExitStatus::Success) => write!(output, "exited 0"),
            Self::Exited(ExitStatus::Failure(code)) => write!(output, "exited {code}"),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum TaskError {
    UnknownTask(TaskId),
    InvalidDuration(String),
    TableFull,
}

impl fmt::Display for TaskError {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownTask(id) => write!(output, "No task with id {id}."),
            Self::InvalidDuration(text) => write!(
                output,
                "'{text}' is not a duration; use whole seconds from 0 to 3600."
            ),
            Self::TableFull => write!(output, "The task table is full; wait for tasks to finish."),
        }
    }
}

// The worker and proc read this same state; neither acquires the task-table lock.
struct ObservedTask(AtomicI64);
impl ObservedTask {
    fn new(state: TaskState) -> Self {
        let result = Self(AtomicI64::new(i64::MAX));
        result.set(state);
        result
    }
    fn set(&self, state: TaskState) {
        self.0.store(
            match state {
                TaskState::Running => i64::MAX,
                TaskState::Exited(ExitStatus::Success) => i64::MIN,
                TaskState::Exited(ExitStatus::Failure(code)) => i64::from(code),
            },
            Ordering::Release,
        );
    }
    fn get(&self) -> TaskState {
        match self.0.load(Ordering::Acquire) {
            i64::MAX => TaskState::Running,
            i64::MIN => TaskState::Exited(ExitStatus::Success),
            code => TaskState::Exited(ExitStatus::Failure(code as i32)),
        }
    }
}
impl JobObservation for ObservedTask {
    fn kind(&self) -> ObservationKind {
        ObservationKind::LegacyTask
    }
    fn state(&self) -> Result<RuntimeJobState, FsError> {
        Ok(match self.get() {
            TaskState::Running => RuntimeJobState::Running,
            TaskState::Exited(ExitStatus::Success) => RuntimeJobState::TaskExited(0),
            TaskState::Exited(ExitStatus::Failure(code)) => RuntimeJobState::TaskExited(code),
        })
    }
}
/// Completion capability; dropping an unfinished foreground operation records failure.
pub(crate) struct ForegroundTask {
    pub(crate) id: TaskId,
    source: Arc<ObservedTask>,
    sender: Option<mpsc::Sender<ExitStatus>>,
}
impl ForegroundTask {
    pub(crate) fn finish(mut self, status: ExitStatus) {
        self.source.set(TaskState::Exited(status));
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(status);
        }
    }
}
impl Drop for ForegroundTask {
    fn drop(&mut self) {
        if let Some(sender) = self.sender.take() {
            self.source.set(TaskState::Exited(ExitStatus::Failure(1)));
            let _ = sender.send(ExitStatus::Failure(1));
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskClass { Background, Foreground }

struct Task {
    class: TaskClass,
    id: TaskId,
    command: String,
    source: Arc<ObservedTask>,
    completion: Option<Receiver<ExitStatus>>,
    cancel: Option<mpsc::Sender<()>>,
}

pub struct TaskSnapshot {
    pub id: TaskId,
    pub command: String,
    pub state: TaskState,
}

pub struct TaskTable {
    next_id: TaskId,
    tasks: Vec<Task>,
    capacity: usize,
}

pub const MAX_SLEEP_SECONDS: u64 = 3600;

pub fn parse_seconds(text: &str) -> Result<Duration, TaskError> {
    text.parse::<u64>()
        .ok()
        .filter(|seconds| *seconds <= MAX_SLEEP_SECONDS)
        .map(Duration::from_secs)
        .ok_or_else(|| TaskError::InvalidDuration(text.to_owned()))
}

impl TaskTable {
    pub fn new(capacity: usize) -> Self {
        Self {
            next_id: 1,
            tasks: Vec::new(),
            capacity,
        }
    }

    /// Start a timer task that runs in the background and exits successfully.
    pub fn spawn_sleep(&mut self, duration: Duration) -> Result<TaskId, TaskError> {
        self.spawn_sleep_configured(duration, None)
    }
    pub(crate) fn spawn_sleep_observed(
        &mut self,
        root: &DataRoot,
        uid: u32,
        duration: Duration,
    ) -> Result<TaskId, TaskError> {
        self.spawn_sleep_configured(duration, Some((root, uid)))
    }
    fn spawn_sleep_configured(
        &mut self,
        duration: Duration,
        observation: Option<(&DataRoot, u32)>,
    ) -> Result<TaskId, TaskError> {
        let task = self.begin(&format!("sleep {}", duration.as_secs()), observation, TaskClass::Background)?;
        let id = task.id;
        let source = Arc::downgrade(&task.source);
        let mut task = task;
        let sender = task.sender.take().ok_or(TaskError::TableFull)?;
        // Only the table owns the observation: dropping a session cannot leave
        // its private rows visible while the detached legacy timer winds down.
        let (cancel, cancelled) = mpsc::channel();
        self.tasks.last_mut().ok_or(TaskError::TableFull)?.cancel = Some(cancel);
        thread::spawn(move || {
            if cancelled.recv_timeout(duration) == Err(mpsc::RecvTimeoutError::Timeout) {
                if let Some(source) = source.upgrade() {
                    source.set(TaskState::Exited(ExitStatus::Success));
                }
                let _ = sender.send(ExitStatus::Success);
            }
        });
        Ok(id)
    }
    pub(crate) fn begin_observed(
        &mut self,
        root: &DataRoot,
        uid: u32,
        command: &str,
    ) -> Result<ForegroundTask, TaskError> {
        self.begin(command, Some((root, uid)), TaskClass::Foreground)
    }
    fn begin(
        &mut self,
        command: &str,
        observation: Option<(&DataRoot, u32)>,
        class: TaskClass,
    ) -> Result<ForegroundTask, TaskError> {
        self.refresh();
        self.trim_exited();
        if self
            .tasks
            .iter()
            .filter(|t| t.class == class && t.source.get() == TaskState::Running)
            .count()
            >= match class { TaskClass::Background => self.capacity, TaskClass::Foreground => 1 }
        {
            return Err(TaskError::TableFull);
        }
        let id = self.next_id;
        let next = id.checked_add(1).ok_or(TaskError::TableFull)?;
        let source = Arc::new(ObservedTask::new(TaskState::Running));
        if let Some((root, uid)) = observation {
            root.observe_job(uid, u64::from(id), source.clone())
                .map_err(|_| TaskError::TableFull)?;
        }
        let (sender, receiver) = mpsc::channel();
        self.next_id = next;
        self.tasks.push(Task {
            class,
            id,
            command: command.to_owned(),
            source: source.clone(),
            completion: Some(receiver),
            cancel: None,
        });
        Ok(ForegroundTask {
            id,
            source,
            sender: Some(sender),
        })
    }

    /// Record a task that already finished in the foreground.
    pub fn record_finished(&mut self, command: &str, status: ExitStatus) -> Result<TaskId, TaskError> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or(TaskError::TableFull)?;
        self.tasks.push(Task {
            class: TaskClass::Foreground,
            id,
            command: command.to_owned(),
            source: Arc::new(ObservedTask::new(TaskState::Exited(status))),
            completion: None,
            cancel: None,
        });
        self.trim_exited();
        Ok(id)
    }

    /// Retire session records without reusing IDs. Dropping cancellation senders
    /// wakes timers immediately; workers own no file capabilities or strong sources.
    pub(crate) fn retire_session(&mut self) {
        self.tasks.clear();
    }

    /// Poll background tasks without blocking.
    pub fn refresh(&mut self) {
        for task in &mut self.tasks {
            if let Some(receiver) = &task.completion {
                match receiver.try_recv() {
                    Ok(status) => task.source.set(TaskState::Exited(status)),
                    Err(TryRecvError::Disconnected) => {
                        task.source.set(TaskState::Exited(ExitStatus::Failure(1)))
                    }
                    Err(TryRecvError::Empty) => continue,
                }
                task.completion = None;
            }
        }
    }

    /// Block until the task exits; returns its final status.
    pub fn wait(&mut self, id: TaskId) -> Result<ExitStatus, TaskError> {
        let task = self
            .tasks
            .iter_mut()
            .find(|task| task.id == id)
            .ok_or(TaskError::UnknownTask(id))?;
        if let Some(receiver) = task.completion.take() {
            let status = receiver.recv().unwrap_or(ExitStatus::Failure(1));
            task.source.set(TaskState::Exited(status));
        }
        match task.source.get() {
            TaskState::Exited(status) => Ok(status),
            TaskState::Running => Ok(ExitStatus::Failure(1)),
        }
    }

    pub fn snapshot(&mut self) -> Vec<TaskSnapshot> {
        self.refresh();
        self.tasks
            .iter()
            .map(|task| TaskSnapshot {
                id: task.id,
                command: task.command.clone(),
                state: task.source.get(),
            })
            .collect()
    }

    fn trim_exited(&mut self) {
        let exited = self
            .tasks
            .iter()
            .filter(|task| task.completion.is_none())
            .count();
        let mut excess = exited.saturating_sub(self.capacity);
        self.tasks.retain(|task| {
            if excess > 0 && task.completion.is_none() {
                excess -= 1;
                false
            } else {
                true
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_seconds, ExitStatus, TaskError, TaskState, TaskTable};
    use std::time::Duration;
    use std::{assert, assert_eq};

    #[test]
    fn background_sleep_appears_in_the_table_and_then_exits() {
        let mut table = TaskTable::new(4);
        let id = table.spawn_sleep(Duration::from_millis(50)).unwrap();
        let listed = table.snapshot();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, id);
        assert_eq!(table.wait(id).unwrap(), ExitStatus::Success);
        assert_eq!(
            table.snapshot()[0].state,
            TaskState::Exited(ExitStatus::Success)
        );
    }

    #[test]
    fn ids_increase_and_unknown_ids_are_errors() {
        let mut table = TaskTable::new(4);
        let first = table.record_finished("echo a", ExitStatus::Success).unwrap();
        let second = table.record_finished("rm missing", ExitStatus::Failure(1)).unwrap();
        assert!(second > first);
        assert_eq!(table.wait(99), Err(TaskError::UnknownTask(99)));
        assert_eq!(table.wait(second).unwrap(), ExitStatus::Failure(1));
    }

    #[test]
    fn the_running_task_count_is_bounded() {
        let mut table = TaskTable::new(1);
        table.spawn_sleep(Duration::from_secs(1)).unwrap();
        assert_eq!(
            table.spawn_sleep(Duration::from_secs(1)),
            Err(TaskError::TableFull)
        );
    }

    #[test]
    fn exited_history_is_trimmed_to_capacity() {
        let mut table = TaskTable::new(2);
        for _ in 0..5 {
            table.record_finished("echo", ExitStatus::Success).unwrap();
        }
        assert_eq!(table.snapshot().len(), 2);
    }

    #[test]
    fn observed_state_preserves_every_exit_variant() {
        for status in [ExitStatus::Success, ExitStatus::Failure(0), ExitStatus::Failure(i32::MIN), ExitStatus::Failure(i32::MAX)] {
            let mut table = TaskTable::new(2);
            let id = table.record_finished("finished", status).unwrap();
            assert_eq!(table.snapshot()[0].state, TaskState::Exited(status));
            assert_eq!(table.wait(id).unwrap(), status);
        }
    }

    #[test]
    fn foreground_timer_has_reserved_admission_when_background_is_full() {
        use super::TaskClass;
        let mut table = TaskTable::new(1);
        let background = table.begin("background", None, TaskClass::Background).unwrap();
        let foreground = table.begin("foreground", None, TaskClass::Foreground).unwrap();
        assert!(table.begin("extra", None, TaskClass::Foreground).is_err());
        assert!(table.begin("extra", None, TaskClass::Background).is_err());
        foreground.finish(ExitStatus::Success);
        background.finish(ExitStatus::Success);
    }

    #[test]
    fn exhausted_ids_do_not_wrap_or_mutate_records() {
        let mut table = TaskTable::new(1);
        table.next_id = u32::MAX;
        assert_eq!(table.record_finished("no wrap", ExitStatus::Success), Err(TaskError::TableFull));
        assert_eq!(table.spawn_sleep(Duration::ZERO), Err(TaskError::TableFull));
        assert!(table.snapshot().is_empty());
        assert_eq!(table.next_id, u32::MAX);
    }

    #[test]
    fn retiring_session_wakes_long_timer_instead_of_leaking_threads() {
        let mut table = TaskTable::new(1);
        table.spawn_sleep(Duration::from_secs(3600)).unwrap();
        let completion = table.tasks[0].completion.take().unwrap();
        table.retire_session();
        assert_eq!(completion.recv_timeout(Duration::from_secs(2)), Err(std::sync::mpsc::RecvTimeoutError::Disconnected));
        assert!(table.snapshot().is_empty());
        assert_eq!(table.spawn_sleep(Duration::ZERO).unwrap(), 2);
    }

    #[test]
    fn durations_are_whole_bounded_seconds() {
        assert_eq!(parse_seconds("2").unwrap(), Duration::from_secs(2));
        assert!(parse_seconds("-1").is_err());
        assert!(parse_seconds("1.5").is_err());
        assert!(parse_seconds("3601").is_err());
        assert!(parse_seconds("abc").is_err());
    }
}
