//! ElaraOS task table: tasks are units of work owned by this program.
//! They are never host processes; nothing here can list, signal or spawn
//! anything outside the virtual system.
use std::fmt;
use std::sync::mpsc::{self, Receiver, TryRecvError};
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

struct Task {
    id: TaskId,
    command: String,
    state: TaskState,
    completion: Option<Receiver<ExitStatus>>,
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
        self.refresh();
        if self
            .tasks
            .iter()
            .filter(|task| task.state == TaskState::Running)
            .count()
            >= self.capacity
        {
            return Err(TaskError::TableFull);
        }
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            thread::sleep(duration);
            let _ = sender.send(ExitStatus::Success);
        });
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.tasks.push(Task {
            id,
            command: format!("sleep {}", duration.as_secs()),
            state: TaskState::Running,
            completion: Some(receiver),
        });
        Ok(id)
    }

    /// Record a task that already finished in the foreground.
    pub fn record_finished(&mut self, command: &str, status: ExitStatus) -> TaskId {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1).max(1);
        self.tasks.push(Task {
            id,
            command: command.to_owned(),
            state: TaskState::Exited(status),
            completion: None,
        });
        self.trim_exited();
        id
    }

    /// Poll background tasks without blocking.
    pub fn refresh(&mut self) {
        for task in &mut self.tasks {
            if let Some(receiver) = &task.completion {
                match receiver.try_recv() {
                    Ok(status) => task.state = TaskState::Exited(status),
                    Err(TryRecvError::Disconnected) => {
                        task.state = TaskState::Exited(ExitStatus::Failure(1))
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
            task.state = TaskState::Exited(status);
        }
        match task.state {
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
                state: task.state,
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
        let first = table.record_finished("echo a", ExitStatus::Success);
        let second = table.record_finished("rm missing", ExitStatus::Failure(1));
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
            table.record_finished("echo", ExitStatus::Success);
        }
        assert_eq!(table.snapshot().len(), 2);
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
