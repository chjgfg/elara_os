//! Tests use the same public library as the actual elara-host binary.
#[path = "script/jobs.rs"]
mod job_tests;
#[path = "script/streaming.rs"]
mod stream_tests;
#[path = "script/pipe.rs"]
mod pipe_tests;
#[path = "script/case.rs"]
mod case_tests;
#[path = "script/declarations.rs"]
mod declaration_tests;
#[path = "script/expansions.rs"]
mod expansion_tests;
#[path = "script/fd.rs"]
mod fd_tests;
#[path = "script/fs_commands.rs"]
mod fs_command_tests;
#[path = "script/globbing.rs"]
mod glob_tests;
#[path = "script/interactive.rs"]
mod interactive_tests;
#[path = "script/lexer.rs"]
mod lexer_tests;
#[path = "script/nested.rs"]
mod nested_tests;
#[path = "script/parser.rs"]
mod parser_tests;
#[path = "script/runtime.rs"]
mod runtime_tests;
#[path = "script/shell.rs"]
mod shell_tests;
#[path = "script/support.rs"]
mod support;
