//! Codex 0.142.0's concrete, per-launch permission profile.
//!
//! Explicit `-P :workspace` ignores legacy `sandbox_workspace_write` settings in
//! this release, including its temporary-write exclusions. Construct the actual
//! filesystem policy instead. Do not add `extends = ":workspace"`: that would
//! restore the temporary-directory grants we are excluding.

use std::ffi::OsString;
use std::fs::File;
use std::io::Read;

use crate::{CommandError, PreparedCommand};

pub(super) fn sandbox_args(command: &PreparedCommand) -> Result<Vec<OsString>, CommandError> {
    if command.workspace().to_str().is_none() {
        return Err(CommandError::Invalid(
            "Codex permission profiles require a UTF-8 workspace path".to_owned(),
        ));
    }
    let mut nonce = [0; 16];
    File::open("/dev/urandom")?.read_exact(&mut nonce)?;
    let name = format!("rig_command_{:032x}", u128::from_be_bytes(nonce));
    Ok(profile_args(command, &name))
}

fn profile_args(command: &PreparedCommand, name: &str) -> Vec<OsString> {
    // A scalar path key interprets brackets, stars and question marks as globs.
    // Scoped entries compile the base as a literal path in Codex 0.142.0.
    let workspace = command
        .workspace()
        .to_str()
        .expect("prepared workspace is UTF-8");
    let profile = format!(
        "permissions.{name}={{filesystem={{\":root\"=\"read\",{}={{\".\"=\"write\",\".git\"=\"read\",\".agents\"=\"read\",\".codex\"=\"read\",\".aws\"=\"read\"}}}},network={{enabled=false}}}}",
        toml_string(workspace)
    );
    let mut args = vec![
        "sandbox".into(),
        "-P".into(),
        name.into(),
        "-C".into(),
        command.workspace().as_os_str().to_owned(),
    ];
    config_override(&mut args, profile);
    config_override(
        &mut args,
        "shell_environment_policy.inherit=\"none\"".into(),
    );
    config_override(
        &mut args,
        "shell_environment_policy.ignore_default_excludes=true".into(),
    );
    config_override(&mut args, "shell_environment_policy.exclude=[]".into());
    // CLI config tables merge recursively. Even a complete profile table with
    // a stable name could inherit extra ambient filesystem grants or `extends`.
    // A fresh 128-bit name makes this table independent of static user/project
    // config. It has no parent, symbolic roots, tmp grants or network grants.
    // The environment map needs a second defense: filter to the captured keys
    // and overwrite every allowed value, excluding ambient `set` entries.
    let keys: Vec<_> = command.environment().keys().collect();
    config_override(
        &mut args,
        format!(
            "shell_environment_policy.include_only={}",
            serde_json::to_string(&keys).expect("string arrays serialize")
        ),
    );
    for (key, value) in command.environment() {
        config_override(
            &mut args,
            format!("shell_environment_policy.set.{key}={}", toml_string(value)),
        );
    }
    args
}

fn toml_string(value: &str) -> String {
    serde_json::to_string(value).expect("strings serialize")
}

fn config_override(args: &mut Vec<OsString>, value: String) {
    args.push("-c".into());
    args.push(value.into());
}
