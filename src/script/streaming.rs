//! Concurrent bounded pipelines for stream-capable builtins. Explicit fallback for interpreter/module operations.
use super::control::{Interrupt, Worker};
use super::lexer::{Op, Quote};
use super::parser::Pipeline;
use super::pipe;
use crate::filesystem::{DataRoot, FileMode, OpenFile};
use crate::permissions::{self, Access};
use crate::users::Account;
use crate::virtual_path::VirtualPath;
use std::io::{self, Cursor, Read, Write};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::{format, matches, vec};

pub const MAX_WORKERS: usize = 16;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WorkClass {
    Foreground,
    Utility,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BufferedReason {
    InterpreterCommand,
    ComplexExpansion,
    ExpansionPreflight,
    AtomicOutputRedirect,
    StageQuota,
    WorkerQuota,
    WorkerPoolPoisoned,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Strategy {
    Streaming {
        chunk_bytes: usize,
        queued_chunks: usize,
        workers: usize,
    },
    Buffered {
        reason: BufferedReason,
        byte_limit: usize,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StageOutcome {
    Exited(u8),
    Panicked,
    NotStarted,
}
#[derive(Clone, Debug)]
pub enum QueueSnapshot {
    Known(pipe::Stats),
    Unavailable(String),
}
#[derive(Clone, Debug)]
pub struct Trace {
    pub strategy: Strategy,
    pub stage_statuses: Vec<StageOutcome>,
    pub queues: Vec<QueueSnapshot>,
    pub inherited_input_consumed: usize,
}
pub enum Plan {
    Streaming,
    Buffered(BufferedReason),
}
#[derive(Clone, Copy)]
pub enum Builtin {
    Echo,
    Printf,
    Cat,
    Wc,
    Pwd,
    Whoami,
    Status(u8),
}
pub struct Stage {
    pub builtin: Builtin,
    pub args: Vec<String>,
    pub cwd: VirtualPath,
    pub input_file: Option<String>,
}
pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub statuses: Vec<StageOutcome>,
    pub queues: Vec<QueueSnapshot>,
    pub consumed: usize,
}
pub enum Completion {
    Complete(Output),
    Failed { error: io::Error, partial: Output },
}

pub fn builtin(name: &str) -> Option<Builtin> {
    match name {
        "echo" => Some(Builtin::Echo),
        "printf" => Some(Builtin::Printf),
        "cat" => Some(Builtin::Cat),
        "wc" => Some(Builtin::Wc),
        "pwd" => Some(Builtin::Pwd),
        "whoami" => Some(Builtin::Whoami),
        "true" | ":" => Some(Builtin::Status(0)),
        "false" => Some(Builtin::Status(1)),
        _ => None,
    }
}
pub fn plan(pipeline: &Pipeline) -> Plan {
    if pipeline.commands.len() > MAX_WORKERS {
        return Plan::Buffered(BufferedReason::StageQuota);
    }
    for command in &pipeline.commands {
        if command.redirects.iter().any(|r| r.op != Op::Input) {
            return Plan::Buffered(BufferedReason::AtomicOutputRedirect);
        }
        if command
            .words
            .first()
            .and_then(|word| word.bare())
            .and_then(builtin)
            .is_none()
        {
            return Plan::Buffered(BufferedReason::InterpreterCommand);
        }
        for word in command
            .words
            .iter()
            .chain(command.redirects.iter().map(|r| &r.path))
        {
            if word.parts.iter().any(|part| {
                matches!(part.quote, Quote::Bare | Quote::Double)
                    && (part.text.contains("$(") || part.text.contains("${"))
            }) {
                return Plan::Buffered(BufferedReason::ComplexExpansion);
            }
        }
    }
    Plan::Streaming
}
struct Pool {
    used: usize,
    utility: usize,
}
static POOL: Mutex<Pool> = Mutex::new(Pool {
    used: 0,
    utility: 0,
});
pub struct Reservation {
    count: usize,
    class: WorkClass,
}
pub fn reserve(count: usize, class: WorkClass) -> Result<Reservation, BufferedReason> {
    let mut pool = POOL
        .lock()
        .map_err(|_| BufferedReason::WorkerPoolPoisoned)?;
    if count > MAX_WORKERS.saturating_sub(pool.used)
        || (class == WorkClass::Utility && count > 8usize.saturating_sub(pool.utility))
    {
        return Err(BufferedReason::WorkerQuota);
    }
    pool.used += count;
    if class == WorkClass::Utility {
        pool.utility += count;
    }
    Ok(Reservation { count, class })
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if let Ok(mut pool) = POOL.lock() {
            pool.used -= self.count;
            if self.class == WorkClass::Utility {
                pool.utility -= self.count;
            }
        }
    }
}

fn display_error(error: impl std::fmt::Display) -> io::Error {
    io::Error::other(error.to_string())
}
struct Source {
    kind: SourceKind,
    worker: Worker,
}
enum SourceKind {
    Bytes {
        cursor: Cursor<Vec<u8>>,
        consumed: Arc<AtomicUsize>,
    },
    File {
        file: OpenFile,
        remaining: u64,
    },
    Pipe(pipe::Reader),
    Empty,
}
impl Source {
    fn file(
        root: &DataRoot,
        user: &Account,
        cwd: &VirtualPath,
        path: &str,
        worker: Worker,
    ) -> io::Result<Self> {
        worker.checkpoint()?;
        let path = cwd.resolve(path).map_err(display_error)?;
        permissions::check(user, &path, Access::Read).map_err(display_error)?;
        let file = root
            .open_file(&path.to_string(), FileMode::READ)
            .map_err(display_error)?;
        let remaining = file.info().map_err(display_error)?.length;
        Ok(Self {
            kind: SourceKind::File { file, remaining },
            worker,
        })
    }
}
impl Read for Source {
    fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
        self.worker.checkpoint()?;
        match &mut self.kind {
            SourceKind::Bytes { cursor, consumed } => {
                let count = cursor.read(target)?;
                consumed.fetch_add(count, Ordering::AcqRel);
                Ok(count)
            }
            SourceKind::File { file, remaining } => {
                if *remaining == 0 || target.is_empty() {
                    return Ok(0);
                }
                let limit = target
                    .len()
                    .min(usize::try_from((*remaining).min(4096)).map_err(display_error)?);
                let bytes = file.read(limit).map_err(display_error)?;
                target[..bytes.len()].copy_from_slice(&bytes);
                *remaining -= bytes.len() as u64;
                Ok(bytes.len())
            }
            SourceKind::Pipe(reader) => reader.read(target),
            SourceKind::Empty => Ok(0),
        }
    }
}
fn copy(input: &mut impl Read, output: &mut impl Write) -> io::Result<()> {
    let mut bytes = [0; 4096];
    loop {
        let count = input.read(&mut bytes)?;
        if count == 0 {
            return Ok(());
        }
        output.write_all(&bytes[..count])?;
    }
}
enum Piece {
    Text(String),
    String,
    Integer,
}
pub(crate) fn format_to(args: &[String], output: &mut impl Write) -> io::Result<()> {
    let Some((pattern, values)) = args.split_first() else {
        return Err(io::Error::other("printf requires a format"));
    };
    let mut pieces = Vec::new();
    let mut literal = String::new();
    let mut chars = pattern.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => literal.push(match chars.next() {
                Some('n') => '\n',
                Some('t') => '\t',
                Some('\\') => '\\',
                _ => return Err(io::Error::other("unsupported printf escape")),
            }),
            '%' => match chars.next() {
                Some('%') => literal.push('%'),
                Some(c @ ('s' | 'd')) => {
                    if !literal.is_empty() {
                        pieces.push(Piece::Text(std::mem::take(&mut literal)));
                    }
                    pieces.push(if c == 's' {
                        Piece::String
                    } else {
                        Piece::Integer
                    });
                }
                _ => return Err(io::Error::other("printf supports only %s, %d and %%")),
            },
            _ => literal.push(c),
        }
    }
    if !literal.is_empty() {
        pieces.push(Piece::Text(literal));
    }
    let consumes = pieces.iter().any(|piece| !matches!(piece, Piece::Text(_)));
    if !consumes && !values.is_empty() {
        return Err(io::Error::other("printf has unused arguments"));
    }
    let mut at = 0;
    loop {
        for piece in &pieces {
            match piece {
                Piece::Text(text) => output.write_all(text.as_bytes())?,
                Piece::String => {
                    output.write_all(values.get(at).map_or("", String::as_str).as_bytes())?;
                    at += 1;
                }
                Piece::Integer => {
                    let value = values
                        .get(at)
                        .map_or("0", String::as_str)
                        .parse::<i64>()
                        .map_err(|_| io::Error::other("printf %d requires an integer"))?;
                    output.write_all(value.to_string().as_bytes())?;
                    at += 1;
                }
            }
        }
        if at >= values.len() || !consumes {
            return Ok(());
        }
    }
}
fn run(
    stage: &Stage,
    root: &DataRoot,
    user: &Account,
    mut input: Source,
    writer: &mut pipe::Writer,
    worker: &Worker,
) -> io::Result<u8> {
    worker.checkpoint()?;
    let (wc_mode, wc_paths) = super::count::options(&stage.args);
    let consumes = matches!(stage.builtin, Builtin::Cat) && stage.args.is_empty()
        || matches!(stage.builtin, Builtin::Wc) && wc_paths.is_empty();
    if !consumes {
        input = Source {
            kind: SourceKind::Empty,
            worker: worker.clone(),
        };
    }
    match stage.builtin {
        Builtin::Echo => {
            let (newline, args) = if stage.args.first().is_some_and(|arg| arg == "-n") {
                (false, &stage.args[1..])
            } else {
                (true, stage.args.as_slice())
            };
            for (at, arg) in args.iter().enumerate() {
                if at != 0 {
                    writer.write_all(b" ")?;
                }
                writer.write_all(arg.as_bytes())?;
            }
            if newline {
                writer.write_all(b"\n")?;
            }
        }
        Builtin::Printf => format_to(&stage.args, writer)?,
        Builtin::Cat => {
            if stage.args.is_empty() {
                copy(&mut input, writer)?;
            } else {
                for path in &stage.args {
                    let mut file = Source::file(root, user, &stage.cwd, path, worker.clone())?;
                    copy(&mut file, writer)?;
                }
            }
        }
        Builtin::Wc => {
            let mut counter = super::count::Counter::new(wc_mode);
            let mut consume = |source: &mut Source| -> io::Result<()> {
                let mut bytes = [0; 4096];
                loop {
                    let n = source.read(&mut bytes)?;
                    if n == 0 {
                        break;
                    }
                    counter.feed(&bytes[..n]).map_err(display_error)?;
                }
                Ok(())
            };
            if wc_paths.is_empty() {
                consume(&mut input)?;
            } else {
                for path in wc_paths {
                    let mut file = Source::file(root, user, &stage.cwd, path, worker.clone())?;
                    consume(&mut file)?;
                }
            }
            writer.write_all(format!("{}\n", counter.value()).as_bytes())?;
        }
        Builtin::Pwd | Builtin::Whoami => {
            if !stage.args.is_empty() {
                return Err(io::Error::other("pwd/whoami take no arguments"));
            }
            let value = if matches!(stage.builtin, Builtin::Pwd) {
                stage.cwd.to_string()
            } else {
                user.name.clone()
            };
            writer.write_all(format!("{value}\n").as_bytes())?;
        }
        Builtin::Status(status) => {
            if !stage.args.is_empty() {
                return Err(io::Error::other("true/false/: take no arguments"));
            }
            return Ok(status);
        }
    }
    Ok(0)
}
struct StageResult {
    status: u8,
    stderr: Vec<u8>,
}
fn error(error: io::Error) -> StageResult {
    match error.kind() {
        io::ErrorKind::BrokenPipe => StageResult {
            status: 141,
            stderr: Vec::new(),
        },
        io::ErrorKind::ConnectionAborted => StageResult {
            status: 130,
            stderr: Vec::new(),
        },
        _ => StageResult {
            status: 1,
            stderr: format!("Error: {error}\n").into_bytes(),
        },
    }
}

