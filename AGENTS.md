# Working in this repository

Never comment on GitHub issues, reviews, or pull requests unless the user explicitly requests it. Local commits do not authorize pushing.

Build small, independently usable Rig plugins. The user's global Rig home owns agent composition through its existing plugins.toml; this repository does not maintain an agent launcher, Rig home, or bundled profile. Keep execution and permission policy separate. Pin the Rig release and matching Bevy version; do not accidentally mix a local Rig checkout with registry APIs.

Keep packages in a flat `crates/` workspace. Each independently reusable capability is its own crate. The Beautiful Rust guidance we applied and the deliberate exceptions are linked in `docs/rust-design.md`; read its Bevy guidance when changing ECS scheduling or ownership.

Before coding, state the responsibility being added and the boundary it protects. Defend input types, ownership, failure behavior, and dependencies in terms a human reviewer can follow. Prefer borrowed inputs for synchronous inspection and owned immutable values when work crosses an asynchronous boundary. Do not add generic abstractions for a single hypothetical consumer.

Review source and the intended behavior before adding tests. A test must exercise an observable contract or a meaningful failure boundary. Do not accumulate cases merely to increase the test count. Run the smallest relevant checks; the parent agent owns Cargo execution and the lockfile during parallel work.

Sandbox enforcement and model approval are separate. Missing enforcement, invalid review output, stale authorization, cancellation, or timeout must never grant broader execution. Never relax the shared process launcher temporarily. Never infer permission to retry an action from a failed or interrupted result.

Keep research in `docs/`: pin source revisions, distinguish verified behavior from design proposals, explain what was reused and deferred. Record material limitations in the README and validation notes. Commit coherent milestones with messages that explain their purpose.
