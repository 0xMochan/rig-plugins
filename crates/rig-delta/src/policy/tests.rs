use super::*;
use serde_json::json;
use std::fs;

fn places() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let temporary = tempfile::tempdir().unwrap();
    let home = temporary.path().join("rig-home");
    let root = temporary.path().join("checkout");
    fs::create_dir(&home).unwrap();
    fs::create_dir(&root).unwrap();
    (temporary, home, root)
}

fn write_project(root: &Path, document: &Value) {
    fs::create_dir_all(root.join(".delta")).unwrap();
    fs::write(
        root.join(".delta/settings.json"),
        serde_json::to_vec(document).unwrap(),
    )
    .unwrap();
}

fn automatic(mode: Mode, organization: &str) -> Policy {
    Policy {
        mode,
        organization: Some(organization.into()),
        ..Policy::default()
    }
}

#[test]
fn repository_policy_is_configuration_not_automatic_consent() {
    let (_temporary, home, root) = places();
    assert!(!read(&root).configured);
    assert!(!effective(&home, &root).policy.records_automatically());
    write_project(
        &root,
        &json!({"threads":{"mode":"organization","organization":"team-1","reasoning":true,"media":true}}),
    );
    let file = read(&root);
    assert!(file.configured);
    assert_eq!(file.policy.mode, Mode::Organization);
    assert!(!file.policy.reasoning);
    assert!(!file.policy.media);
    let resolved = effective(&home, &root);
    assert_eq!(resolved.policy.mode, Mode::Manual);
    assert!(!resolved.trusted);
    assert!(resolved.consent_audit.is_none());
    assert!(
        !home.join("delta").exists(),
        "inspection must not create consent state"
    );
    let workspace_home = root.join("local-rig-home");
    fs::create_dir(&workspace_home).unwrap();
    let plan = prepare(&root, &automatic(Mode::Private, "team-1")).unwrap();
    assert!(save_confirmed(&workspace_home, &plan, "person selected automatic mode").is_err());
    assert_eq!(effective(&workspace_home, &root).policy.mode, Mode::Manual);
    assert!(
        !workspace_home.join("delta").exists(),
        "workspace-writable state cannot become trusted consent"
    );

    for invalid in [
        json!({"threads":{"mode":"organization"}}),
        json!({"threads":{"mode":"automatic","organization":"team-1"}}),
        json!({"threads":{"mode":"private","organization":42}}),
        json!({"threads":"private"}),
    ] {
        write_project(&root, &invalid);
        assert!(read(&root).problem.is_some());
        assert_eq!(effective(&home, &root).policy.mode, Mode::Manual);
    }
    fs::write(root.join(".delta/settings.json"), "not JSON").unwrap();
    assert!(read(&root).problem.is_some());
    assert!(prepare(&root, &automatic(Mode::Private, "team-1")).is_err());
    assert_eq!(
        fs::read_to_string(root.join(".delta/settings.json")).unwrap(),
        "not JSON"
    );
}

