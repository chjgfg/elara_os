#![forbid(unsafe_code)]

use elara_os::{config, filesystem, initialization, shell};

use std::io::{self, IsTerminal};
use std::path::PathBuf;
use std::{env, eprintln, println, process};

fn main() {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    let configuration = match arguments.as_slice() {
        [] => PathBuf::from(".env"),
        [path] => PathBuf::from(path),
        _ => {
            eprintln!("Usage: elara-host [configuration-file]");
            process::exit(2);
        }
    };
    let startup = config::Config::load(&configuration).and_then(|config| {
        let state = initialization::prepare(&config.data_root).map_err(|e| e.to_string())?;
        let root = filesystem::DataRoot::open(&config.data_root).map_err(|e| e.to_string())?;
        Ok((root, state))
    });
    let (data_root, state) = match startup {
        Ok(root) => root,
        Err(error) => {
            eprintln!("Startup refused: {error}");
            process::exit(2);
        }
    };
    println!("Initialization: {state}");
    let stdin = io::stdin();
    let stdout = io::stdout();
    let interactive = stdin.is_terminal() && stdout.is_terminal();
    match shell::run_status(
        &mut stdin.lock(),
        &mut stdout.lock(),
        interactive,
        &data_root,
    ) {
        Ok(status) => process::exit(i32::from(status)),
        Err(error) => {
            eprintln!("Console I/O error: {error}");
            process::exit(1);
        }
    }
}
