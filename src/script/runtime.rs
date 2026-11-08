//! In-process script interpreter. Buffered pipelines are explicit, bounded, and never host processes.
use super::lexer::{self, Op, Quote, Word};
use super::modules::{Dispatch, InputUse, ModuleCommands, Request};
use super::parser::{Command, Join, List, Node, Pipeline};
use crate::filesystem::{DataRoot, EntryKind, FsError};
use crate::permissions::{self, Access};
use crate::users::Account;
use crate::virtual_path::VirtualPath;
use std::collections::BTreeMap;
use std::{format, matches, vec};

pub const MAX_OUTPUT: usize = 64 * 1024;
pub const MAX_STEPS: usize = 4096;
pub const MAX_VARIABLES: usize = 256;
pub const MAX_FUNCTIONS: usize = 64;
/// One invocation budget shared by its synchronous calls and asynchronous descendants.
pub struct Budget {
    remaining: std::sync::atomic::AtomicUsize,
}
impl Default for Budget {
    fn default() -> Self {
        Self::new()
    }
}
impl Budget {
    pub fn new() -> Self {
        Self {
            remaining: std::sync::atomic::AtomicUsize::new(MAX_STEPS),
        }
    }
    fn claim(&self) -> Result<(), String> {
        self.remaining
            .fetch_update(
                std::sync::atomic::Ordering::AcqRel,
                std::sync::atomic::Ordering::Acquire,
                |remaining| remaining.checked_sub(1),
            )
            .map(|_| ())
            .map_err(|_| "script step budget exhausted".into())
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionMode {
    InProcessBuffered {
        byte_limit: usize,
    },
    InProcessStreaming {
        queue_bytes: usize,
        max_workers: usize,
    },
    InProcessMixed {
        buffer_limit: usize,
        queue_bytes: usize,
        max_workers: usize,
    },
}
/// Distinguish normal completion from an explicit exit request to the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Completion {
    Finished,
    ExitRequested,
}
#[derive(Debug)]
pub struct Report {
    pub completion: Completion,
    pub status: u8,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub steps: usize,
    pub mode: ExecutionMode,
    pub pipelines: Vec<super::streaming::Trace>,
}
#[derive(Debug, Clone)]
pub struct Context {
    pub cwd: VirtualPath,
    vars: BTreeMap<String, String>,
    functions: BTreeMap<String, List>,
    local_frames: Vec<BTreeMap<String, Option<String>>>,
    errexit: bool,
    nounset: bool,
    exit_trap: Option<List>,
    last: u8,
    parameters: Vec<String>,
}
impl Context {
    pub fn status(&self) -> u8 {
        self.last
    }
    pub fn record_status(&mut self, status: u8) {
        self.last = status;
    }
    pub fn prepare_identity(&mut self, user: &Account) {
        self.vars
            .entry("HOME".into())
            .or_insert_with(|| user.home.clone());
        self.vars
            .entry("USER".into())
            .or_insert_with(|| user.name.clone());
        if self.parameters.is_empty() {
            self.parameters.push("elara".into());
        }
    }
    pub fn new(cwd: VirtualPath) -> Self {
        Self {
            cwd,
            vars: BTreeMap::new(),
            functions: BTreeMap::new(),
            local_frames: Vec::new(),
            errexit: false,
            nounset: false,
            exit_trap: None,
            last: 0,
            parameters: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, Copy)]
enum Flow {
    Next(u8),
    Exit(u8),
    Abort(u8),
    Return(u8),
    Break,
    Continue,
}
enum PipelineAttempt {
    Executed(Flow),
    Buffered(super::streaming::BufferedReason),
}
#[derive(Clone, Copy)]
enum InputOrigin {
    Inherited,
    Pipeline,
    Redirected,
}
struct Engine<'work, 'scope, 'env> {
    root: &'env DataRoot,
    user: &'work Account,
    ctx: &'work mut Context,
    report: Report,
    worker: Option<super::control::Worker>,
    work_class: super::streaming::WorkClass,
    substitution_status: Option<u8>,
    abort_expansion: bool,
    suppress_errexit: usize,
    errexit_eligible: bool,
    reserved_prefix_bytes: usize,
    reserved_prefix_names: BTreeMap<String, usize>,
    depth: usize,
    stdin: Vec<u8>,
    modules: &'work mut dyn ModuleCommands,
    jobs: Option<super::jobs::JobTable<'scope, 'env>>,
    current_job: Option<super::jobs::JobId>,
    budget: std::sync::Arc<Budget>,
}

fn append(target: &mut Vec<u8>, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_OUTPUT.saturating_sub(target.len()) {
        return Err("64 KiB output limit exceeded".into());
    }
    target.extend_from_slice(bytes);
    Ok(())
}
fn extend_arguments(target: &mut Vec<String>, values: Vec<String>) -> Result<(), String> {
    if values.len() > 1024usize.saturating_sub(target.len()) {
        return Err("command expansion exceeds 1024 arguments".into());
    }
    let used: usize = target.iter().map(String::len).sum();
    let additional: usize = values.iter().map(String::len).sum();
    if additional > MAX_OUTPUT.saturating_sub(used) {
        return Err("command arguments exceed 64 KiB".into());
    }
    target.extend(values);
    Ok(())
}
fn assignment(word: &Word) -> Option<(&str, Word)> {
    let first = word.parts.first()?;
    if first.quote != Quote::Bare {
        return None;
    }
    let (name, value) = first.text.split_once('=')?;
    if !lexer::valid_name(name) {
        return None;
    }
    let mut parts = word.parts.clone();
    parts[0].text = value.into();
    Some((name, Word { parts }))
}

impl Engine<'_, '_, '_> {
    fn set_var(&mut self, name: &str, value: String) -> Result<(), String> {
        let mut names: std::collections::BTreeSet<&str> = self
            .ctx
            .vars
            .keys()
            .map(String::as_str)
            .chain(
                self.ctx
                    .local_frames
                    .iter()
                    .flat_map(|frame| frame.iter())
                    .filter(|(_, old)| old.is_some())
                    .map(|(name, _)| name.as_str()),
            )
            .chain(self.reserved_prefix_names.keys().map(String::as_str))
            .collect();
        names.insert(name);
        if names.len() > MAX_VARIABLES {
            return Err("variable count limit exceeded".into());
        }
        let used: usize = self
            .ctx
            .vars
            .iter()
            .filter(|(key, _)| key.as_str() != name)
            .map(|(key, value)| key.len() + value.len())
            .sum();
        let reserved_locals: usize = self
            .ctx
            .local_frames
            .iter()
            .flat_map(|frame| frame.iter())
            .map(|(name, value)| name.len() + value.as_ref().map_or(0, String::len))
            .sum();
        let used = used + reserved_locals + self.reserved_prefix_bytes;
        if name.len() + value.len() > MAX_OUTPUT.saturating_sub(used) {
            return Err("variable storage exceeds 64 KiB".into());
        }
        self.ctx.vars.insert(name.into(), value);
        Ok(())
    }
    fn cancellation_observed(&self) -> bool {
        self.worker.as_ref().is_some_and(|worker| {
            worker
                .control()
                .snapshot()
                .is_ok_and(|flags| flags.cancellation_observed)
        })
    }
    fn tick(&mut self) -> Result<(), String> {
        if let Some(worker) = &self.worker {
            if let Err(error) = worker.checkpoint() {
                self.abort_expansion = true;
                return Err(error.to_string());
            }
        }
        if self.report.steps >= MAX_STEPS {
            self.abort_expansion = true;
            return Err("script step budget exhausted".into());
        }
        if let Err(error) = self.budget.claim() {
            self.abort_expansion = true;
            return Err(error);
        }
        self.report.steps += 1;
        Ok(())
    }
    fn parameter(&self, name: &str) -> String {
        match name {
            "?" => self.ctx.last.to_string(),
            "#" => self.ctx.parameters.len().saturating_sub(1).to_string(),
            _ => match name.parse::<usize>() {
                Ok(i) => self.ctx.parameters.get(i).cloned().unwrap_or_default(),
                Err(_) => self.ctx.vars.get(name).cloned().unwrap_or_default(),
            },
        }
    }
    fn defined_parameter(&self, name: &str) -> bool {
        match name.parse::<usize>() {
            Ok(index) => index < self.ctx.parameters.len(),
            Err(_) => ["?", "#"].contains(&name) || self.ctx.vars.contains_key(name),
        }
    }
    fn get_parameter(&mut self, name: &str) -> Result<String, String> {
        if self.ctx.nounset && !self.defined_parameter(name) {
            self.abort_expansion = true;
            return Err(format!("unbound variable: {name}"));
        }
        Ok(self.parameter(name))
    }
    fn expand(&mut self, word: &Word) -> Result<Vec<String>, String> {
        let mut word = word.clone();
        if let Some(first) = word.parts.first() {
            if first.quote == Quote::Bare && (first.text == "~" || first.text.starts_with("~/")) {
                let mut tail = first.clone();
                tail.text = tail.text[1..].into();
                let home = self
                    .ctx
                    .vars
                    .get("HOME")
                    .cloned()
                    .unwrap_or_else(|| self.user.home.clone());
                word.parts.remove(0);
                word.parts.insert(0, tail);
                word.parts.insert(
                    0,
                    lexer::Part {
                        quote: Quote::Literal,
                        text: home,
                    },
                );
            }
        }
        if word.parts.len() == 1
            && matches!(word.parts[0].text.as_str(), "$@" | "${@}")
            && matches!(word.parts[0].quote, Quote::Double | Quote::Bare)
        {
            let values: Vec<_> = self.ctx.parameters.iter().skip(1).cloned().collect();
            if word.parts[0].quote == Quote::Double {
                return Ok(values);
            }
            let mut fields = Vec::new();
            let mut bytes = 0usize;
            for index in 1..self.ctx.parameters.len() {
                let values = self.expand(&Word {
                    parts: std::vec![lexer::Part {
                        quote: Quote::Bare,
                        text: format!("${{{index}}}")
                    }],
                })?;
                bytes += values.iter().map(String::len).sum::<usize>();
                if bytes > MAX_OUTPUT || fields.len() + values.len() > 1024 {
                    return Err("word expansion exceeds its argument budget".into());
                }
                fields.extend(values);
            }
            return Ok(fields);
        }
        let mut fields = Vec::new();
        let mut current = String::new();
        let mut mask = String::new();
        let mut present = false;
        let mut white_ended_field = false;
        let ifs = self
            .ctx
            .vars
            .get("IFS")
            .cloned()
            .unwrap_or_else(|| " \t\n".into());
        let mut size = 0;
        for part in &word.parts {
            if part.quote != Quote::Bare {
                present = true;
            }
            let mut chars = part.text.char_indices().peekable();
            while let Some((at, c)) = chars.next() {
                let mut expansion = false;
                let text = if c == '$' && matches!(part.quote, Quote::Bare | Quote::Double) {
                    if chars.peek().is_some_and(|(_, c)| ['(', '{'].contains(c)) {
                        let capture =
                            super::expansion::capture(&part.text, at).map_err(|e| e.to_string())?;
                        let value = match capture.kind {
                            super::expansion::Kind::Command => self.substitute(capture.body)?,
                            super::expansion::Kind::Arithmetic => {
                                self.eval_arithmetic(capture.body)?.to_string()
                            }
                            super::expansion::Kind::Parameter => {
                                self.parameter_expansion(capture.body)?
                            }
                        };
                        while chars.peek().is_some_and(|(at, _)| *at < capture.end) {
                            chars.next();
                        }
                        expansion = true;
                        value
                    } else {
                        let mut name = String::new();
                        match chars.peek().map(|(_, c)| *c) {
                            Some('?' | '#' | '@') => name.push(chars.next().unwrap().1),
                            Some(c) if c.is_ascii_digit() => name.push(chars.next().unwrap().1),
                            Some(c) if c == '_' || c.is_ascii_alphabetic() => {
                                while chars
                                    .peek()
                                    .is_some_and(|(_, c)| *c == '_' || c.is_ascii_alphanumeric())
                                {
                                    name.push(chars.next().unwrap().1);
                                }
                            }
                            _ => {}
                        }
                        if name.is_empty() {
                            "$".into()
                        } else {
                            if name == "@" {
                                return Err(
                                    "$@ must occupy a whole word in this interpreter".into()
                                );
                            }
                            expansion = true;
                            self.get_parameter(&name)?
                        }
                    }
                } else {
                    c.to_string()
                };
                size += text.len();
                if size > MAX_OUTPUT {
                    return Err("expanded word exceeds 64 KiB".into());
                }
                for c in text.chars() {
                    if expansion && part.quote == Quote::Bare && ifs.contains(c) {
                        if [' ', '\t', '\n'].contains(&c) {
                            if present || !current.is_empty() {
                                if fields.len() >= 1024 {
                                    return Err("word expansion exceeds its argument budget".into());
                                }
                                fields.push((
                                    std::mem::take(&mut current),
                                    std::mem::take(&mut mask),
                                ));
                                present = false;
                                white_ended_field = true;
                            }
                        } else {
                            if present || !current.is_empty() || !white_ended_field {
                                if fields.len() >= 1024 {
                                    return Err("word expansion exceeds its argument budget".into());
                                }
                                fields.push((
                                    std::mem::take(&mut current),
                                    std::mem::take(&mut mask),
                                ));
                            }
                            present = false;
                            white_ended_field = false;
                        }
                    } else {
                        white_ended_field = false;
                        current.push(c);
                        if part.quote != Quote::Bare && ['*', '?', '[', ']', '\\'].contains(&c) {
                            mask.push('\\');
                        }
                        mask.push(c);
                        present = true;
                    }
                }
            }
        }
        if present || !current.is_empty() {
            if fields.len() >= 1024 {
                return Err("word expansion exceeds its argument budget".into());
            }
            fields.push((current, mask));
        }
        self.glob_fields(fields)
    }
    fn glob_fields(&self, fields: Vec<(String, String)>) -> Result<Vec<String>, String> {
        use super::globbing::PathExpansion;
        let mut output = Vec::new();
        let mut bytes = 0usize;
        for (field, mask) in fields {
            let values = if super::pattern::has_wildcards(&mask)? {
                match super::globbing::expand(self.root, self.user, &self.ctx.cwd, &mask)? {
                    PathExpansion::Unmatched => std::vec![field],
                    PathExpansion::Matched(paths) => paths,
                }
            } else {
                std::vec![field]
            };
            for value in values {
                bytes += value.len();
                if bytes > MAX_OUTPUT || output.len() >= 1024 {
                    return Err("word expansion exceeds its argument budget".into());
                }
                output.push(value);
            }
        }
        Ok(output)
    }
    fn expand_value(&mut self, word: &Word) -> Result<String, String> {
        let mut word = word.clone();
        for part in &mut word.parts {
            if part.quote == Quote::Bare {
                part.quote = Quote::Double;
            }
        }
        Ok(self.expand(&word)?.join(" "))
    }
    fn expand_pattern(&mut self, word: &Word) -> Result<String, String> {
        let mut value = String::new();
        for part in &word.parts {
            let text = self.expand_value(&Word {
                parts: std::vec![part.clone()],
            })?;
            let text = if part.quote == Quote::Bare {
                text
            } else {
                super::pattern::escape(&text)
            };
            if text.len() > MAX_OUTPUT.saturating_sub(value.len()) {
                return Err("expanded pattern exceeds 64 KiB".into());
            }
            value.push_str(&text);
        }
        Ok(value)
    }
    fn eval_arithmetic(&mut self, text: &str) -> Result<i64, String> {
        let unbound = std::cell::Cell::new(false);
        let result = super::arithmetic::evaluate(text, &|name| {
            if self.ctx.nounset && !self.defined_parameter(name) {
                unbound.set(true);
                return Err(format!("unbound variable: {name}"));
            }
            let value = self.parameter(name);
            if value.is_empty() {
                Ok(0)
            } else {
                value
                    .parse()
                    .map_err(|_| format!("arithmetic variable is not an integer: {name}"))
            }
        });
        self.abort_expansion |= unbound.get();
        result
    }
    fn expand_fragment(&mut self, text: &str, pattern: bool) -> Result<String, String> {
        let mut result = String::new();
        let mut end = 0;
        for token in lexer::lex(text).map_err(|e| e.to_string())? {
            let lexer::Kind::Word(parsed) = token.kind else {
                return Err("expansion fragment requires words".into());
            };
            let gap = &text[end..token.byte];
            let value = if pattern {
                self.expand_pattern(&parsed)?
            } else {
                self.expand_value(&parsed)?
            };
            if gap.len() + value.len() > MAX_OUTPUT.saturating_sub(result.len()) {
                return Err("expanded word exceeds 64 KiB".into());
            }
            result.push_str(gap);
            result.push_str(&value);
            end = token.end;
        }
        let gap = &text[end..];
        if gap.len() > MAX_OUTPUT.saturating_sub(result.len()) {
            return Err("expanded word exceeds 64 KiB".into());
        }
        result.push_str(gap);
        Ok(result)
    }
    fn parameter_expansion(&mut self, body: &str) -> Result<String, String> {
        use super::expansion::Parameter;
        match super::expansion::parameter(body).map_err(|e| e.to_string())? {
            Parameter::Value(name) => self.get_parameter(name),
            Parameter::Length(name) => Ok(self.get_parameter(name)?.chars().count().to_string()),
            Parameter::Replace {
                name,
                all,
                pattern,
                replacement,
            } => {
                let value = self.get_parameter(name)?;
                let mask = self.expand_fragment(pattern, true)?;
                if super::pattern::has_wildcards(&mask)? {
                    return Err("parameter replacement supports only literal patterns".into());
                }
                let pattern = super::pattern::literal(&mask)?;
                let replacement = self.expand_fragment(replacement, false)?;
                if pattern.is_empty() {
                    return Ok(value);
                }
                let mut output = Vec::new();
                let mut end = 0;
                for (at, matched) in value.match_indices(&pattern) {
                    self.tick()?;
                    append(&mut output, &value.as_bytes()[end..at])?;
                    append(&mut output, replacement.as_bytes())?;
                    end = at + matched.len();
                    if !all {
                        break;
                    }
                }
                append(&mut output, &value.as_bytes()[end..])?;
                String::from_utf8(output).map_err(|_| "replacement output must be UTF-8".into())
            }
            Parameter::Trim {
                name,
                prefix,
                longest,
                word,
            } => {
                let value = self.get_parameter(name)?;
                let pattern = self.expand_fragment(word, true)?;
                let mut boundaries: Vec<usize> = value
                    .char_indices()
                    .map(|(at, _)| at)
                    .chain(std::iter::once(value.len()))
                    .collect();
                if prefix == longest {
                    boundaries.reverse();
                }
                for at in boundaries {
                    self.tick()?;
                    let candidate = if prefix { &value[..at] } else { &value[at..] };
                    if super::pattern::matches(&pattern, candidate)? {
                        return Ok(if prefix {
                            value[at..].into()
                        } else {
                            value[..at].into()
                        });
                    }
                }
                Ok(value)
            }
            Parameter::Modify {
                name,
                colon,
                op,
                word,
            } => {
                let value = self.parameter(name);
                let defined = match name.parse::<usize>() {
                    Ok(index) => index < self.ctx.parameters.len(),
                    Err(_) => ["?", "#"].contains(&name) || self.ctx.vars.contains_key(name),
                };
                let missing = !defined || colon && value.is_empty();
                if op == '+' && missing {
                    return Ok(String::new());
                }
                if op != '+' && !missing {
                    return Ok(value);
                }
                let result = self.expand_fragment(word, false)?;
                if op == '?' {
                    self.abort_expansion = true;
                    return Err(if result.is_empty() {
                        format!("{name}: parameter is unset or empty")
                    } else {
                        result
                    });
                }
                if op == '=' {
                    self.set_var(name, result.clone())?;
                }
                Ok(result)
            }
        }
    }