#[test]
fn confirmed_setup_preserves_unknown_keys_and_binds_exports_and_audit_locally() {
    let (_temporary, home, root) = places();
    write_project(
        &root,
        &json!({"future":{"enabled":true},"threads":{"mode":"manual","unknown":[1,2],"export":{"media":true}}}),
    );
    let policy = Policy {
        mode: Mode::Private,
        organization: Some("team-1".into()),
        reasoning: true,
        media: false,
    };
    let plan = prepare(&root, &policy).unwrap();
    assert_eq!(plan.root(), root.canonicalize().unwrap());
    assert_eq!(plan.policy(), &policy);
    assert_eq!(plan.document()["threads"]["unknown"], json!([1, 2]));
    let audit = r#"{"decider":"person","executable":"/installed/Delta","connection_scope":"this-checkout"}"#;
    save_confirmed(&home, &plan, audit).unwrap();
    let document: Value =
        serde_json::from_slice(&fs::read(root.join(".delta/settings.json")).unwrap()).unwrap();
    assert_eq!(document["future"]["enabled"], true);
    assert_eq!(document["threads"]["unknown"], json!([1, 2]));
    assert_eq!(
        document["threads"]["export"]["media"], true,
        "unknown project fields are preserved, not promoted into consent"
    );
    assert_eq!(document["threads"]["mode"], "private");
    let resolved = effective(&home, &root);
    assert!(resolved.trusted);
    assert_eq!(resolved.policy, policy);
    assert_eq!(resolved.consent_audit.as_deref(), Some(audit));
    assert!(read(&root).configured);
    assert!(
        !read(&root).policy.reasoning,
        "project inspection never reconstructs private export consent"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(home.join("delta"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        assert_eq!(
            fs::metadata(home.join("delta/policies.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
        assert_eq!(
            fs::metadata(root.join(".delta/settings.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o077,
            0
        );
    }
}

#[test]
fn any_changed_document_or_revocation_disables_automatic_recording() {
    let (_temporary, home, root) = places();
    let plan = prepare(&root, &automatic(Mode::Private, "team-1")).unwrap();
    save_confirmed(&home, &plan, "native person confirmed private team-1").unwrap();
    let original: Value =
        serde_json::from_slice(&fs::read(root.join(".delta/settings.json")).unwrap()).unwrap();
    for changed in [
        json!({"threads":{"mode":"private","organization":"team-2"}}),
        json!({"threads":{"mode":"organization","organization":"team-1"}}),
        json!({"threads":{"mode":"private","organization":"team-1","new_unknown_key":true}}),
    ] {
        write_project(&root, &changed);
        let effective = effective(&home, &root);
        assert_eq!(effective.policy.mode, Mode::Manual);
        assert!(effective.problem.is_some());
        assert!(effective.consent_audit.is_none());
    }
    write_project(&root, &original);
    assert!(effective(&home, &root).trusted);
    let before = fs::read(root.join(".delta/settings.json")).unwrap();
    revoke(&home, &root).unwrap();
    assert_eq!(fs::read(root.join(".delta/settings.json")).unwrap(), before);
    assert_eq!(effective(&home, &root).policy.mode, Mode::Manual);

    let plan = prepare(&root, &automatic(Mode::Private, "team-1")).unwrap();
    save_confirmed(&home, &plan, "person confirmed again").unwrap();
    let manual = prepare(&root, &Policy::default()).unwrap();
    save_confirmed(&home, &manual, "person disabled automatic recording").unwrap();
    assert_eq!(read(&root).policy.mode, Mode::Manual);
    assert!(!effective(&home, &root).trusted);
}

#[test]
fn stale_plans_and_replaced_checkouts_cannot_reuse_consent() {
    let (temporary, home, root) = places();
    let plan = prepare(&root, &automatic(Mode::Private, "team-1")).unwrap();
    write_project(
        &root,
        &json!({"threads":{"mode":"manual"},"changed_while_asking":true}),
    );
    assert!(save_confirmed(&home, &plan, "person answered an obsolete question").is_err());
    assert!(!home.join("delta").exists());
    let plan = prepare(&root, &automatic(Mode::Private, "team-1")).unwrap();
    save_confirmed(&home, &plan, "person confirmed current question").unwrap();
    let document: Value =
        serde_json::from_slice(&fs::read(root.join(".delta/settings.json")).unwrap()).unwrap();
    let still_current = prepare(&root, &automatic(Mode::Private, "team-1")).unwrap();
    let old = temporary.path().join("old-checkout");
    fs::rename(&root, &old).unwrap();
    fs::create_dir(&root).unwrap();
    write_project(&root, &document);
    assert!(
        save_confirmed(
            &home,
            &still_current,
            "person answered for the replaced checkout"
        )
        .is_err()
    );
    assert_eq!(effective(&home, &root).policy.mode, Mode::Manual);
    assert!(
        effective(&home, &root)
            .problem
            .unwrap()
            .contains("identity changed")
    );
}

#[test]
fn locked_corrupt_symlinked_or_oversized_state_never_enables_or_overwrites() {
    let (_temporary, home, root) = places();
    let policy = automatic(Mode::Private, "team-1");
    let plan = prepare(&root, &policy).unwrap();
    assert!(save_confirmed(&home, &plan, " ").is_err());
    save_confirmed(&home, &plan, "person approved exact native setup").unwrap();
    let before = fs::read(root.join(".delta/settings.json")).unwrap();
    let plan = prepare(&root, &automatic(Mode::Organization, "team-2")).unwrap();
    fs::write(home.join("delta/policies.lock"), "other process").unwrap();
    assert!(save_confirmed(&home, &plan, "person selected team-2").is_err());
    assert_eq!(fs::read(root.join(".delta/settings.json")).unwrap(), before);
    fs::remove_file(home.join("delta/policies.lock")).unwrap();
    let state = home.join("delta/policies.json");
    fs::write(&state, "invalid trusted state").unwrap();
    assert_eq!(effective(&home, &root).policy.mode, Mode::Manual);
    assert!(save_confirmed(&home, &plan, "person selected team-2").is_err());
    assert_eq!(fs::read(root.join(".delta/settings.json")).unwrap(), before);
    fs::write(&state, "x".repeat(MAX_BYTES as usize + 1)).unwrap();
    assert_eq!(effective(&home, &root).policy.mode, Mode::Manual);
    #[cfg(unix)]
    {
        let target = home.join("original-policy-state");
        fs::rename(&state, &target).unwrap();
        std::os::unix::fs::symlink(&target, &state).unwrap();
        assert_eq!(effective(&home, &root).policy.mode, Mode::Manual);
        assert!(save_confirmed(&home, &plan, "person selected team-2").is_err());
        let settings = root.join(".delta/settings.json");
        let settings_target = root.join("settings-original.json");
        fs::rename(&settings, &settings_target).unwrap();
        std::os::unix::fs::symlink(&settings_target, &settings).unwrap();
        assert!(read(&root).problem.unwrap().contains("non-symlink"));
        assert!(prepare(&root, &policy).is_err());
    }
}

#[cfg(unix)]
#[test]
fn pinned_settings_parent_cannot_redirect_a_transaction_outside_the_checkout() {
    let (temporary, home, root) = places();
    write_project(&root, &json!({"threads":{"mode":"manual"}}));
    let plan = prepare(&root, &automatic(Mode::Private, "team-1")).unwrap();
    let pinned_settings = project_directory(&plan.root_fd, false).unwrap().unwrap();
    let outside = temporary.path().join("outside");
    fs::create_dir(&outside).unwrap();
    let sentinel = b"unrelated private file";
    fs::write(outside.join("settings.json"), sentinel).unwrap();

    // Deterministic equivalent of replacing the parent after it was opened:
    // the ordinary API refuses it, and the transaction primitive remains on
    // the original directory descriptor rather than following the new link.
    let original = root.join(".delta-reviewed");
    fs::rename(root.join(".delta"), &original).unwrap();
    std::os::unix::fs::symlink(&outside, root.join(".delta")).unwrap();
    assert!(save_confirmed(&home, &plan, "person approved the old settings parent").is_err());
    assert!(read(&root).problem.is_some());
    assert!(prepare(&root, &automatic(Mode::Private, "team-1")).is_err());
    write_json_at(&pinned_settings, "settings.json", plan.document()).unwrap();
    assert_eq!(fs::read(outside.join("settings.json")).unwrap(), sentinel);
    let saved: Value =
        serde_json::from_slice(&fs::read(original.join("settings.json")).unwrap()).unwrap();
    assert_eq!(saved["threads"]["mode"], "private");
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        1,
        "no transaction file may be created through the replacement link"
    );

    // A target-name link inserted after validation is atomically replaced at
    // the pinned parent; renameat never writes through that target either.
    fs::remove_file(original.join("settings.json")).unwrap();
    std::os::unix::fs::symlink(
        outside.join("settings.json"),
        original.join("settings.json"),
    )
    .unwrap();
    write_json_at(&pinned_settings, "settings.json", plan.document()).unwrap();
    assert!(
        !fs::symlink_metadata(original.join("settings.json"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(fs::read(outside.join("settings.json")).unwrap(), sentinel);
    assert!(!effective(&home, &root).trusted);
}
