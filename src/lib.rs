//! ElaraOS modules shared by the real binary and external integration tests.
#![forbid(unsafe_code)]
pub mod command_line;
pub mod config;
pub mod filesystem;
pub mod initialization;
pub mod logging;
pub mod permissions;
pub mod script;
mod sha256;
pub mod shell;
pub mod tasks;
mod terminal;
pub mod users;
pub mod virtual_path;
#[cfg(test)]
mod test_support;
