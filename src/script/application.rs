//! Foreground application entry with fresh variables, descriptors and job registry.
use super::{modules::{CommandOutput, InputUse}, runtime, shared::{Resources, SharedModules}, Context};
use crate::{filesystem::DataRoot, logging::Logger, users::Account, virtual_path::VirtualPath};
use std::sync::Arc;
use std::vec;

pub(super) fn run(root: &DataRoot, caller: &Account, cwd: &VirtualPath, args: &[String]) -> Result<CommandOutput, String> {
    let [app, image, rest @ ..] = args else {
        return Err("Usage: app run APP EXECUTABLE [args...] (foreground only)".into());
    };
    let app = cwd.resolve(app).map_err(|e| e.to_string())?;
    let actor = root.application_account_as(caller, &app.to_string()).map_err(|e| e.to_string())?;
    let mut context = Context::new(VirtualPath::strict(&actor.home).map_err(|e| e.to_string())?);
    let mut executable = vec![image.clone()];
    executable.extend_from_slice(rest);
    let prepared = super::prepare_executable(root, &actor, &context, &executable).map_err(|e| e.to_string())?;
    let super::PreparedIdentity::Executed(actor) = prepared.identity else { return Err("missing application identity".into()); };
    let resources = Resources::for_application(&actor, Arc::new(Logger::open(root).map_err(|e| e.to_string())?))?;
    context.prepare_identity(&actor);
    context.set_application_parameters(prepared.parameters);
    std::thread::scope(|scope| {
        let jobs = super::jobs::JobTable::new(scope, actor.session_uid(), 16);
        let mut modules = SharedModules::new(root, &actor, &resources);
        let report = runtime::execute_application(&prepared.ast, root, &actor, &mut context, &mut modules, jobs.clone());
        let joined = jobs.shutdown();
        let cleanup = context.finish_task(&mut modules);
        joined?;
        let cleanup = cleanup?;
        if cleanup.deferred != 0 { return Err("application cleanup deferred".into()); }
        Ok(CommandOutput { status: report.status, stdout: report.stdout, stderr: report.stderr, input_use: InputUse::Untouched })
    })
}