pub fn execute(
    stages: Vec<Stage>,
    root: &DataRoot,
    user: &Account,
    stdin: &[u8],
    worker: Option<&Worker>,
    output_limit: usize,
    reservation: Reservation,
) -> io::Result<Completion> {
    if stages.is_empty()
        || stages.len() > MAX_WORKERS
        || stages.len() != reservation.count
        || stdin.len() > 65536
        || output_limit > 65536
    {
        return Err(io::Error::other("invalid streaming budget/stage count"));
    }
    for stage in &stages {
        if stage.args.len() > 1024 || stage.args.iter().map(String::len).sum::<usize>() > 65536 {
            return Err(io::Error::other("streaming argument budget exceeded"));
        }
    }
    let count = stages.len();
    let control = worker.map(Worker::control).unwrap_or_default();
    let interrupt = Interrupt::new();
    let root_worker = match worker {
        Some(worker) => worker.clone(),
        None => control.worker()?,
    }
    .with_interrupt(&interrupt);
    let workers: Vec<Worker> = (0..count)
        .map(|_| control.worker().map(|w| w.with_interrupt(&interrupt)))
        .collect::<io::Result<_>>()?;
    let config = pipe::Config::default();
    let consumed = Arc::new(AtomicUsize::new(0));
    let mut inputs: Vec<Option<Source>> = (0..count).map(|_| None).collect();
    let mut writers = Vec::new();
    let mut probes = Vec::new();
    inputs[0] = Some(Source {
        kind: SourceKind::Bytes {
            cursor: Cursor::new(stdin.to_vec()),
            consumed: consumed.clone(),
        },
        worker: workers[0].clone(),
    });
    let mut final_reader = None;
    for index in 0..count {
        let next = if index + 1 < count {
            workers[index + 1].clone()
        } else {
            root_worker.clone()
        };
        let (reader, writer, probe) = pipe::bounded(config, next, workers[index].clone())?;
        if index + 1 < count {
            inputs[index + 1] = Some(Source {
                kind: SourceKind::Pipe(reader),
                worker: workers[index + 1].clone(),
            });
        } else {
            final_reader = Some(reader);
        }
        writers.push(Some(writer));
        probes.push(probe);
    }
    let mut reader =
        final_reader.ok_or_else(|| io::Error::other("pipeline has no output reader"))?;
    let (stdout, results, mut failure) = std::thread::scope(|scope| {
        let mut handles = Vec::new();
        let mut failure = None;
        for (index, stage) in stages.into_iter().enumerate() {
            let (Some(input), Some(mut writer)) = (inputs[index].take(), writers[index].take())
            else {
                failure = Some(io::Error::other("pipeline endpoint missing"));
                interrupt.cancel();
                break;
            };
            let worker = workers[index].clone();
            let spawned = std::thread::Builder::new()
                .name("elara-pipe".into())
                .spawn_scoped(scope, move || {
                    let input = match &stage.input_file {
                        Some(path) => {
                            drop(input);
                            Source::file(root, user, &stage.cwd, path, worker.clone())
                        }
                        None => Ok(input),
                    };
                    let result = input
                        .and_then(|input| run(&stage, root, user, input, &mut writer, &worker));
                    match result {
                        Ok(status) => match writer.flush() {
                            Ok(()) => StageResult {
                                status,
                                stderr: Vec::new(),
                            },
                            Err(e) => error(e),
                        },
                        Err(e) => error(e),
                    }
                });
            match spawned {
                Ok(handle) => handles.push((index, handle)),
                Err(error) => {
                    interrupt.cancel();
                    failure = Some(error);
                    break;
                }
            }
        }
        drop(workers);
        drop(inputs);
        drop(writers);
        let mut stdout = Vec::new();
        let mut bytes = [0; 4096];
        if failure.is_none() {
            loop {
                match reader.read(&mut bytes) {
                    Ok(0) => break,
                    Ok(n) if n <= output_limit.saturating_sub(stdout.len()) => {
                        stdout.extend_from_slice(&bytes[..n])
                    }
                    Ok(_) => {
                        failure = Some(io::Error::other("64 KiB output limit exceeded"));
                        interrupt.cancel();
                        break;
                    }
                    Err(error) => {
                        failure = Some(error);
                        interrupt.cancel();
                        break;
                    }
                }
            }
        }
        drop(reader);
        let mut results = Vec::new();
        for (index, handle) in handles {
            match handle.join() {
                Ok(result) => results.push((index, Some(result))),
                Err(_) => {
                    interrupt.cancel();
                    failure.get_or_insert_with(|| io::Error::other("pipeline worker panicked"));
                    results.push((index, None));
                }
            }
        }
        (stdout, results, failure)
    });
    let mut stderr = Vec::new();
    let mut statuses = vec![StageOutcome::NotStarted; count];
    for (index, result) in results {
        match result {
            Some(result) => {
                statuses[index] = StageOutcome::Exited(result.status);
                if result.stderr.len() > 65536usize.saturating_sub(stderr.len()) {
                    failure.get_or_insert_with(|| io::Error::other("64 KiB stderr limit exceeded"));
                } else {
                    stderr.extend(result.stderr);
                }
            }
            None => statuses[index] = StageOutcome::Panicked,
        }
    }
    let queues = probes
        .into_iter()
        .map(|probe| match probe.stats() {
            Ok(stats) => QueueSnapshot::Known(stats),
            Err(error) => QueueSnapshot::Unavailable(error.to_string()),
        })
        .collect();
    let output = Output {
        stdout,
        stderr,
        statuses,
        queues,
        consumed: consumed.load(Ordering::Acquire),
    };
    Ok(match failure {
        Some(error) => Completion::Failed {
            error,
            partial: output,
        },
        None => Completion::Complete(output),
    })
}
