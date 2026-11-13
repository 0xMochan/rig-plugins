# rig-status

`StatusPlugin` adds the read-only `agent_status` tool. It reports actual
harness-registered plugins, the calling agent's allowed tools, its effective
working directory, and optional native-execution startup diagnostics. It installs
no other plugins, starts no processes, and needs no working shell backend.

Add this entry to your existing Rig home's `plugins.toml`:

```toml
[[plugin]]
crate = "rig-status"
path = "/absolute/path/to/rig-plugins/crates/rig-status"
plugin = "rig_status::StatusPlugin"
```

Use Rig 0.46.0 and run `rig build`, then restart or reload manually. No separate
agent or Rig home is needed.

## Boundaries

The inventory comes from Rig's registrations, not source-code guesses or the
skills directory. A `rig-native-shell` Rust dependency lets this plugin inspect
its optional status resource; it does **not** load any native runtime or expose
`native_shell`. `native_execution.state` reports `"disabled"` without a backend,
or `"unavailable"` when a separately selected backend is broken.

`native_shell_tool_registered` separately reports whether the public shell tool
exists. Xonsh can select the shared native runtime while that flag remains
`false`; a usable backend is not evidence that the direct shell tool was enabled.

A `"ready"` native backend means startup compatibility validation passed, not
that filesystem or network isolation was probed. This tool grants no execution
or escalation permission. Plugin registrations made outside Rig's tracked load
API may not appear in the inventory.

## Checks

```sh
cargo test --locked -p rig-status
```

Tests exercise an inventory without execution and a separately selected broken
backend with and without the optional public tool, including the tool allowlist
and effective work directory. They do not
start native commands or make model requests.
