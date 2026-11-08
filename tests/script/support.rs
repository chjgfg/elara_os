use elara_os::filesystem::DataRoot;
use elara_os::logging::Logger;
use elara_os::script::{self, modules, runtime, Context};
use elara_os::tasks::TaskTable;
use elara_os::users::{self, Account};
use elara_os::virtual_path::VirtualPath;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use std::{assert, env, format};
static NEXT: AtomicU64 = AtomicU64::new(0);

pub fn fixture() -> (DataRoot, Account) {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target/test-data")
        .join(format!(
            "script-api-{}-{nonce}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
    assert!(!path.exists());
    elara_os::initialization::prepare(&path).unwrap();
    let root = DataRoot::open(&path).unwrap();
    users::ensure_passwd(&root).unwrap();
    let accounts = users::load(&root).unwrap();
    let user = users::lookup(&accounts, "root").unwrap().clone();
    (root, user)
}
pub fn run(
    root: &DataRoot,
    user: &Account,
    context: &mut Context,
    args: &[String],
) -> Result<runtime::Report, script::lexer::Error> {
    let mut tasks = TaskTable::new(16);
    let logger = Logger::open(root).unwrap();
    let mut files = script::fd::FdSession::new(user.uid).unwrap();
    let mut modules = modules::SystemModules {
        root,
        user,
        tasks: &mut tasks,
        logger: &logger,
        files: &mut files,
    };
    script::run_with_modules(root, user, context, args, &mut modules)
}
pub fn execute(source: &str) -> runtime::Report {
    let (root, user) = fixture();
    let mut context = Context::new(VirtualPath::root());
    run(&root, &user, &mut context, &["-c".into(), source.into()]).unwrap()
}
pub fn transcript_in(root: &DataRoot, commands: &str) -> String {
    let mut input = Cursor::new(format!("\n{commands}").into_bytes());
    let mut output = Vec::new();
    elara_os::shell::run(&mut input, &mut output, false, root).unwrap();
    String::from_utf8(output).unwrap()
}
pub fn transcript(commands: &str) -> String {
    let (root, _) = fixture();
    transcript_in(&root, commands)
}
