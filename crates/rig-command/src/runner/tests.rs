use super::*;

fn run_script(script: &str, timeout_secs: u64, cancelled: &AtomicBool) -> CommandOutput {
    let mut command = Command::new("/bin/bash");
    command
        .env_clear()
        .args(["--noprofile", "--norc", "-c", script]);
    collect_output(spawn(command).unwrap(), timeout_secs, cancelled).unwrap()
}

#[test]
fn output_is_bounded_while_both_pipes_and_invalid_utf8_are_drained() {
    // More than a pipe buffer on each stream, with invalid UTF-8 and no newlines.
    // A collector that stops reading at its cap would deadlock this command.
    let output = run_script(
        "printf '\\377'; /usr/bin/head -c 2000000 /dev/zero; /usr/bin/head -c 2000000 /dev/zero >&2",
        5,
        &AtomicBool::new(false),
    );
    assert_eq!(output.status, CommandStatus::Exited(0));
    assert!(output.truncated);
    assert!(output.output.len() <= OUTPUT_BYTES);
}

#[test]
fn deadline_and_cancellation_stop_an_uncooperative_child() {
    let started = Instant::now();
    let output = run_script("trap '' TERM; /bin/sleep 60", 1, &AtomicBool::new(false));
    assert_eq!(output.status, CommandStatus::TimedOut);
    assert!(started.elapsed() < Duration::from_secs(5));

    let output = run_script("/bin/sleep 60", 60, &AtomicBool::new(true));
    assert_eq!(output.status, CommandStatus::Stopped);
}

#[tokio::test]
async fn dropping_wait_cancels_the_owned_group() {
    let directory = tempfile::tempdir().unwrap();
    let pid_file = directory.path().join("child.pid");
    let mut command = Command::new("/bin/bash");
    command
        .env_clear()
        .args([
            "--noprofile",
            "--norc",
            "-c",
            "echo $$ > \"$1\"; exec /bin/sleep 60",
            "test",
        ])
        .arg(&pid_file);
    let running = RunningCommand::start(command, tempfile::tempdir().unwrap(), 60).unwrap();
    let mut waiting = Box::pin(running.wait());
    tokio::select! {
        result = &mut waiting => panic!("command unexpectedly finished: {result:?}"),
        _ = tokio::time::sleep(Duration::from_millis(100)) => {}
    }
    let leader: i32 = std::fs::read_to_string(&pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let leader = Pid::from_raw(leader).unwrap();
    drop(waiting);
    // Wait only for cleanup, never for the original minute-long command.
    let limit = Instant::now() + Duration::from_secs(2);
    loop {
        if rustix::process::test_kill_process(leader).is_err() {
            break;
        }
        assert!(Instant::now() < limit, "cancelled leader was not reaped");
        tokio::time::sleep(PROCESS_POLL).await;
    }
}