    fn finish_trap(&mut self, result: Result<Flow, String>) -> Result<Flow, String> {
        // Cooperative cancellation ends this environment; an EXIT handler must
        // not perform more module effects after the worker observed cancellation.
        if self.cancellation_observed() {
            self.ctx.exit_trap = None;
            return Ok(Flow::Abort(130));
        }
        let status = match &result {
            Ok(Flow::Next(code) | Flow::Exit(code) | Flow::Abort(code)) => *code,
            _ => 2,
        };
        let Some(handler) = self.ctx.exit_trap.take() else {
            return result;
        };
        self.ctx.last = status;
        let handled = self.list(&handler);
        self.ctx.last = status;
        match handled {
            Ok(Flow::Exit(code)) => Ok(Flow::Exit(code)),
            Ok(Flow::Next(_)) => result,
            Ok(_) => {
                append(
                    &mut self.report.stderr,
                    b"Error: invalid control flow in EXIT trap\n",
                )?;
                result
            }
            Err(error) => {
                append(
                    &mut self.report.stderr,
                    format!("Error: EXIT trap: {error}\n").as_bytes(),
                )?;
                result
            }
        }
    }
    fn substitute(&mut self, source: &str) -> Result<String, String> {
        if self.depth >= 32 {
            return Err("call depth exceeds 32".into());
        }
        let ast = super::parser::parse(source).map_err(|e| e.to_string())?;
        let saved = self.ctx.clone();
        self.ctx.exit_trap = None;
        let stdout = std::mem::take(&mut self.report.stdout);
        let previous_stdin = self.stdin.clone();
        self.depth += 1;
        let result = self.list(&ast);
        let result = self.finish_trap(result);
        self.depth -= 1;
        let out = std::mem::replace(&mut self.report.stdout, stdout);
        // stdin consumption and real module operations persist; variables/cwd do not.
        *self.ctx = saved;
        let status = match result? {
            Flow::Next(code) | Flow::Exit(code) | Flow::Abort(code) => code,
            _ => {
                self.stdin = previous_stdin;
                return Err("invalid control flow in command substitution".into());
            }
        };
        self.substitution_status = Some(status);
        let value =
            String::from_utf8(out).map_err(|_| "command substitution output must be UTF-8")?;
        if value.contains('\0') {
            return Err("command substitution cannot contain NUL".into());
        }
        Ok(value.trim_end_matches('\n').to_owned())
    }

