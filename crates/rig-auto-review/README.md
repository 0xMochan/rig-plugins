# rig-auto-review

`AutoReviewPlugin` independently reviews explicit `native_shell` escalations.
It never installs `NativeShellPlugin` or enables an execution tool. The host
must select native execution separately; with native execution disabled the
reviewer cannot execute commands. Its read-only capacity diagnostics remain
available. Loading an approval policy must not restore disabled authority.

The execution paths are explicit:

- xonsh `backend: "virtual"` calls `virtual_bash` (Bashkit's isolated in-memory
  filesystem), without native execution or escalation.
- xonsh `backend: "native"` calls the separately enabled `native_shell` under
  its OS sandbox. Bashkit is not the native process sandbox.
- An explicit `native_shell` call with `escalate: true` and a justification
  requests review of that exact command outside the OS sandbox. xonsh has no
  automatic escalation or fallback after failure.

It sends a tool-free model call containing the exact prepared command and all
available root-user messages. Assistant text, tool output and file attachments
do not become authorization. Evidence exceeding the configured bound causes a
refusal; it is never silently truncated.
The already loaded global home/project `AGENTS.md` instructions accompany those
messages as restrictions, within the same evidence bound. Changing that section
invalidates an outstanding permit. These are Rig's startup snapshot, not a fresh
disk read at approval time.

Only a strict `allow` verdict can issue an action-specific permit. Missing model
configuration, provider errors, malformed output, timeout, changed commands,
new user input and cancellation never approve escalation. The shell rechecks
the evidence immediately before starting. There is no automatic native retry
and no human-question fallback in this first version.
After compaction, reviewed escalation is refused because original user
restrictions may have been removed. Normal sandboxed work remains available.
Complete original authorization is required until it can be recovered with its
provenance from saved history. A new thread or assistant-written handoff is not
a way to discard earlier restrictions.

Trusted user settings in `RIG_HOME/settings.toml`:

```toml
[auto_review]
model = "chatgpt/gpt-6.1-sol"
timeout_secs = 30
max_evidence_bytes = 65536
# policy = "Your explicit additional permission policy."
```

## Long-session capacity and recovery

Call `auto_review_status` with `{}` to inspect the **running instance's** configured
bound, the supported ceiling, and complete required bytes/counts split between
root-user text and loaded project restrictions. It does not run a shell, call a
model, reveal message contents, change settings, or issue a permit. With native
execution disabled it reports that state without enabling the backend.
`within_bound` is a size measurement only, not authorization or a promise that
the reviewer/provider will accept a request. `configuration_error` must be
checked separately. Missing, pending or compacted evidence is `unavailable`;
partial remaining history is never reported as complete.

Overflow refusals now include the same complete numeric totals and explicitly
state that the reviewer was **not called**. Capture measures borrowed text before
allocating the owned review snapshot. Neither the default (65,536 bytes) nor the
ceiling (262,144 bytes) has been increased. The limit counts trimmed user text
(with two-byte separators between text parts of a message) plus the full loaded
restriction sections, not JSON overhead, the command, policy, or model tokens.
Measurement scans retained history; its counters and report are fixed-size, not
a per-message diagnostic list.

For an existing oversized but **uncompacted** thread:

1. Rebuild/restart the host using these plugin sources and resume the same saved
   thread with its full history. Merely editing this checkout does not update an
   already running agent. Call `auto_review_status` to measure that instance;
   do not resubmit the denied command just to measure it.
2. Only if the complete evidence fits within 262,144 bytes, ask the trusted user
   to approve an appropriate `auto_review.max_evidence_bytes` in their Rig-home
   settings, with headroom for additional input. Do not change trusted settings
   or raise the ceiling automatically. Repeating consent grows history rather
   than fixing capacity. Raising to the maximum is not guaranteed to suffice.
3. After loading the approved setting and measuring again, submit a **new** exact
   action for independent review. The failed call is never automatically retried
   or retroactively approved. New input or changed restrictions still revoke
   permits, and the shell revalidates immediately before launch.

If the total exceeds the ceiling, or history is compacted, this change does not
restore reviewed escalation. Continue only work already allowed by the ordinary
sandbox. Do not prune restrictions, trust a generated summary, switch tools, or
move execution to a fresh thread to bypass the refusal. Provenance-preserving,
bounded recovery from original saved entries remains deferred; no session logs
are read by this tool.

Rig author metadata, not tag names or claims inside text, determines provenance.
Plugin/delegate/rig-authored entries and titled injected user entries do not
become authority. If a transport supplies generated context as plain root-user
text, this plugin cannot recover its origin; it retains that text rather than
heuristically stripping possible user restrictions. That classification needs
a trustworthy transport-side solution, not an allow rule in this plugin.

Model availability comes from Rig's configured providers and login; the plugin
does not add credentials or silently substitute a model. The initial prompt is
original wording informed by Codex's permission-review architecture, rather than
a claim of Codex policy or security parity. This is a prototype permission layer;
the independent OS sandbox remains responsible for constrained execution.
