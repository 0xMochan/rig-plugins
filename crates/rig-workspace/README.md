# rig-workspace

Explicit working-directory confinement, independently selectable from execution
and approval. `WorkspacePlugin` installs no tools, other plugins or process
launcher.

```toml
[[plugin]]
crate = "rig-workspace"
path = "/absolute/path/to/rig-plugins/crates/rig-workspace"
plugin = "rig_workspace::WorkspacePlugin"
```

Use the pinned Rig 0.46.0 / Bevy 0.20.0 APIs. Add this entry to the existing Rig
home's `plugins.toml`; it does not create another agent or Rig home.

Root agents receive `WorkDir(ProjectRoot)`. This matters: Rig's stock file tools
confine paths only when the agent has a `WorkDir`; otherwise the project directory
is just their base for relative paths. Rig's `ServicesPlugin` retains ownership
of inheriting a parent's narrower directory into children.

Before each tool call, the policy checks the canonical directory against the
original project. Assigning an out-of-project `WorkDir`, or a symlink leading
there, does not widen authority. Metadata tools `agent_status`,
`auto_review_status` and `delta_session` remain available for diagnosis.
Execution/collaboration plugins can order their gates after
`WorkspaceSystems::Gate` without this plugin loading them.

Native-user-approved integrations can install an immutable `WorkDirScope` for
specific canonical worktrees. The grant checks the stable agent identity and
continued identity of each root on every call. A copied, stale or explicitly
denied scope cannot fall back to the original project. Denials propagate to
existing and future descendants; positive grants do not automatically transfer
to another identity. The scope is not serialized or reflect-cloned into forks.
Only a trusted host integration should construct a
grant after obtaining native-user confirmation; the constructor itself does not
ask for or prove consent. Integration-specific exclusions still belong to that
integration. Delta, for example, excludes Rig home and its containing directories.

This is a tool-path/workdir policy, not an OS sandbox or an escalation permit.
It does not hide files from native programs; native isolation and explicit
command escalation remain separate capabilities. Trusted Rust plugins can
change components or launch processes, so loading arbitrary plugins is not a
security boundary. Disable this plugin only if removing its file-path/workdir
restrictions is intentional.

Checks: `cargo test --locked -p rig-workspace` exercises actual file-tool
confinement, narrowed child inheritance, out-of-project workdirs, copied/revoked
worktree grants and changed mount symlinks. It does not launch native commands or
contact a model.