    fn path(&self, name: &str, access: Access) -> Result<String, String> {
        let path = self.ctx.cwd.resolve(name).map_err(|e| e.to_string())?;
        permissions::check(self.user, &path, access).map_err(|e| e.to_string())?;
        Ok(path.to_string())
    }
    fn list(&mut self, list: &List) -> Result<Flow, String> {
        let mut status = 0;
        for node in list {
            let ticked = self.tick();
            if self.cancellation_observed() {
                return Ok(Flow::Abort(130));
            }
            ticked?;
            self.errexit_eligible = true;
            let result = self.node(node);
            if self.cancellation_observed() {
                return Ok(Flow::Abort(130));
            }
            match result? {
                Flow::Next(code) => {
                    status = code;
                    self.ctx.last = code;
                    if code != 0
                        && self.ctx.errexit
                        && self.suppress_errexit == 0
                        && self.errexit_eligible
                    {
                        return Ok(Flow::Exit(code));
                    }
                }
                flow => return Ok(flow),
            }
        }
        Ok(Flow::Next(status))
    }
    fn node(&mut self, node: &Node) -> Result<Flow, String> {
        match node {
            Node::Background { body } => {
                let jobs = self
                    .jobs
                    .clone()
                    .ok_or("background execution requires a scoped job environment")?;
                let label = super::interactive::audit_label(std::slice::from_ref(body.as_ref()));
                let id = jobs.spawn(
                    self.root,
                    self.user,
                    super::jobs::Spawn {
                        context: self.ctx.clone(),
                        program: vec![body.as_ref().clone()],
                        label: label.clone(),
                        budget: self.budget.clone(),
                    },
                    self.modules,
                )?;
                append(
                    &mut self.report.stdout,
                    format!("[{}] started job: {label}\n", id.number()).as_bytes(),
                )?;
                Ok(Flow::Next(0))
            }
            Node::Chain { first, rest } => {
                let mut eligible = rest.is_empty();
                let mut flow = self.conditional_pipeline(first, !rest.is_empty())?;
                for (index, (join, pipe)) in rest.iter().enumerate() {
                    let Flow::Next(code) = flow else {
                        return Ok(flow);
                    };
                    self.ctx.last = code;
                    if matches!(join, Join::And) && code == 0
                        || matches!(join, Join::Or) && code != 0
                    {
                        eligible = index + 1 == rest.len();
                        flow = self.conditional_pipeline(pipe, !eligible)?;
                    }
                }
                self.errexit_eligible = eligible;
                Ok(flow)
            }
            Node::If {
                branches,
                otherwise,
            } => {
                for (condition, body) in branches {
                    match self.condition(condition)? {
                        Flow::Next(0) => return self.list(body),
                        Flow::Next(_) => {}
                        flow => return Ok(flow),
                    }
                }
                self.list(otherwise)
            }
            Node::For { name, items, body } => {
                let values = match items {
                    Some(words) => {
                        let mut values = Vec::new();
                        for w in words {
                            extend_arguments(&mut values, self.expand(w)?)?;
                        }
                        values
                    }
                    None => self.ctx.parameters.iter().skip(1).cloned().collect(),
                };
                let mut status = 0;
                for value in values {
                    self.tick()?;
                    self.set_var(name, value)?;
                    match self.list(body)? {
                        Flow::Next(code) => status = code,
                        Flow::Break => break,
                        Flow::Continue => {}
                        flow => return Ok(flow),
                    }
                }
                Ok(Flow::Next(status))
            }
            Node::While {
                condition,
                body,
                until,
            } => {
                let mut status = 0;
                loop {
                    self.tick()?;
                    let run = match self.condition(condition)? {
                        Flow::Next(code) => (code == 0) != *until,
                        flow => return Ok(flow),
                    };
                    if !run {
                        break;
                    }
                    match self.list(body)? {
                        Flow::Next(code) => status = code,
                        Flow::Break => break,
                        Flow::Continue => {}
                        flow => return Ok(flow),
                    }
                }
                Ok(Flow::Next(status))
            }
            Node::Case { word, branches } => {
                let value = self.expand_value(word)?;
                for (patterns, body) in branches {
                    for pattern in patterns {
                        let pattern = self.expand_pattern(pattern)?;
                        if super::pattern::matches(&pattern, &value)? {
                            return self.list(body);
                        }
                    }
                }
                Ok(Flow::Next(0))
            }
            Node::Function { name, body } => {
                if [
                    "echo", "printf", "pwd", "cd", "whoami", "cat", "wc", "ls", "mkdir", "touch",
                    "rm", "rmdir", "export", "test", "exit", "return", "break", "continue", "true",
                    "false", "ps", "sleep", "wait", "users", "log", "fd", "mv", "cp", "stat",
                    "find", "du", "owned", "sh", "source", ".", "su", "passwd", "useradd", "clear",
                    "help", "info", "local", "unset", "set", "trap", "jobs", "fg", "bg", "kill",
                ]
                .contains(&name.as_str())
                {
                    return Err(
                        "a function cannot shadow a built-in/system command in this interpreter"
                            .into(),
                    );
                }
                if !self.ctx.functions.contains_key(name)
                    && self.ctx.functions.len() >= MAX_FUNCTIONS
                {
                    return Err("function count limit exceeded".into());
                }
                self.ctx.functions.insert(name.clone(), body.clone());
                Ok(Flow::Next(0))
            }
        }
    }
    fn condition(&mut self, list: &List) -> Result<Flow, String> {
        self.suppress_errexit += 1;
        let result = self.list(list);
        self.suppress_errexit -= 1;
        result
    }
    fn conditional_pipeline(
        &mut self,
        pipeline: &Pipeline,
        conditional: bool,
    ) -> Result<Flow, String> {
        self.suppress_errexit += usize::from(conditional);
        let result = self.pipeline(pipeline);
        self.suppress_errexit -= usize::from(conditional);
        result
    }
    fn record_pipeline(&mut self, trace: super::streaming::Trace) {
        use super::streaming::Strategy;
        self.report.pipelines.push(trace);
        let streamed = self
            .report
            .pipelines
            .iter()
            .any(|trace| matches!(trace.strategy, Strategy::Streaming { .. }));
        let buffered = self
            .report
            .pipelines
            .iter()
            .any(|trace| matches!(trace.strategy, Strategy::Buffered { .. }));
        self.report.mode = if streamed && buffered {
            ExecutionMode::InProcessMixed {
                buffer_limit: MAX_OUTPUT,
                queue_bytes: 32768,
                max_workers: super::streaming::MAX_WORKERS,
            }
        } else if streamed {
            ExecutionMode::InProcessStreaming {
                queue_bytes: 32768,
                max_workers: super::streaming::MAX_WORKERS,
            }
        } else {
            ExecutionMode::InProcessBuffered {
                byte_limit: MAX_OUTPUT,
            }
        };
    }
    fn attempt_stream(&mut self, pipeline: &Pipeline) -> Result<PipelineAttempt, String> {
        use super::streaming::{self, Plan, Stage, StageOutcome, Strategy, Trace};
        match streaming::plan(pipeline) {
            Plan::Streaming => {}
            Plan::Buffered(reason) => return Ok(PipelineAttempt::Buffered(reason)),
        }
        let reservation = match streaming::reserve(pipeline.commands.len(), self.work_class) {
            Ok(reservation) => reservation,
            Err(reason) => return Ok(PipelineAttempt::Buffered(reason)),
        };
        let prepared = (|| -> Result<Vec<Stage>, String> {
            let mut stages = Vec::new();
            for command in &pipeline.commands {
                if let Some(worker) = &self.worker {
                    worker.checkpoint().map_err(|e| e.to_string())?;
                }
                let mut args = Vec::new();
                for word in &command.words {
                    extend_arguments(&mut args, self.expand(word)?)?;
                }
                let Some((name, arguments)) = args.split_first() else {
                    return Err("streaming command missing".into());
                };
                let builtin = streaming::builtin(name).ok_or("streaming command changed")?;
                let mut input_file = None;
                for redirect in &command.redirects {
                    let fields = self.expand(&redirect.path)?;
                    let [path] = fields.as_slice() else {
                        return Err("redirection requires exactly one path".into());
                    };
                    input_file = Some(path.clone());
                }
                stages.push(Stage {
                    builtin,
                    args: arguments.to_vec(),
                    input_file,
                    cwd: self.ctx.cwd.clone(),
                });
            }
            Ok(stages)
        })();
        let stages = match prepared {
            Ok(stages) => stages,
            Err(_) => {
                return Ok(PipelineAttempt::Buffered(
                    streaming::BufferedReason::ExpansionPreflight,
                ))
            }
        };
        for _ in &stages {
            self.tick()?;
        }
        let completion = streaming::execute(
            stages,
            self.root,
            self.user,
            &self.stdin,
            self.worker.as_ref(),
            MAX_OUTPUT.saturating_sub(self.report.stdout.len()),
            reservation,
        )
        .map_err(|e| e.to_string())?;
        let (output, failure) = match completion {
            streaming::Completion::Complete(output) => (output, None),
            streaming::Completion::Failed { error, partial } => (partial, Some(error)),
        };
        if output.consumed > self.stdin.len() {
            return Err("invalid streaming stdin consumption".into());
        }
        self.stdin.drain(..output.consumed);
        let status = match output.statuses.last() {
            Some(StageOutcome::Exited(code)) => *code,
            _ => 125,
        };
        self.record_pipeline(Trace {
            strategy: Strategy::Streaming {
                chunk_bytes: 4096,
                queued_chunks: 8,
                workers: pipeline.commands.len(),
            },
            stage_statuses: output.statuses,
            queues: output.queues,
            inherited_input_consumed: output.consumed,
        });
        append(&mut self.report.stdout, &output.stdout)?;
        append(&mut self.report.stderr, &output.stderr)?;
        if let Some(error) = failure {
            append(
                &mut self.report.stderr,
                format!("Error: {error}\n").as_bytes(),
            )?;
            return Ok(PipelineAttempt::Executed(
                if error.kind() == std::io::ErrorKind::ConnectionAborted {
                    Flow::Abort(130)
                } else {
                    Flow::Next(1)
                },
            ));
        }
        Ok(PipelineAttempt::Executed(Flow::Next(status)))
    }
    fn pipeline(&mut self, pipeline: &Pipeline) -> Result<Flow, String> {
        let buffered_reason = if pipeline.commands.len() > 1 {
            match self.attempt_stream(pipeline)? {
                PipelineAttempt::Executed(flow) => return Ok(flow),
                PipelineAttempt::Buffered(reason) => Some(reason),
            }
        } else {
            None
        };
        let before_input = self.stdin.len();
        let mut statuses =
            vec![super::streaming::StageOutcome::NotStarted; pipeline.commands.len()];
        let mut input = self.stdin.clone();
        let mut flow = Flow::Next(0);
        for (stage, command) in pipeline.commands.iter().enumerate() {
            self.tick()?;
            let saved = if pipeline.commands.len() > 1 {
                Some(self.ctx.clone())
            } else {
                None
            };
            match self.command(
                command,
                &input,
                if stage == 0 {
                    InputOrigin::Inherited
                } else {
                    InputOrigin::Pipeline
                },
            ) {
                Ok((result, bytes)) => {
                    flow = result;
                    input = bytes;
                }
                Err(message) => {
                    append(
                        &mut self.report.stderr,
                        format!("Error: {message}\n").as_bytes(),
                    )?;
                    flow = if self.cancellation_observed() {
                        Flow::Abort(130)
                    } else if self.abort_expansion {
                        Flow::Abort(2)
                    } else {
                        Flow::Next(1)
                    };
                    self.abort_expansion = false;
                    input.clear();
                }
            }
            if let Some(saved) = saved {
                *self.ctx = saved;
                // Pipeline stages are subshell contexts: exit never exits the caller.
                flow = match flow {
                    Flow::Abort(130) if self.cancellation_observed() => Flow::Abort(130),
                    Flow::Exit(code) | Flow::Abort(code) => Flow::Next(code),
                    other => other,
                };
            }
            let status = match flow {
                Flow::Next(code) | Flow::Exit(code) | Flow::Abort(code) | Flow::Return(code) => {
                    code
                }
                _ => 2,
            };
            statuses[stage] = super::streaming::StageOutcome::Exited(status);
            if !matches!(flow, Flow::Next(_)) {
                break;
            }
        }
        if let Some(reason) = buffered_reason {
            self.record_pipeline(super::streaming::Trace {
                strategy: super::streaming::Strategy::Buffered {
                    reason,
                    byte_limit: MAX_OUTPUT,
                },
                stage_statuses: statuses,
                queues: Vec::new(),
                inherited_input_consumed: before_input.saturating_sub(self.stdin.len()),
            });
        }
        append(&mut self.report.stdout, &input)?;
        Ok(flow)
    }
    fn command(
        &mut self,
        command: &Command,
        input: &[u8],
        origin: InputOrigin,
    ) -> Result<(Flow, Vec<u8>), String> {
        self.substitution_status = None;
        self.abort_expansion = false;
        let reserved_before = self.reserved_prefix_bytes;
        let names_before = self.reserved_prefix_names.clone();
        let temporary = command.words.iter().any(|word| assignment(word).is_none());
        let saved_stdin = if matches!(origin, InputOrigin::Pipeline) {
            Some(std::mem::replace(&mut self.stdin, input.to_vec()))
        } else {
            None
        };
        let mut prefix = Vec::new();
        let mut at = 0;
        let result = (|| {
            while let Some((name, mut value)) = command.words.get(at).and_then(assignment) {
                // Assignment values are expanded without field splitting.
                for part in &mut value.parts {
                    if part.quote == Quote::Bare {
                        part.quote = Quote::Double;
                    }
                }
                let fields = self.expand(&value)?;
                let previous = self.ctx.vars.get(name).cloned();
                if temporary {
                    self.reserved_prefix_bytes +=
                        name.len() + previous.as_ref().map_or(0, String::len);
                    if previous.is_some() {
                        *self.reserved_prefix_names.entry(name.into()).or_default() += 1;
                    }
                }
                prefix.push((name.to_owned(), previous));
                self.set_var(name, fields.join(" "))?;
                at += 1;
            }
            self.command_inner(command, at, InputOrigin::Inherited)
        })();
        if at < command.words.len() || result.is_err() {
            for (name, previous) in prefix.into_iter().rev() {
                if let Some(value) = previous {
                    self.ctx.vars.insert(name, value);
                } else {
                    self.ctx.vars.remove(&name);
                }
            }
        }
        self.reserved_prefix_bytes = reserved_before;
        self.reserved_prefix_names = names_before;
        if let Some(saved_stdin) = saved_stdin {
            self.stdin = saved_stdin;
        }
        result
    }
    fn command_inner(
        &mut self,
        command: &Command,
        at: usize,
        origin: InputOrigin,
    ) -> Result<(Flow, Vec<u8>), String> {
        let mut arguments = Vec::new();
        for word in &command.words[at..] {
            if arguments
                .first()
                .is_some_and(|command: &String| ["local", "export"].contains(&command.as_str()))
                && assignment(word).is_some()
            {
                let mut word = word.clone();
                for part in &mut word.parts {
                    if part.quote == Quote::Bare {
                        part.quote = Quote::Double;
                    }
                }
                extend_arguments(&mut arguments, self.expand(&word)?)?;
            } else {
                extend_arguments(&mut arguments, self.expand(word)?)?;
            }
        }
        let mut redirected_input = None;
        let mut output_path = None;
        for redirect in &command.redirects {
            let names = self.expand(&redirect.path)?;
            let [name] = names.as_slice() else {
                return Err("redirection requires exactly one path".into());
            };
            if redirect.op == Op::Input {
                let path = self.path(name, Access::Read)?;
                redirected_input = Some(
                    self.root
                        .read_file(&path, MAX_OUTPUT as u64)
                        .map_err(|e| e.to_string())?,
                );
            } else {
                let path = self.path(name, Access::Write)?;
                match self.root.create_file(&path) {
                    Ok(()) | Err(FsError::AlreadyExists) => {}
                    Err(e) => return Err(e.to_string()),
                }
                if redirect.op == Op::Output {
                    self.root.write_file(&path, b"")
                } else {
                    self.root.append_file(&path, b"")
                }
                .map_err(|e| e.to_string())?;
                output_path = Some((path, redirect.op));
            }
        }
        let inherited_input = self.stdin.clone();
        let result = self.builtin(
            &arguments,
            redirected_input.as_deref().unwrap_or(&inherited_input),
            if redirected_input.is_some() {
                InputOrigin::Redirected
            } else {
                origin
            },
        );
        let (flow, mut bytes) = result?;
        if let Some((path, op)) = output_path {
            match self.root.create_file(&path) {
                Ok(()) | Err(FsError::AlreadyExists) => {}
                Err(e) => return Err(e.to_string()),
            }
            if op == Op::Append {
                self.root.append_file(&path, &bytes)
            } else {
                self.root.write_file(&path, &bytes)
            }
            .map_err(|e| e.to_string())?;
            bytes.clear();
        }
        Ok((flow, bytes))
    }
    fn builtin(
        &mut self,
        args: &[String],
        input: &[u8],
        origin: InputOrigin,
    ) -> Result<(Flow, Vec<u8>), String> {
        let Some((command, arguments)) = args.split_first() else {
            return Ok((
                Flow::Next(self.substitution_status.take().unwrap_or(0)),
                Vec::new(),
            ));
        };
        let mut out = Vec::new();
        let mut status = 0;
        match command.as_str() {
            ":" | "true" | "false" => {
                if !arguments.is_empty() {
                    return Err("true/false/: take no arguments".into());
                }
                status = u8::from(command == "false");
            }
            "echo" => {
                let (newline, args) = if arguments.first().is_some_and(|a| a == "-n") {
                    (false, &arguments[1..])
                } else {
                    (true, arguments)
                };
                append(&mut out, args.join(" ").as_bytes())?;
                if newline {
                    append(&mut out, b"\n")?;
                }
            }
            "printf" => out = print_format(arguments)?,
            "pwd" => {
                if !arguments.is_empty() {
                    return Err("pwd takes no arguments".into());
                }
                append(&mut out, format!("{}\n", self.ctx.cwd).as_bytes())?;
            }
            "whoami" => {
                if !arguments.is_empty() {
                    return Err("whoami takes no arguments".into());
                }
                append(&mut out, format!("{}\n", self.user.name).as_bytes())?;
            }
            "cd" => {
                let path = match arguments {
                    [] => self
                        .ctx
                        .vars
                        .get("HOME")
                        .map_or(self.user.home.as_str(), String::as_str),
                    [path] => path,
                    _ => return Err("cd takes one path".into()),
                };
                let path = self.path(path, Access::Read)?;
                self.root
                    .validate_directory(&path)
                    .map_err(|e| e.to_string())?;
                self.ctx.cwd = VirtualPath::strict(&path).map_err(|e| e.to_string())?;
            }
            "cat" => {
                if arguments.is_empty() {
                    append(&mut out, input)?;
                    if matches!(origin, InputOrigin::Inherited) {
                        self.stdin.clear();
                    }
                } else {
                    for path in arguments {
                        let path = self.path(path, Access::Read)?;
                        append(
                            &mut out,
                            &self
                                .root
                                .read_file(&path, MAX_OUTPUT as u64)
                                .map_err(|e| e.to_string())?,
                        )?;
                    }
                }
            }
            "ls" => {
                let path = match arguments {
                    [] => ".",
                    [path] => path,
                    _ => return Err("ls takes one directory".into()),
                };
                let path = self.path(path, Access::Read)?;
                for entry in self.root.list(&path).map_err(|e| e.to_string())? {
                    let suffix = match entry.kind {
                        EntryKind::Directory => "/",
                        EntryKind::File => "",
                        EntryKind::BlockedLink => " [link blocked]",
                    };
                    append(
                        &mut out,
                        format!("{}{suffix}\n", entry.name.to_string_lossy()).as_bytes(),
                    )?;
                }
            }
            "mkdir" | "touch" | "rm" | "rmdir" => {
                if arguments.is_empty() {
                    return Err(format!("Usage: {command} <path> [path...]"));
                }
                for path in arguments {
                    let path = self.path(path, Access::Write)?;
                    match command.as_str() {
                        "mkdir" => self.root.create_directory(&path),
                        "touch" => self.root.create_file(&path),
                        "rm" => self.root.remove_file(&path),
                        _ => self.root.remove_directory(&path),
                    }
                    .map_err(|e| e.to_string())?;
                }
            }
            "jobs" | "fg" | "bg" | "kill" => return self.job_command(command, arguments),
            "wait" if arguments.first().is_some_and(|id| id.starts_with('%')) => {
                return self.job_command(command, arguments)
            }
            "trap" => {
                let [text, event] = arguments else {
                    return Err("Usage: trap <commands|-> EXIT".into());
                };
                if event != "EXIT" && event != "0" {
                    return Err("only the EXIT trap is supported; no host signal handlers".into());
                }
                self.ctx.exit_trap = if text == "-" {
                    None
                } else {
                    Some(super::parser::parse(text).map_err(|e| e.to_string())?)
                };
            }
            "set" => {
                let mut errexit = self.ctx.errexit;
                let mut nounset = self.ctx.nounset;
                for option in arguments {
                    let Some(sign) = option.chars().next().filter(|c| ['-', '+'].contains(c))
                    else {
                        return Err("Usage: set [-e] [-u] [+e] [+u]".into());
                    };
                    if option.len() < 2 {
                        return Err("Usage: set [-e] [-u] [+e] [+u]".into());
                    }
                    for flag in option[1..].chars() {
                        match flag {
                            'e' => errexit = sign == '-',
                            'u' => nounset = sign == '-',
                            _ => return Err("set supports only e/u options".into()),
                        }
                    }
                }
                self.ctx.errexit = errexit;
                self.ctx.nounset = nounset;
            }
            "unset" => {
                for name in arguments {
                    if !lexer::valid_name(name) {
                        return Err("unset requires variable names".into());
                    }
                }
                for name in arguments {
                    self.ctx.vars.remove(name);
                }
            }
            "local" => {
                if self.ctx.local_frames.is_empty() {
                    return Err("local requires a function".into());
                }
                for argument in arguments {
                    let (name, value) = argument.split_once('=').unwrap_or((argument, ""));
                    if !lexer::valid_name(name) {
                        return Err("local requires variable names".into());
                    }
                    let frame = self.ctx.local_frames.last_mut().unwrap();
                    if !frame.contains_key(name) && frame.len() >= MAX_VARIABLES {
                        return Err("local variable count limit exceeded".into());
                    }
                    let old = self.ctx.vars.get(name).cloned();
                    self.ctx
                        .local_frames
                        .last_mut()
                        .unwrap()
                        .entry(name.into())
                        .or_insert(old);
                    self.set_var(name, value.into())?;
                }
            }
            "sh" | "source" | "." => return self.call_script(command, arguments, input, origin),
            "su" | "passwd" | "useradd" | "clear" | "help" | "info" => {
                return Err(
                    "session controls require a standalone command without redirection".into(),
                );
            }
            "export" => {
                for argument in arguments {
                    let (name, value) = argument
                        .split_once('=')
                        .map_or((argument.as_str(), None), |(n, v)| (n, Some(v)));
                    if !lexer::valid_name(name) {
                        return Err("invalid variable name".into());
                    }
                    if let Some(value) = value {
                        self.set_var(name, value.into())?;
                    }
                }
            }
            "test" | "[" => {
                let args = if command == "[" {
                    if arguments.last().map(String::as_str) != Some("]") {
                        return Err("missing ]".into());
                    }
                    &arguments[..arguments.len() - 1]
                } else {
                    arguments
                };
                status = u8::from(!self.test(args)?);
            }
            "exit" | "return" => {
                let code = match arguments {
                    [] => self.ctx.last,
                    [n] => n.parse::<u8>().map_err(|_| "status must be 0..255")?,
                    _ => return Err("exit/return take one status".into()),
                };
                return Ok((
                    if command == "exit" {
                        Flow::Exit(code)
                    } else {
                        Flow::Return(code)
                    },
                    out,
                ));
            }
            "break" | "continue" => {
                if !arguments.is_empty() {
                    return Err("break/continue take no arguments".into());
                }
                return Ok((
                    if command == "break" {
                        Flow::Break
                    } else {
                        Flow::Continue
                    },
                    out,
                ));
            }
            _ => {
                if let Some(body) = self.ctx.functions.get(command).cloned() {
                    if self.depth >= 32 {
                        return Err("call depth exceeds 32".into());
                    }
                    let mut params = Vec::new();
                    params.push(self.ctx.parameters.first().cloned().unwrap_or_default());
                    params.extend_from_slice(arguments);
                    let previous = std::mem::replace(&mut self.ctx.parameters, params);
                    self.depth += 1;
                    let stdout = std::mem::take(&mut self.report.stdout);
                    let previous_stdin = std::mem::replace(&mut self.stdin, input.to_vec());
                    self.ctx.local_frames.push(BTreeMap::new());
                    let result = self.list(&body);
                    let frame = self.ctx.local_frames.pop().expect("function scope exists");
                    for (name, previous) in frame {
                        if let Some(value) = previous {
                            self.ctx.vars.insert(name, value);
                        } else {
                            self.ctx.vars.remove(&name);
                        }
                    }
                    let remaining = std::mem::take(&mut self.stdin);
                    self.stdin = if matches!(origin, InputOrigin::Inherited) {
                        remaining
                    } else {
                        previous_stdin
                    };
                    out = std::mem::replace(&mut self.report.stdout, stdout);
                    self.depth -= 1;
                    self.ctx.parameters = previous;
                    return Ok((
                        match result? {
                            Flow::Return(code) => Flow::Next(code),
                            flow => flow,
                        },
                        out,
                    ));
                }
                match self.modules.execute(Request {
                    name: command,
                    args: arguments,
                    stdin: input,
                    cwd: &self.ctx.cwd,
                })? {
                    Dispatch::Handled(result) => {
                        if let InputUse::Consumed(count) = result.input_use {
                            if count > input.len() {
                                return Err("module reported invalid stdin consumption".into());
                            }
                            if matches!(origin, InputOrigin::Inherited) {
                                self.stdin.drain(..count);
                            }
                        }
                        append(&mut self.report.stderr, &result.stderr)?;
                        append(&mut out, &result.stdout)?;
                        return Ok((Flow::Next(result.status), out));
                    }
                    Dispatch::Unsupported => {}
                }
                append(
                    &mut self.report.stderr,
                    format!("Unknown script command: {command}\n").as_bytes(),
                )?;
                status = 127;
            }
        }
        Ok((Flow::Next(status), out))
    }
    fn job_command(&mut self, name: &str, args: &[String]) -> Result<(Flow, Vec<u8>), String> {
        use super::jobs::{JobId, PauseOutcome, RequestOutcome, Waited};
        let jobs = self
            .jobs
            .clone()
            .ok_or("scoped job control is unavailable in this execution environment")?;
        let uid = self.user.uid;
        let mut out = Vec::new();
        if name == "jobs" {
            if !args.is_empty() {
                return Err("Usage: jobs".into());
            }
            for job in jobs.snapshot(uid)? {
                append(
                    &mut out,
                    format!(
                        "[{}] {:?} uid {} {}\n",
                        job.id.number(),
                        job.state,
                        job.owner_uid,
                        job.label
                    )
                    .as_bytes(),
                )?;
            }
            return Ok((Flow::Next(0), out));
        }
        let (signal, text) = if name == "kill" {
            match args {
                [id] => ("-TERM", id),
                [signal, id] => (signal.as_str(), id),
                _ => return Err("Usage: kill [-STOP|-CONT|-TERM|-INT] %ID".into()),
            }
        } else {
            let [id] = args else {
                return Err(format!("Usage: {name} %ID"));
            };
            ("", id)
        };
        let id = JobId::parse(text)?;
        if name == "bg" || name == "kill" && signal == "-CONT" {
            let result = jobs.resume(uid, id)?;
            let message = if result == RequestOutcome::Finished {
                "already finished"
            } else {
                "resume requested"
            };
            append(
                &mut out,
                format!("[{}] {message}\n", id.number()).as_bytes(),
            )?;
            return Ok((Flow::Next(0), out));
        }
        if name == "kill" {
            let message = match signal {
                "-STOP" => match jobs.pause(uid, id)? {
                    PauseOutcome::Stopped => "stopped",
                    PauseOutcome::Requested => "pause requested",
                    PauseOutcome::Finished => "already finished",
                },
                "-TERM" | "-INT" => match jobs.cancel(uid, id)? {
                    RequestOutcome::Requested => "cancellation requested",
                    RequestOutcome::Finished => "already finished",
                },
                _ => return Err(
                    "only cooperative STOP/CONT/TERM/INT requests are supported; no host signals"
                        .into(),
                ),
            };
            append(
                &mut out,
                format!("[{}] {message}\n", id.number()).as_bytes(),
            )?;
            return Ok((Flow::Next(0), out));
        }
        if name == "fg" {
            jobs.resume(uid, id)?;
        }
        match jobs.wait(uid, id, self.current_job, self.worker.as_ref())? {
            Waited::Finished { report, cancelled } => {
                let status = report.status;
                append(&mut self.report.stderr, &report.stderr)?;
                for trace in report.pipelines {
                    self.record_pipeline(trace);
                }
                append(&mut out, &report.stdout)?;
                append(
                    &mut out,
                    format!(
                        "[{}] {} {}\n",
                        id.number(),
                        if cancelled { "cancelled" } else { "exited" },
                        status
                    )
                    .as_bytes(),
                )?;
                Ok((Flow::Next(status), out))
            }
            Waited::Panicked => Err("scoped job worker panicked".into()),
        }
    }

