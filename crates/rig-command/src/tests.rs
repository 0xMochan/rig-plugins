use super::*;

// These test the security-relevant input binding, not the operating-system
// sandbox. The README describes a real containment probe for an unconfined host.
fn prepared() -> PreparedCommand {
    PreparedCommand {
        script: "printf '%s' '$HOME; $(touch should-not-exist)'".into(),
        workspace: "/workspace".into(),
        cwd: "/workspace/has spaces".into(),
        environment: BTreeMap::from([
            ("HOME".into(), "/home/example".into()),
            ("PATH".into(), "/usr/bin:/bin".into()),
        ]),
        timeout_secs: 60,
        sandbox_executable: "/opt/codex".into(),
    }
}

#[test]
fn command_and_directory_cross_as_arguments() {
    let command = prepared();
    for mode in [ExecutionMode::Sandboxed, ExecutionMode::Escalated] {
        let launch = launch_command(&command, mode, Path::new("/private/tmp/config")).unwrap();
        let args: Vec<_> = launch.get_args().collect();
        assert_eq!(args[args.len() - 1], std::ffi::OsStr::new(command.script()));
        assert_eq!(args[args.len() - 2], command.cwd().as_os_str());
        assert_eq!(args[args.len() - 3], "rig-command");
        assert_eq!(
            args[args.len() - 4],
            "cd -- \"$1\" && exec /bin/bash --noprofile --norc -c \"$2\""
        );
    }
}

#[test]
fn sandbox_authority_and_child_environment_are_explicit() {
    let launch =
        launch_command(&prepared(), ExecutionMode::Sandboxed, Path::new("/config")).unwrap();
    let args: Vec<_> = launch.get_args().map(|arg| arg.to_str().unwrap()).collect();
    assert_eq!(launch.get_program(), "/opt/codex");
    assert_eq!(launch.get_envs().count(), 3);
    for setting in [
        "shell_environment_policy.inherit=\"none\"",
        "shell_environment_policy.include_only=[\"HOME\",\"PATH\"]",
    ] {
        assert!(args.windows(2).any(|pair| pair == ["-c", setting]));
    }
    let profile_name = args.windows(2).find(|pair| pair[0] == "-P").unwrap()[1];
    assert!(profile_name.starts_with("rig_command_"));
    assert_eq!(profile_name.len(), "rig_command_".len() + 32);
    let profile = args
        .windows(2)
        .filter(|pair| pair[0] == "-c")
        .map(|pair| pair[1])
        .find(|value| value.starts_with("permissions."))
        .unwrap();
    assert_eq!(
        profile,
        format!(
            "permissions.{profile_name}={{filesystem={{\":root\"=\"read\",\"/workspace\"={{\".\"=\"write\",\".git\"=\"read\",\".agents\"=\"read\",\".codex\"=\"read\",\".aws\"=\"read\"}}}},network={{enabled=false}}}}"
        )
    );
    let next = launch_command(&prepared(), ExecutionMode::Sandboxed, Path::new("/config")).unwrap();
    let next_args: Vec<_> = next.get_args().map(|arg| arg.to_str().unwrap()).collect();
    let next_profile = next_args.windows(2).find(|pair| pair[0] == "-P").unwrap()[1];
    assert_ne!(
        profile_name, next_profile,
        "ambient profiles must not share a predictable policy key"
    );
}

#[test]
fn review_snapshot_changes_with_execution_input() {
    let command = prepared();
    let mut changed = command.clone();
    changed.environment.insert("PATH".into(), "/other".into());
    assert_ne!(changed, command);
    assert_ne!(
        serde_json::to_value(&changed).unwrap(),
        serde_json::to_value(&command).unwrap()
    );
    changed = command.clone();
    changed.script.push_str("; another-action");
    assert_ne!(changed, command);
}
