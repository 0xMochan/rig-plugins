# rig-delta

A native Rust integration with the [Delta CLI](https://delta.dev/docs/cli/get-started),
not a Claude Code wrapper, MCP server, or the unrelated Git diff viewer.

The plugin now contains **live multiplayer workflows**, not just snapshot
sharing: ongoing transcript recording, owned file mounts and watches, remote
context, turn-takeover cancellation, and fresh-session join. **This is not yet a
full Claude Code parity or real-service validation claim.** The two-client
protocol fixture has passed; actual Delta account uploads and cross-machine
collaboration have not been exercised. See [remaining parity and validation
gaps](#remaining-parity-and-validation-gaps); detailed source studies and
session-specific validation notes are kept in the local research archive.

## Setup

This workspace pins Rig **0.46.0** and Bevy **0.20.0**. The process owner targets
the repository's Unix/macOS environment. Install Delta separately using its
official instructions. Delta **0.22.0+** introduced recording contract **8**,
which this implementation requires:

```sh
delta --version
delta --json thread contract
```

The installed Delta 0.24.0 was observed reporting contracts 8–9. Incompatible
ranges or malformed required fields refuse; extra response fields are allowed.
The CLI remains beta.

By default the plugin resolves `delta` from captured PATH. To avoid a name
collision, configure the installed CLI or its link:

```toml
[delta]
executable = "/absolute/path/to/delta"
```

Do not copy Delta's executable: it locates its app next to itself. Native host
workflows refuse a configured executable beneath any model-writable project,
workdir or approved workspace root. Use the installed client, not a writable
build/script that could change after consent. The plugin does not run an
installer or download executable code automatically.

**Keep `RIG_HOME` outside every agent's writable project/worktree roots.** Live
mode refuses otherwise: a model-writable journal or consent store is not trusted
future protocol input. It also refuses file sharing whose checkout contains
Rig's private home, so raw sessions, prompts or credentials cannot enter file
sync merely because Git ignore rules changed. Keep using your normal `~/.rig`
home outside the checkout rather than putting an agent home in this repository.

Add `DeltaPlugin` explicitly to your existing global `~/.rig/plugins.toml`, beside
your selected execution/review policy and a native interactive frontend:

```toml
[[plugin]]
crate = "rig-delta"
path = "/absolute/path/to/rig-plugins/crates/rig-delta"
plugin = "rig_delta::DeltaPlugin"
```

Run `rig plugin check` and `rig build`, then restart/reload Rig. The optional
`tui` feature selects the fresh joined agent in Rig's TUI. An already-running
binary does not acquire new behavior from source edits. Bounded model CLI calls
require a separately enabled native-shell tool; this plugin does not enable it
or its reviewer.

## Live share

```text
/share
```

`/delta share` and `/delta-share` are aliases. Audience hints are accepted, but
are not confirmation:

```text
/share privately
/share with my team
/share ada@example.com sam@example.com
```

These are **native UI workflows**, not prompts asking a model to approve itself.
They require an interactive frontend and an idle root session, including idle
background descendants. They do not start a model turn. The workflow separately
asks for:

1. **Delta host access** — the exact installed executable, captured environment,
   control directory, fixed CLI purpose and process lifetimes. This uses the OS
   user's network, app storage and filesystem authority outside the command
   sandbox; it is not an OS containment claim.
2. **Audience and ongoing data scope** — private sync, organization-wide access,
   or explicit email invitations; the organization is chosen separately. Public
   messages and tool inputs/results may contain secrets. Reasoning and supported
   embedded images are off unless explicitly selected. Checkout file sync
   includes non-ignored staged, unstaged and untracked changes.
3. **Publication confirmation** — this enables ongoing session/subagent
   recording and file sync, not merely a historical snapshot.

A positive answer must be the local frontend's `Person` decision on the unchanged
native question and unchanged answer. A model/plugin answer, rewritten outcome,
missing UI, queued superseding local intent, changed identity or stale directory
cannot grant the workflow. A consent audit is flushed before the first host
process. Publication consent does not authorize unrelated commands or widen
`native_shell` access.

The plugin creates/reuses the stable producer (`rig` + Rig `AgentId`), saves
identity before fallible attachment steps, reuses an identified existing worktree
rather than blindly creating another, and owns the existing-checkout mount.
It validates readiness, access and producer frontier before enabling the live
watch/recording path. Existing broader sharing is never described as “just me.”
Every invitation result is checked; invitations do not narrow existing access,
and unsharing keeps explicit invitations.

A returned link is displayed only if Delta returned it. Private background
upload may not yet have a URL. **Local append receipts are not server delivery,
and cached status is not proof that a teammate's files have arrived.** No link is
fabricated to make a pending upload look complete.

## Join, worktrees and local ownership

```text
/delta join https://delta.dev/join/RETURNED_SHARE_ID
```

`/delta-join LINK` is an alias. Joining checks access and mounts the existing
thread's complete inspected worktree set. It never exports the previous local
conversation, attaches its checkout, or republishes the thread.

After all worktrees report ready, native folder trust shows the exact canonical
roots and frozen applicable AGENTS instructions. Trust creates a **fresh root
Rig agent**, not a fork or descendant of the old session. The original session
remains resumable. No broad common-parent directory is granted, and ordinary
agents remain confined to the original project.

For multiple worktrees, the first actual checkout is the initial workdir:

```text
/delta worktree RETURNED_OPAQUE_REFERENCE
```

This selects only an already-approved root. The native UI command waits for an
idle turn; the model's `delta_worktree` tool reports an explicit in-turn change.
File tools and native commands remain confined to the active workdir. Project
instructions used by the model and native review evidence match the scoped
joined roots; unrelated global policies remain effective. Restored/copied
metadata is not workspace trust.

Before coding effects run, the local turn's `running` record must receive its
matching acknowledgment. A watch `superseded` or append `fenced` signal stops the
matching local turn and descendant work through Rig's real `StopTurn`, including
owned native children. Late output retains its original Delta sequence. Remote
messages, comments and tool data enter as **plugin-authored inert context**;
they never become local user authorization, executable tool calls, or an
automatically started model turn. Remote thoughts are excluded from model
context.

Actual local subagents get their own producer and the **opaque child reference
returned by Delta**. Their inherited checkpoint prefix is baselined rather than
re-exported; a real parent assignment carries parent-sender attribution. They
borrow their parent's approved mount and are held until their own turn start is
acknowledged. No new or isolated filesystem root is inferred from a task role.

Reported remote child/subthread activity has a separate bounded observation path:
invisible watches preserve the returned opaque references, carry no producer or
participation claim, and can only produce inert context changes. They cannot
append, mount, start local turns or execute tools. Their ledgers survive parent
reconnect. This newer integration needs the final validation run; it is not
included in the earlier two-client fixture milestone.

## Controls and setup policy

```text
/delta status
/delta reconnect
/delta disconnect
/delta setup
/delta disable-auto
```

- **Status** reports the current connection, cached audience/returned URL,
  participants, worktrees, child metadata, pending sequences and pause reason.
  It is not a fresh server-delivery or file-arrival check.
- **Reconnect** preserves producer, mount owner, journal and context checkpoint.
  It requests fresh native host permission and, for joins, re-establishes mounted
  directory trust. Failures pause; they do not authorize retries or escalation.
- **Disconnect** stops participation and observes owned CLI termination/flush.
  Unsent records and identity remain durable. A saved bound session is gated
  rather than silently resuming as an unshared executing session.
- **Setup** saves manual/private/organization intent in `.delta/settings.json`
  while preserving unknown keys. A separate local grant binds the exact document,
  directory identity and export scope. Repository text alone cannot enable
  publication. Malformed, changed or untrusted policy disables automation.
- **Automatic policy still requires a fresh native host-access confirmation for
  every connection.** A protected prior setup can preconfirm publication scope,
  not manufacture process authority. This is not unattended Claude-style sync.
- **Disable-auto** revokes local automatic-policy consent for the checkout;
  existing connections are unchanged until disconnected.

Keep `.delta/settings.json` excluded from Git; the policy module does not silently
edit Git metadata. A copied/forked binding cannot reuse the source producer or
outbox. Start a fresh unbound Rig session for an independent share; never reset
or delete a journal to force recovery.

Closed, separately confirmed administrative controls include:

```text
/delta org list
/delta org current
/delta org switch ORGANIZATION_ID
/delta org create "Team Name"
/delta org invite ada@example.com sam@example.com
/delta auth status
/delta auth login
/delta update --check
/delta update
/delta unshare THREAD_REFERENCE
/delta fork THREAD_REFERENCE
```

The login control uses Delta's nonblocking device-login form. Device sign-in
displays the returned HTTPS URL for the user to open, then offers an explicit
status check. No browser is opened automatically. Update may restart
Delta and disconnect clients. `/delta plugin-update` explains the native Rig
rebuild/reload route; it does not invoke Claude marketplace commands or download
code. Help (`/delta`, `/share --help`, `/delta share --help`) runs no process or
model turn.

## Model tools and execution boundaries

| Surface | Responsibility |
| --- | --- |
| `delta` | Bounded typed CLI inspection/management, including explicit thread sharing/invites |
| `delta_sync` | One explicit historical public-transcript batch; no live ownership or file sync |
| `delta_session` | Live in-memory metadata only; no process, upload, reconnect or permission |
| `delta_worktree` | Select an already native-approved joined worktree; no new root or mount |
| Native `/share` / `/delta …` | Locally confirmed live workflow and session-owned processes |

The two bounded legacy tools still create ordinary `native_shell` child effects:
OS sandbox by default, separate exact-command review for explicit escalation,
1–300-second deadline, no automatic retry. Both the Delta tool and `native_shell`
must be allowed; the plugin never widens an allowlist. Xonsh's typed native
command path does not satisfy this legacy dependency: these Delta tools have not
migrated to it. `confirm_upload` is intent, not an execution permit. Their typed
actions include contract/auth/org inspection,
thread list/search/show/status/create/share/invite/unshare/fork,
worktree list/create/attach, and MCP schema search. Search is bounded to 1–100
results; message ranges are 1-based, inclusive and at most 20 messages. MCP schema
search never invokes connected tools.

Live mode is a **different native host capability**, with direct executable/argv,
no shell, a closed command family and explicit local approval. Watch/mount stdin
is structurally closed; append stdin is an owned immutable bounded record batch.
It does not reuse a foreground-command permit, increase its timeout, relax the
shared launcher, or give the model a general host-process tool. Delta itself owns
authentication, storage, networking and file-sync behavior; the Rust controller
owns its CLI handles and readiness/cancellation transitions.

Legacy `delta`/`delta_sync` are refused in live-bound agents so their temporary
workspace spools cannot be synced. Once a producer has `live.json`, historical
recording also refuses its stale old `outbox.json`, even if binding metadata was
lost. Opaque child locators are preserved, never reconstructed from thread IDs.

## Durable records and limits

Before sending, immutable numbered records are atomically saved and flushed under
`<RIG_HOME>/delta/<AgentId>/`. Preserve that state with the matching Rig and Delta
stores. It includes **plaintext** retained public updates after receipts for
reconciliation, `live.json`, binding/audit metadata and a shared recording lock.
A hard crash can leave a lock: inspect and establish that no owner runs before
removing only the stale lock, never resetting sequence history.

Matching receipt prefixes retire records; lost receipts replay the same bytes,
including duplicate receipts. Partial/fenced/malformed responses cannot become
successful ownership. Migration from the historical journal preserves pending
wire bytes and the frontier. Export scope cannot silently expand after journal
initialization. System/plugin prompts and opaque provider payloads remain
excluded even when reasoning/images are selected. No URL/provider media is fetched
for export. Unsupported binary content gets an explicit omission note.

| Boundary | Limit |
| --- | --- |
| Live journal | 64 MiB |
| Live JSONL record | 16 MiB |
| Ordinary live append batch | 64 records / 2 MiB; a larger valid record is sent alone |
| CLI stdout line | 1 MiB |
| CLI queued stdout | 128 lines / 8 MiB |
| Separate stderr tail | 64 KiB |
| Native one-shot / readiness / TERM grace | 60 seconds / 30 seconds / 5 seconds |
| Main watch state / delivered ledger | 8 MiB each; 4,096 items, 2,048 comments, 256 directory entries |
| Remote child observation | 8 watches / 8 MiB aggregate delivered child ledgers |
| Joined worktrees / AGENTS text | 32 roots / 256 KiB |
| Policy/store / policy consent audit | 1 MiB / 16 KiB |
| Historical `delta_sync` | 64 records / 32 KiB batch, 32 KiB record, 8 MiB journal |

Overflow and malformed protocol pause instead of silently truncating ownership
activity. Normal teardown retains process owners through terminal results, then
uses bounded TERM/flush followed by kill/reap. A hard host kill cannot execute
Rust cleanup; Delta's owner monitoring, retained checkouts and journal recovery
still need real-service validation. Foreground legacy spool directories may
remain after a hard crash and can contain private CLI output; never share them.

## Remaining parity and validation gaps

The implementation is not yet a blanket feature-parity claim. Remaining work or
unverified behavior includes real-account two-client/cross-machine handoff,
installer execution, automatic browser opening, managed offline Fork & Restore /
Discard, fully unattended activation/reconnect, richer native status controls,
and recovery of earlier child/checkpoint archives. It does not create isolated
subagent worktrees or merge them back. A failed watch/process or rejected append
gates coding; a local acknowledgment and watch snapshot do not themselves prove
global server ownership or delivery. Managed offline arbitration/recovery remains
unverified. Explicit `fork` is not the complete offline conflict-recovery workflow.

The fixtures prove native workflow/protocol plumbing—including file exchange and
nested cancellation—not Delta's remote durability or all app/platform behavior.
See the [workspace execution contract](../../README.md#execution-contract) for
shared policy boundaries. Detailed source and acceptance notes are kept in the
local research archive.