    fn call_script(
        &mut self,
        name: &str,
        args: &[String],
        input: &[u8],
        origin: InputOrigin,
    ) -> Result<(Flow, Vec<u8>), String> {
        if self.depth >= 32 {
            return Err("call depth exceeds 32".into());
        }
        let prepared = match super::prepare(self.root, self.user, self.ctx, args) {
            Ok(prepared) => prepared,
            Err(error) => {
                append(
                    &mut self.report.stderr,
                    format!("Error: {error}\n").as_bytes(),
                )?;
                return Ok((Flow::Next(2), Vec::new()));
            }
        };
        if prepared.check {
            return Ok((Flow::Next(0), Vec::new()));
        }
        let source = name != "sh";
        let saved = if source { None } else { Some(self.ctx.clone()) };
        if !source {
            self.ctx.local_frames.clear();
            self.ctx.exit_trap = None;
        }
        let mut parameters = prepared.parameters;
        if source {
            if prepared.explicit_parameters {
                parameters[0] = self.ctx.parameters.first().cloned().unwrap_or_default();
            } else {
                parameters = self.ctx.parameters.clone();
            }
        }
        let previous_parameters = std::mem::replace(&mut self.ctx.parameters, parameters);
        let stdout = std::mem::take(&mut self.report.stdout);
        let previous_stdin = std::mem::replace(&mut self.stdin, input.to_vec());
        let before = self.report.steps;
        self.depth += 1;
        // Deliberately reuse this engine: steps, call depth, stderr and modules
        // are shared. Creating a fresh runtime here would reset all those gates.
        let saved_suppression = self.suppress_errexit;
        if !source {
            self.suppress_errexit = 0;
        }
        let result = self.list(&prepared.ast);
        let result = if source {
            result
        } else {
            self.finish_trap(result)
        };
        self.suppress_errexit = saved_suppression;
        self.depth -= 1;
        let remaining = std::mem::take(&mut self.stdin);
        self.stdin = if matches!(origin, InputOrigin::Inherited) {
            remaining
        } else {
            previous_stdin
        };
        let out = std::mem::replace(&mut self.report.stdout, stdout);
        self.ctx.parameters = previous_parameters;
        if let Some(saved) = saved {
            *self.ctx = saved;
        }
        let flow = match result {
            Ok(Flow::Abort(130)) if self.cancellation_observed() => Flow::Abort(130),
            Ok(Flow::Abort(code)) if source => Flow::Abort(code),
            Ok(Flow::Abort(code)) => Flow::Next(code),
            Ok(Flow::Next(code)) => Flow::Next(code),
            Ok(Flow::Exit(code)) if source => Flow::Exit(code),
            Ok(Flow::Exit(code)) => Flow::Next(code),
            Ok(Flow::Return(code)) if source => Flow::Next(code),
            Ok(Flow::Return(_) | Flow::Break | Flow::Continue) => {
                append(
                    &mut self.report.stderr,
                    b"Error: return/break/continue outside their context\n",
                )?;
                Flow::Next(2)
            }
            Err(error) => {
                append(
                    &mut self.report.stderr,
                    format!("Error: {error}\n").as_bytes(),
                )?;
                Flow::Next(2)
            }
        };
        let status = match flow {
            Flow::Next(code) | Flow::Exit(code) | Flow::Abort(code) => code,
            _ => 2,
        };
        if status != 0 {
            append(
                &mut self.report.stderr,
                format!("Script exited with status {status}.\n").as_bytes(),
            )?;
        }
        if let Err(error) = self.modules.script_finished(
            name,
            status,
            self.report.steps - before,
            &self.report.mode,
        ) {
            append(
                &mut self.report.stderr,
                format!("Warning: log write failed: {error}\n").as_bytes(),
            )?;
        }
        Ok((flow, out))
    }

