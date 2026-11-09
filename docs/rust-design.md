# How we defend the Rust

Code is communication to its next maintainer. A passing build is necessary, but the design should explain itself without relying on another adversarial review round.

For each module, ask:

- Does its name describe one responsibility and the state it owns?
- Is the public API smaller than the implementation? Can callers construct an invalid permission or bypass a required transition accidentally?
- Are strings command source, paths, model identifiers, or explanations? Keep those roles explicit at the boundaries where confusion would change behavior.
- Does a value need ownership because an asynchronous operation outlives the caller? Otherwise prefer borrowing. Use `Arc` for actual shared ownership, not to avoid thinking about lifetimes.
- Is an enum expressing a real choice? Are `Option` and defaults safe when configuration is absent or malformed?
- Does every error say what failed? Does cancellation actually stop the owned work rather than detach it?
- Is a helper removing repeated mechanics, or hiding the decision a reviewer needs to see?
- What observable contract earns each test? Use actual interpreter execution for virtual isolation, actual OS enforcement for native isolation, and a scripted model for approval lifecycle checks.

We pin the release API, keep the first concrete backends concrete, and add an abstraction when another implemented consumer needs it. This avoids a shell framework before we have a dependable shell.

The user suggested [Beautiful Rust](https://www.beautifulrust.dev/) as inspiration. We read its [`rust-best-practices`](https://github.com/morganlinton/BeautifulRust/blob/edd3e86087eda07983251056508fe3b8856d85d7/skills/rust-best-practices/SKILL.md) and [`rust-review`](https://github.com/morganlinton/BeautifulRust/blob/edd3e86087eda07983251056508fe3b8856d85d7/skills/rust-review/SKILL.md) skills at revision `edd3e86087eda07983251056508fe3b8856d85d7`, including its [Bevy 0.20 digest](https://github.com/morganlinton/BeautifulRust/blob/edd3e86087eda07983251056508fe3b8856d85d7/skills/rust-best-practices/references/bevy.md). The site initially failed through the research browser; fetching its public skill source directly succeeded.

The concrete choices this guidance informs are a flat `crates/` workspace, private validated command state, owned snapshots across asynchronous work, typed error values, deliberate cloning of immutable review evidence, and explicit ECS ordering where one system consumes another's deferred commands. Bevy systems keep their normal by-value parameters and framework prelude; those are already borrowed views. Passive settings remain plain data, while permission invariants stay behind constructors.

We deliberately avoid copying its entire lint configuration or creating abstraction and test matrices merely to comply with a checklist. Rig/Bevy dependencies are exactly pinned because their release APIs must agree. A concrete process worker owns cleanup and deadlines even when an async caller stops polling; introducing an executor abstraction just for that worker would add a second lifetime contract. The reasons for that exception belong with the runner's ownership documentation. Review questions and the chosen behavior matter more than a larger warning count.

The follow-up audit keeps restriction evidence owned with the user-message
snapshot because both cross the same asynchronous review boundary. Capture
borrows the existing global `project_instructions` sections, checks their bytes,
then copies only their text. That published Rig tag is a deliberate release-pinned
integration point; it avoids a second instruction loader or copying the whole
actor prompt. Later capture compares the same restrictions before launch.
Compacted history refuses escalation until original authorization can be
recovered with provenance. A generated summary cannot fill that ownership gap.
Child directory inheritance belongs to Rig's ServicesPlugin; the profile only
supplies defaults for roots, keeping one owner for each lifecycle responsibility.

Monty code and JSON inputs are owned across worker/host suspension. Conversion
borrows `ObjectRef`, explicitly budgets JSON bytes/depth, and rejects unsupported
types instead of flattening them. Only the separate trusted worker executable
owns Monty's allocator; the Rig plugin owns its checkout and abort guard. A
bounded request queue carries concrete tool requests into ECS, not arbitrary
world-mutating closures. The same host path serves Monty and xonsh.
Dispatch reads parent state through a query and spawns through Commands before
PollTasks/AgentLoop in Update, following Rig Steel's established ingress point.
This avoids Rig's private between-stage persistence systems and runs even when
AgentPass is quiet. It needs no exclusive world borrow. Tests drive App::update
so they exercise the same dispatcher and kernel ordering as the real app.

The xonsh frontend borrows source while scanning. Decoded literal words own their
strings; interpolation expressions remain slices until emitted into Python.
Capture is a boolean, backend is an enum, and literal versus splice is an enum.
The source limit bounds parser work before execution, while Monty validates the
whole generated module. Python helpers render quoted argv only; they do not own
permission or implement a second process launcher. Each module has a concrete
responsibility without a general language framework.

The TUI plugins extend the public key pipeline and panel canvases instead of
forking the frontend or saving a second transcript/history. Wheel events become
stock scroll actions; Rig owns reading anchors and bottom follow. Prefix
navigation owns one original draft and its selected entry across key events,
while searches borrow `PromptHistory`. A suggestion is rendered separately from
the editor text, so merely displaying it cannot submit or persist a completion.
The composition explicitly orders the public wheel, click and history filter
sets because they mutate `PendingInput`; each capability remains independent.

Mouse reporting owns a Unix terminal file for the one immediate write required
before Rig hands control to an external program. Other mode writes use the
public terminal message. That file also gives drop cleanup an independent owner
after Rig redirects stdout; in-memory tests never open it. Startup diagnostics
are ordinary serialized metadata, separate from both sandbox enforcement and
review permits. The profile inventories harness registrations rather than
inventing a second plugin registry.

Checkpoint capture borrows the delivered conversation until Gate and then owns
its view across deferred spawning. Message bodies remain Rig's immutable shared
values; the child owns its own message indices and new suffix. Only history,
cache identity and local prompt data cross this boundary. Arbitrary components,
live effects and approval permits do not. Rig's existing spawn effect still
owns ancestry, settings, depth checks, workdir, reports and cancellation. Local
prompt data uses a concrete saved serde value so restoration needs no registry
of arbitrary forkable components. Only Rig's generated role/environment sections
are replaced; other plugins' child restrictions remain. The task adapter owns
the default selection and schema, independently of the checkpoint mechanism or
execution policy. The pinned deferred-brief timing is covered by requests
actually sent to a scripted model, including the restored child's first call.

Script-tool presentation is its own `rig-tui-tools` crate. It borrows Rig's
owned render batch rather than querying mutable execution effects. Per-agent
fold choices contain only call identities and presentation modes, and native
key observers invalidate cached rows through `RenderRevision`. The renderer
uses the public grapheme-aware wrapper to budget actual terminal rows, keeping
long single-line inputs bounded too. Typed JSON results are read directly, so
stdout and returned values do not disappear in the generic text-only helper.
Exports retain full content independently of folds. Mouse folding uses the
main pane's public `DrawnTranscript` snapshot to map actual scrolled and wrapped
rows to call identities. The expand control is a named decoration, so script
text cannot imitate a clickable row. A press/release pair must hit the same
control without a drag, modifier or modal. Only the mouse filter reads that
snapshot, which adds work and memory proportional to the laid-out transcript;
the execution plugins and mouse-reporting owner remain separate.

The loading line is a separate presentation crate. It borrows the shown agent's
stock `Activity`, owns a monotonic busy-period clock, and draws its own panel.
A post-Draw canvas pass lights only authenticated native welcome-logo cells,
using the main pane's public snapshot instead of invalidating render caches.
It reuses an installed snapshot; standalone snapshot cost grows with retained
transcript text. Prepare follows the upstream activity set; Fit relocates only
the status item whose replacement received space in the default layout. The retained item
restores the stock display on a suppressed panel or agent switch without
overwriting a newer publication. This avoids a second activity model and
preserves other composer metadata. Animation owns a scoped `KeepAwake`, not an
asynchronous task or global poller. Both animations share it using final-layout
visibility; idle, questions and fully hidden animations remove it. No execution
or approval state crosses this presentation boundary.
[Runtime behavior and limitations](../crates/rig-tui-loading).
