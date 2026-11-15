//! Administrative application/cache lifecycle commands; no host installer tools.
use super::modules::{CommandOutput, InputUse};
use crate::{
    filesystem::{DataRoot, ResourceKind, ResourceLimits},
    users::Account,
    virtual_path::VirtualPath,
};
use std::io::Write;
use std::writeln;
const HELP: &str =
    "app register PATH BYTES OBJECTS (new /opt or /srv directory, administrator only)
app list | app stat PATH | app quota PATH BYTES OBJECTS | app uninstall PATH
app cache APP PATH BYTES OBJECTS | app data APP PATH BYTES OBJECTS
app locations APP (registered cache/data locations; uninstall freezes and removes the group)
cache register PATH BYTES OBJECTS (new /var/cache directory, administrator only)
cache list | cache stat PATH | cache quota PATH BYTES OBJECTS
cache clear PATH (registered contents only) | cache unregister PATH
Quotas count logical payload bytes and registered files/directories, including the root.
Unknown/replaced objects are never adopted or removed; deferred cleanup resumes at startup.
";
pub(super) fn execute(
    root: &DataRoot,
    user: &Account,
    cwd: &VirtualPath,
    name: &str,
    args: &[String],
) -> Result<CommandOutput, String> {
    let kind = if name == "app" {
        ResourceKind::Application
    } else {
        ResourceKind::Cache
    };
    let path = |text: &str| {
        cwd.resolve(text)
            .map(|p| p.to_string())
            .map_err(|e| e.to_string())
    };
    let limits = |bytes: &str, objects: &str| -> Result<ResourceLimits, String> {
        Ok(ResourceLimits {
            bytes: bytes.parse().map_err(|_| "expected unsigned byte quota")?,
            objects: objects
                .parse()
                .map_err(|_| "expected unsigned object quota")?,
        })
    };
    let check_kind = |path: &str| -> Result<(), String> {
        if root
            .resource_info_as(user, path)
            .map_err(|e| e.to_string())?
            .0
            .kind
            != kind
        {
            return Err("resource kind does not match command".into());
        }
        Ok(())
    };
    let mut stdout = Vec::new();
    match args {
        [] => stdout.extend_from_slice(HELP.as_bytes()),
        [verb] if verb == "help" => stdout.extend_from_slice(HELP.as_bytes()),
        [verb] if verb == "list" => {
            for record in root
                .resources_as(user)
                .map_err(|e| e.to_string())?
                .into_iter()
                .filter(|r| r.kind == kind)
            {
                writeln!(stdout, "{record:?}").map_err(|e| e.to_string())?;
            }
        }
        [verb, app, p, bytes, objects] if name == "app" && (verb == "cache" || verb == "data") => {
            let location_kind = if verb == "cache" {
                ResourceKind::Cache
            } else {
                ResourceKind::ApplicationData
            };
            let receipt = root
                .register_application_location_as(
                    user,
                    &path(app)?,
                    &path(p)?,
                    location_kind,
                    limits(bytes, objects)?,
                )
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{receipt:?}").map_err(|e| e.to_string())?;
        }
        [verb, app] if name == "app" && verb == "locations" => {
            for record in root
                .application_locations_as(user, &path(app)?)
                .map_err(|e| e.to_string())?
            {
                writeln!(stdout, "{record:?}").map_err(|e| e.to_string())?;
            }
        }
        [verb, p, bytes, objects] if verb == "register" => {
            let receipt = root
                .register_resource_as(user, &path(p)?, kind, limits(bytes, objects)?)
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{receipt:?}").map_err(|e| e.to_string())?;
        }
        [verb, p] if verb == "stat" => {
            let path = path(p)?;
            check_kind(&path)?;
            let (record, usage) = root
                .resource_info_as(user, &path)
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{record:?}\n{usage:?}").map_err(|e| e.to_string())?;
        }
        [verb, p, bytes, objects] if verb == "quota" => {
            let path = path(p)?;
            check_kind(&path)?;
            let receipt = root
                .resource_quota_as(user, &path, limits(bytes, objects)?)
                .map_err(|e| e.to_string())?;
            writeln!(stdout, "{receipt:?}").map_err(|e| e.to_string())?;
        }
        [verb, p]
            if (name == "app" && verb == "uninstall")
                || (name == "cache" && verb == "unregister")
                || (name == "cache" && verb == "clear") =>
        {
            let path = path(p)?;
            check_kind(&path)?;
            let report = if verb == "clear" {
                root.clear_cache_as(user, &path)
            } else {
                root.remove_resource_as(user, &path)
            }
            .map_err(|e| e.to_string())?;
            writeln!(stdout, "{report:?}").map_err(|e| e.to_string())?;
            if report.deferred != 0 {
                return Ok(CommandOutput {
                    status: 1,
                    stdout,
                    stderr: b"Cleanup deferred; registration retained for recovery.\n".to_vec(),
                    input_use: InputUse::Untouched,
                });
            }
        }
        _ => return Err(HELP.into()),
    }
    Ok(CommandOutput {
        status: 0,
        stdout,
        stderr: Vec::new(),
        input_use: InputUse::Untouched,
    })
}