    fn test(&self, args: &[String]) -> Result<bool, String> {
        match args {
            [] => Ok(false),
            [value] => Ok(!value.is_empty()),
            [not, rest @ ..] if not == "!" => Ok(!self.test(rest)?),
            [op, value] if op == "-n" || op == "-z" => Ok(value.is_empty() != (op == "-n")),
            [op, value] if op == "-f" || op == "-d" => {
                let path = self.path(value, Access::Read)?;
                Ok(if op == "-f" {
                    self.root.read_file(&path, MAX_OUTPUT as u64).is_ok()
                } else {
                    self.root.validate_directory(&path).is_ok()
                })
            }
            [a, op, b] if op == "=" || op == "!=" => Ok((a == b) == (op == "=")),
            [a, op, b] => {
                let a = a.parse::<i64>().map_err(|_| "test requires integers")?;
                let b = b.parse::<i64>().map_err(|_| "test requires integers")?;
                match op.as_str() {
                    "-eq" => Ok(a == b),
                    "-ne" => Ok(a != b),
                    "-lt" => Ok(a < b),
                    "-le" => Ok(a <= b),
                    "-gt" => Ok(a > b),
                    "-ge" => Ok(a >= b),
                    _ => Err("unsupported test operator".into()),
                }
            }
            _ => Err("unsupported test expression".into()),
        }
    }
}

