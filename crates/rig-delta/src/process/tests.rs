use super::*;

fn request(directory: &std::path::Path, script: &str, lifetime: Lifetime) -> Request {
    Request {
        executable: PathBuf::from("/bin/sh").canonicalize().unwrap(),
        args: vec!["-c".into(), script.into()],
        cwd: directory.canonicalize().unwrap(),
        environment: BTreeMap::from([("PATH".into(), "/usr/bin:/bin".into())]),
        stdin: None,
        lifetime,
        shutdown_grace: Duration::from_millis(100),
    }
}

fn session(directory: &std::path::Path, script: &str) -> Process {
    Process::start(
        request(
            directory,
            script,
            Lifetime::Session {
                startup_timeout: Duration::from_secs(1),
            },
        ),
        Wake::default(),
    )
    .unwrap()
}

fn next(process: &mut Process, timeout: Duration) -> Event {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(event) = process.try_event() {
            return event;
        }
        assert!(
            Instant::now() < deadline,
            "Delta worker did not yield before the test deadline"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn terminal(process: &mut Process) -> (Vec<String>, Event) {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut lines = Vec::new();
    loop {
        assert!(
            Instant::now() < deadline,
            "Delta worker failed to terminate"
        );
        match next(process, Duration::from_secs(5)) {
            Event::Line(line) => lines.push(line),
            terminal => return (lines, terminal),
        }
    }
}

#[test]
fn incremental_output_stays_separate_and_term_flush_finishes_before_exit() {
    let directory = tempfile::tempdir().unwrap();
    let mut process = session(
        directory.path(),
        "trap 'printf \"flushed\\n\"; exit 0' TERM; printf 'ready\\n'; printf 'diagnostic' >&2; while :; do sleep 0.05; done",
    );
    assert!(
        matches!(next(&mut process, Duration::from_secs(2)), Event::Line(line) if line == "ready")
    );
    assert!(!process.is_finished());
    process.mark_ready();
    process.request_stop();
    let (lines, exit) = terminal(&mut process);
    assert_eq!(lines, ["flushed"]);
    match exit {
        Event::Exited {
            code,
            diagnostics,
            stopped,
        } => {
            assert_eq!(code, Some(0));
            assert!(stopped);
            assert!(diagnostics.contains("diagnostic"));
            assert!(!diagnostics.contains("ready"));
        }
        other => panic!("expected graceful exit, got {other:?}"),
    }
    assert!(process.is_finished());
    assert!(process.try_event().is_none());
}

#[test]
fn approved_one_shot_stdin_and_final_unterminated_line_are_preserved() {
    let directory = tempfile::tempdir().unwrap();
    let mut request = request(
        directory.path(),
        "cat; printf 'stderr only' >&2",
        Lifetime::Command {
            timeout: Duration::from_secs(3),
        },
    );
    request.stdin = Some("first\nλ\nlast".as_bytes().to_vec());
    let mut process = Process::start(request, Wake::default()).unwrap();
    let (lines, exit) = terminal(&mut process);
    assert_eq!(lines, ["first", "λ", "last"]);
    assert!(
        matches!(exit, Event::Exited { code: Some(0), diagnostics, stopped: false } if diagnostics == "stderr only")
    );
}

#[test]
fn output_overflow_stops_instead_of_silently_losing_watch_frames() {
    let directory = tempfile::tempdir().unwrap();
    let mut huge_line = session(
        directory.path(),
        "dd if=/dev/zero bs=65536 count=20 2>/dev/null; sleep 10",
    );
    let (lines, failure) = terminal(&mut huge_line);
    assert!(lines.is_empty());
    assert!(matches!(failure, Event::Failed { error, .. } if error.contains("line exceeds 1 MiB")));

    let mut full_queue = session(
        directory.path(),
        "i=0; while [ \"$i\" -lt 1024 ]; do printf 'frame\\n'; i=$((i+1)); done; sleep 10",
    );
    // The protocol consumer is deliberately absent while the producer fills it.
    std::thread::sleep(Duration::from_millis(200));
    let (lines, failure) = terminal(&mut full_queue);
    assert_eq!(lines, vec!["frame"; MAX_QUEUED_LINES]);
    assert!(
        matches!(failure, Event::Failed { error, .. } if error.contains("queue exceeds 128 lines"))
    );
}

#[test]
fn startup_requires_validation_and_blocked_stdin_cannot_hide_command_deadline() {
    let directory = tempfile::tempdir().unwrap();
    let mut not_ready = session(
        directory.path(),
        "trap '' TERM; printf 'not a readiness frame\\n'; sleep 10",
    );
    let (lines, failure) = terminal(&mut not_ready);
    assert_eq!(lines, ["not a readiness frame"]);
    assert!(
        matches!(failure, Event::Failed { error, .. } if error.contains("before validated readiness"))
    );

    let mut blocked = request(
        directory.path(),
        "trap '' TERM; sleep 10",
        Lifetime::Command {
            timeout: Duration::from_secs(1),
        },
    );
    blocked.stdin = Some(vec![b'x'; MAX_STDIN_BYTES]);
    let mut blocked = Process::start(blocked, Wake::default()).unwrap();
    let (_, failure) = terminal(&mut blocked);
    assert!(
        matches!(failure, Event::Failed { error, .. } if error.contains("command deadline expired"))
    );
}

#[test]
fn ignoring_term_is_forced_and_dropping_owner_stops_its_group() {
    let directory = tempfile::tempdir().unwrap();
    let mut ignores_term = session(
        directory.path(),
        "trap '' TERM; printf '%s\\n' $$; while :; do sleep 0.1; done",
    );
    let leader = match next(&mut ignores_term, Duration::from_secs(2)) {
        Event::Line(pid) => Pid::from_raw(pid.parse().unwrap()).unwrap(),
        other => panic!("expected leader identity, got {other:?}"),
    };
    ignores_term.mark_ready();
    let stopped = Instant::now();
    ignores_term.request_stop();
    let (_, exit) = terminal(&mut ignores_term);
    assert!(stopped.elapsed() >= Duration::from_millis(80));
    assert!(matches!(
        exit,
        Event::Exited {
            code: None,
            stopped: true,
            ..
        }
    ));
    assert_eq!(
        waitid(
            WaitId::Pid(leader),
            WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT
        )
        .unwrap_err(),
        rustix::io::Errno::CHILD
    );

    let mut dropped = session(
        directory.path(),
        "(sleep 1; printf orphan > child-survived) & printf '%s\\n' $$; while :; do sleep 0.1; done",
    );
    let leader = match next(&mut dropped, Duration::from_secs(2)) {
        Event::Line(pid) => Pid::from_raw(pid.parse().unwrap()).unwrap(),
        other => panic!("expected leader identity, got {other:?}"),
    };
    dropped.mark_ready();
    drop(dropped);
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if rustix::process::test_kill_process(leader).is_err() {
            break;
        }
        assert!(Instant::now() < deadline, "dropped process was not reaped");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_millis(1100));
    assert!(
        !directory.path().join("child-survived").exists(),
        "same-group descendant outlived its owner"
    );
}

#[test]
fn session_stdin_is_structurally_rejected_before_any_process_starts() {
    let directory = tempfile::tempdir().unwrap();
    let mut forbidden = request(
        directory.path(),
        "touch should-not-exist",
        Lifetime::Session {
            startup_timeout: Duration::from_secs(1),
        },
    );
    forbidden.stdin = Some(Vec::new());
    assert!(
        Process::start(forbidden, Wake::default())
            .err()
            .unwrap()
            .contains("cannot have stdin")
    );
    assert!(!directory.path().join("should-not-exist").exists());
}
