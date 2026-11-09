use super::*;

fn request(script: impl Into<String>) -> Request {
    Request {
        script: script.into(),
        files: Vec::new(),
    }
}

#[tokio::test]
async fn transforms_explicit_inputs_without_retaining_state() {
    let first = Request {
        script: "cat data/items.txt | sort | uniq; printf saved > /tmp/saved".to_owned(),
        files: vec![InputFile {
            path: "data/items.txt".to_owned(),
            content: "pear\napple\npear\n".to_owned(),
        }],
    };
    let output = execute(&first).await.unwrap();
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, "apple\npear\n");
    let next = execute(&request("test ! -e /tmp/saved && test ! -e data/items.txt"))
        .await
        .unwrap();
    assert_eq!(next.exit_code, 0);
}

#[tokio::test]
async fn has_no_host_files_environment_or_native_executables() {
    // The test executable exists on the host, but is not supplied as input.
    let host_file = std::env::current_exe().unwrap();
    let input = Request {
        script: "test ! -e \"$(cat probe)\" && test \"$HOME\" = /home/user && printf isolated; \
                 \"$(cat probe)\""
            .to_owned(),
        files: vec![InputFile {
            path: "probe".to_owned(),
            content: host_file.to_string_lossy().into_owned(),
        }],
    };
    let output = execute(&input).await.unwrap();
    assert_eq!(output.stdout, "isolated");
    assert_eq!(output.exit_code, 127);
    // A native-only command is never retried through the real shell.
    let native = execute(&request("cargo --version")).await.unwrap();
    assert_eq!(native.exit_code, 127);
}

#[tokio::test]
async fn enforces_host_chosen_input_work_and_output_limits() {
    let oversized = request(" ".repeat(MAX_SCRIPT_BYTES + 1));
    assert!(execute(&oversized).await.is_err());
    let invalid_path = Request {
        script: "true".to_owned(),
        files: vec![InputFile {
            path: "../outside".to_owned(),
            content: String::new(),
        }],
    };
    assert!(execute(&invalid_path).await.is_err());
    let work = execute(&request("for ((i=0;i<2000;i++)); do :; done")).await;
    assert!(work.is_err());
    // Each printf width stays within Bashkit's 10,000-character limit, while
    // the successful commands together emit 70,000 bytes into the capture.
    let output = execute(&request(
        "for i in 1 2 3 4 5 6 7; do printf '%10000s' x; done",
    ))
    .await
    .unwrap();
    assert_eq!(output.exit_code, 0, "{}", output.stderr);
    assert!(output.stderr.is_empty());
    assert!(output.stdout_truncated);
    assert_eq!(output.stdout.len(), MAX_OUTPUT_BYTES);
}