fn print_format(args: &[String]) -> Result<Vec<u8>, String> {
    struct Buffer(Vec<u8>);
    impl std::io::Write for Buffer {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            append(&mut self.0, bytes).map_err(std::io::Error::other)?;
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Buffer(Vec::new());
    super::streaming::format_to(args, &mut output).map_err(|e| e.to_string())?;
    Ok(output.0)
}

pub fn session_words(
    root: &DataRoot,
    user: &Account,
    context: &mut Context,
    words: &[Word],
    modules: &mut dyn ModuleCommands,
) -> Result<Vec<String>, String> {
    context.prepare_identity(user);
    let mut engine = Engine {
        root,
        user,
        ctx: context,
        modules,
        worker: None,
        jobs: None,
        current_job: None,
        budget: std::sync::Arc::new(Budget::new()),
        work_class: super::streaming::WorkClass::Foreground,
        substitution_status: None,
        abort_expansion: false,
        suppress_errexit: 0,
        errexit_eligible: true,
        reserved_prefix_bytes: 0,
        reserved_prefix_names: BTreeMap::new(),
        depth: 0,
        stdin: Vec::new(),
        report: Report {
            completion: Completion::Finished,
            pipelines: Vec::new(),
            status: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
            steps: 0,
            mode: ExecutionMode::InProcessBuffered {
                byte_limit: MAX_OUTPUT,
            },
        },
    };
    let mut arguments = Vec::new();
    for word in words {
        extend_arguments(&mut arguments, engine.expand(word)?)?;
    }
    Ok(arguments)
}
pub fn execute_interactive(
    list: &List,
    root: &DataRoot,
    user: &Account,
    context: &mut Context,
    modules: &mut dyn ModuleCommands,
) -> Report {
    context.prepare_identity(user);
    let parameters = context.parameters.clone();
    execute_entry(
        list,
        root,
        user,
        context,
        &parameters,
        modules,
        Entry::new(Boundary::PromptLine),
    )
}

#[derive(Clone, Copy)]
enum Boundary {
    Program,
    PromptLine,
}
struct Entry<'scope, 'env> {
    boundary: Boundary,
    jobs: Option<super::jobs::JobTable<'scope, 'env>>,
    worker: Option<super::control::Worker>,
    id: Option<super::jobs::JobId>,
    budget: std::sync::Arc<Budget>,
}
impl Entry<'_, '_> {
    fn new(boundary: Boundary) -> Self {
        Self {
            boundary,
            jobs: None,
            worker: None,
            id: None,
            budget: std::sync::Arc::new(Budget::new()),
        }
    }
}
pub struct JobRun<'scope, 'env> {
    pub jobs: super::jobs::JobTable<'scope, 'env>,
    pub worker: super::control::Worker,
    pub id: super::jobs::JobId,
    pub budget: std::sync::Arc<Budget>,
}
pub fn execute_job<'scope, 'env>(
    list: &List,
    root: &'env DataRoot,
    user: &Account,
    context: &mut Context,
    modules: &mut dyn ModuleCommands,
    job: JobRun<'scope, 'env>,
) -> Report {
    context.exit_trap = None;
    let parameters = context.parameters.clone();
    execute_entry(
        list,
        root,
        user,
        context,
        &parameters,
        modules,
        Entry {
            boundary: Boundary::Program,
            jobs: Some(job.jobs),
            worker: Some(job.worker),
            id: Some(job.id),
            budget: job.budget,
        },
    )
}
pub fn execute_in_session<'scope, 'env>(
    list: &List,
    root: &'env DataRoot,
    user: &Account,
    context: &mut Context,
    modules: &mut dyn ModuleCommands,
    jobs: super::jobs::JobTable<'scope, 'env>,
) -> Report {
    context.prepare_identity(user);
    let parameters = context.parameters.clone();
    let mut entry = Entry::new(Boundary::PromptLine);
    entry.jobs = Some(jobs);
    execute_entry(list, root, user, context, &parameters, modules, entry)
}
pub fn execute(
    list: &List,
    root: &DataRoot,
    user: &Account,
    context: &mut Context,
    parameters: &[String],
    modules: &mut dyn ModuleCommands,
) -> Report {
    execute_entry(
        list,
        root,
        user,
        context,
        parameters,
        modules,
        Entry::new(Boundary::Program),
    )
}
fn execute_entry<'scope, 'env>(
    list: &List,
    root: &'env DataRoot,
    user: &Account,
    context: &mut Context,
    parameters: &[String],
    modules: &mut dyn ModuleCommands,
    entry: Entry<'scope, 'env>,
) -> Report {
    if parameters.len() > 1024 || parameters.iter().map(String::len).sum::<usize>() > MAX_OUTPUT {
        return Report {
            completion: Completion::Finished,
            pipelines: Vec::new(),
            status: 2,
            stdout: Vec::new(),
            stderr: b"Error: script argument limit exceeded\n".to_vec(),
            steps: 0,
            mode: ExecutionMode::InProcessBuffered {
                byte_limit: MAX_OUTPUT,
            },
        };
    }
    context.parameters = parameters.to_vec();
    context
        .vars
        .entry("HOME".into())
        .or_insert_with(|| user.home.clone());
    context
        .vars
        .entry("USER".into())
        .or_insert_with(|| user.name.clone());
    let work_class = if entry.id.is_some() {
        super::streaming::WorkClass::Utility
    } else {
        super::streaming::WorkClass::Foreground
    };
    let mut engine = Engine {
        root,
        user,
        ctx: context,
        modules,
        worker: entry.worker,
        jobs: entry.jobs,
        current_job: entry.id,
        budget: entry.budget,
        work_class,
        substitution_status: None,
        abort_expansion: false,
        suppress_errexit: 0,
        errexit_eligible: true,
        reserved_prefix_bytes: 0,
        reserved_prefix_names: BTreeMap::new(),
        depth: 0,
        stdin: Vec::new(),
        report: Report {
            completion: Completion::Finished,
            pipelines: Vec::new(),
            status: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
            steps: 0,
            mode: ExecutionMode::InProcessBuffered {
                byte_limit: MAX_OUTPUT,
            },
        },
    };
    let result = engine.list(list);
    let result =
        if matches!(entry.boundary, Boundary::Program) || matches!(result, Ok(Flow::Exit(_))) {
            engine.finish_trap(result)
        } else {
            result
        };
    engine.report.status = match result {
        Ok(Flow::Next(code) | Flow::Abort(code)) => code,
        Ok(Flow::Exit(code)) => {
            engine.report.completion = Completion::ExitRequested;
            code
        }
        Ok(Flow::Return(_) | Flow::Break | Flow::Continue) => {
            let _ = append(
                &mut engine.report.stderr,
                b"Error: return/break/continue outside their context\n",
            );
            2
        }
        Err(error) => {
            let _ = append(
                &mut engine.report.stderr,
                format!("Error: {error}\n").as_bytes(),
            );
            2
        }
    };
    engine.ctx.last = engine.report.status;
    engine.report
}
