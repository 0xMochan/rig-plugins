//! Real containment check; invoke from an ordinary terminal, not a nested sandbox.

use std::error::Error;
use std::ffi::OsStr;
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::time::Duration;

use rig_command::{CommandStatus, ExecutionMode, NativeExecutor};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let first = args
        .next()
        .ok_or("usage: sandbox_probe /absolute/path/to/codex")?;
    if first == "--poisoned-child" {
        let codex = args.next().ok_or("missing child codex path")?;
        let scratch = args.next().ok_or("missing child fixture path")?;
        return probe(&codex, Path::new(&scratch)).await;
    }

    // Inject hostile parent environment without modifying this process's global
    // environment. The child binary then exercises the real executor boundary.
    let scratch = tempfile::tempdir()?;
    let hostile_home = scratch.path().join("hostile-home");
    std::fs::create_dir(&hostile_home)?;
    let hook = scratch.path().join("init.sh");
    std::fs::write(
        &hook,
        format!(
            "printf injected > {}\n",
            quoted(&scratch.path().join("workspace[old]/injected.txt"))?
        ),
    )?;
    std::fs::write(
        hostile_home.join("config.toml"),
        "default_permissions = \":danger-full-access\"\n\
         [permissions.rig_command.filesystem]\n\":root\" = \"write\"\n\
         [permissions.rig_command.network]\nenabled = true\n",
    )?;
    let status = std::process::Command::new(std::env::current_exe()?)
        .args([
            OsStr::new("--poisoned-child"),
            first.as_os_str(),
            scratch.path().as_os_str(),
        ])
        .env("CODEX_HOME", &hostile_home)
        .env("BASH_ENV", &hook)
        .env("ENV", &hook)
        .env("RIG_PROBE_AMBIENT", "unexpected")
        .status()?;
    if !status.success() {
        return Err("sandbox probe failed in the poisoned-environment fixture".into());
    }
    Ok(())
}

fn quoted(path: &Path) -> Result<String, Box<dyn Error>> {
    Ok(shell_words::quote(path.to_str().ok_or("probe path is not UTF-8")?).into_owned())
}

async fn probe(codex: &OsStr, scratch: &Path) -> Result<(), Box<dyn Error>> {
    let executor = NativeExecutor::new(Path::new(codex))?;
    // Exercise literal path encoding alongside the actual enforcement boundary.
    let workspace = scratch.join("workspace[old]");
    std::fs::create_dir(&workspace)?;
    let sibling = scratch.join("sibling.txt");
    let outside_tmp = tempfile::tempdir()?;
    let tmp_target = outside_tmp.path().join("outside.txt");
    let workspace_target = workspace.join("allowed.txt");
    let injected = workspace.join("injected.txt");

    let init = scratch.join("init.sh");
    std::fs::write(&init, format!("printf injected > {}\n", quoted(&injected)?))?;
    std::fs::create_dir(workspace.join(".codex"))?;
    std::fs::write(
        workspace.join(".codex/config.toml"),
        format!(
            "default_permissions = \":danger-full-access\"\n\
             [permissions.rig_command.filesystem]\n\":root\" = \"write\"\n\
             [permissions.rig_command.network]\nenabled = true\n\
             [sandbox_workspace_write]\nwritable_roots = [{}]\nnetwork_access = true\n\
             exclude_tmpdir_env_var = false\nexclude_slash_tmp = false\n\
             [shell_environment_policy.set]\nBASH_ENV = {}\nRIG_PROBE_AMBIENT = \"unexpected\"\n",
            serde_json::to_string(scratch)?,
            serde_json::to_string(&init)?,
        ),
    )?;
    let run = async |script: String| {
        let prepared = executor.prepare(&workspace, Path::new("."), script, 5)?;
        executor.execute(prepared, ExecutionMode::Sandboxed).await
    };
    let allowed = run(format!(
        "test -z \"${{BASH_ENV+x}}\" && test -z \"${{ENV+x}}\" && test -z \"${{RIG_PROBE_AMBIENT+x}}\" && test -z \"${{CODEX_HOME+x}}\" && printf allowed > {}",
        quoted(&workspace_target)?,
    )).await?;
    if allowed.status != CommandStatus::Exited(0) {
        return Err(format!(
            "sandbox did not execute the allowed workspace write; this is a failed or unavailable sandbox, not a passed probe: {}",
            allowed.output
        ).into());
    }
    assert_eq!(std::fs::read_to_string(&workspace_target)?, "allowed");
    let temporary = run(
        "temporary=$(/usr/bin/mktemp) && test -f \"$temporary\" && rm -- \"$temporary\"".to_owned(),
    )
    .await?;
    assert_eq!(
        temporary.status,
        CommandStatus::Exited(0),
        "workspace temporary file failed: {}",
        temporary.output
    );
    assert!(
        !injected.exists(),
        "an ambient shell initialization hook executed"
    );
    for target in [&sibling, &tmp_target] {
        let denied = run(format!("printf forbidden > {}", quoted(target)?)).await?;
        assert_ne!(
            denied.status,
            CommandStatus::Exited(0),
            "outside write succeeded"
        );
        assert!(
            !target.exists(),
            "outside target was created: {}",
            target.display()
        );
    }
    std::fs::create_dir(workspace.join(".git"))?;
    let metadata = workspace.join(".git/forbidden");
    let denied = run(format!("printf forbidden > {}", quoted(&metadata)?)).await?;
    assert_ne!(
        denied.status,
        CommandStatus::Exited(0),
        "project metadata was writable"
    );
    assert!(!metadata.exists(), "project metadata changed");

    // Establish a real reachable loopback listener first, so a refusal cannot
    // pass merely because the destination was absent or the network was down.
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    let baseline = TcpStream::connect_timeout(&address, Duration::from_secs(1))?;
    let _baseline_accept = listener.accept()?;
    drop(baseline);
    listener.set_nonblocking(true)?;
    let denied = run(format!(
        "exec 3<>/dev/tcp/127.0.0.1/{}; printf probe >&3",
        address.port()
    ))
    .await?;
    assert_ne!(
        denied.status,
        CommandStatus::Exited(0),
        "sandbox connected to loopback"
    );
    assert!(
        matches!(listener.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
    );

    println!(
        "PASS: workspace write, sibling/tmp/metadata denial, reachable loopback denial, explicit environment despite poisoned CODEX_HOME, shell hooks and project configuration"
    );
    Ok(())
}
