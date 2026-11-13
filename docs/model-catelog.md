# Day-one model catalog supplement

`rig-model-catelog` is a provider-neutral, curated supplement for definitions
missing from the pinned Rig catalog. Its `ModelCatalogPlugin` reads all provider
rows in `src/models.json` through Rig's existing strict parser; it contains no
Gemini-specific registration logic. New supported-provider models need data and
source evidence, not a new plugin or loader branch. There is no automatic
network discovery, provider installation or permission change.

The initial additions remain Gemini 4 Carbon and Hydrogen. Their verified
sources and model-specific compatibility limits follow. Future additions should
record their own pinned source and meaningful validation here; the registration
test checks every bundled row without a hard-coded model count.

## Initial Gemini additions: verified sources

Rig **0.46.0**, published from
[`6e039cd24994bc1e7d963af6f6278df73a81b130`](https://github.com/0xPlaygrounds/rig/tree/6e039cd24994bc1e7d963af6f6278df73a81b130),
ships Gemini 4 Argon but not Carbon or Hydrogen. Relevant source integration
points, verified in the installed release crates:

- `rig-core/src/catalog/models.json`: the bundled definitions.
- `rig-core/src/catalog/{read,mod,connector}.rs`: strict override parsing,
  exact lookup, insertion, and connector-preserving catalog mutation.
- `rig-ecs/src/model.rs`: `Models(pub Connector)`; the kernel initializes it
  with `init_resource`, preserving an earlier plugin's additions.
- `rig-basics/src/login.rs`: sign-ins/providers are added to that same connector.
- `rig-acp/src/session/options.rs`: ACP builds choices from `Models.0.reachable()`
  and observes changes to the resource. There is no ACP-specific model list.
- `rig-core/src/providers/gemini/options.rs`: native Gemini rejects `XHigh` and
  `Max` unconditionally, even when a model's catalog entry includes them.

The published [models.dev API](https://models.dev/api.json) was read on
**2026-11-12**. Its provider rows were checked against immutable upstream files
at revision
[`a6f2292b7a1ecccaf61e6d3cdec317bb7aef05b9`](https://github.com/anomalyco/models.dev/tree/a6f2292b7a1ecccaf61e6d3cdec317bb7aef05b9):

| Source path under `providers/` | Git blob |
| --- | --- |
| `google/models/gemini-4-carbon.toml` | `48682cd55a2af3e624000ea0acbf1087dc116243` |
| `google/models/gemini-4-hydrogen.toml` | `30425643c5f26fdde81e88738d66784a4f9b49aa` |
| `google-vertex/models/gemini-4-carbon.toml` | `598730a7233559f0c3773948e0ad373ed3e9a61b` |
| `google-vertex/models/gemini-4-hydrogen.toml` | `bb71dc76596bd5a2bcb91646a47ede663ebdb8e5` |
| `openrouter/models/google/gemini-4-carbon.toml` | `7e767044497aba22fd78581b0d30642074c5b1d4` |
| `openrouter/models/google/gemini-4-hydrogen.toml` | `9062c8207f45ea19dc9b2b1a2a9d0989cb09b645` |
| `github-copilot/models/gemini-4-carbon.toml` | `9602eed55c3058d7d5de0ed995e14f179fb3f376` |

The local, deliberately adapted `crates/rig-model-catelog/src/models.json` has
SHA-256 `8f2f89912f6afe014dab23425279807d27a8f63c8fa55ecbfe3dc8eabd66f8a4`.
It uses Rig 0.46.0's existing models.dev-shaped schema, not upstream's newer TOML
schema. These are public source/catalog observations, not our own live provider
validation. Upstream itself notes that Copilot Carbon's input/limits were
inherited from its Argon entry without a live Copilot check.

## Implementation and deliberate limits

`ModelCatalogPlugin` validates the embedded data as a strict override against
an empty catalog, then inserts **only missing** vendor/model rows during plugin
build. This protects existing custom/upstream definitions and preserves the
host connector's handlers, sign-ins and transport. An invalid embedded catalog
fails before any host mutation. Borrowed specs are cloned only when a missing
row needs an owned catalog value. Only the already pinned `rig-harness` runtime
dependency is added; there is no scheduler, network task, provider or tool.

The Google Hydrogen entry omits `xhigh` from its effective choices because the
pinned native Gemini wire rejects it. OpenRouter keeps all four published
Hydrogen levels. Vertex AI retains its published controls but requires a
separate provider; no `rig-vertexai` source/runtime was available for validation.
A full Gemini translator update is deferred rather than silently switching
providers or forking Rig.

OpenCode's rows were verified to use Google's protocol, which Rig 0.46.0 cannot
select at that vendor. They are excluded: the strict override reader could
otherwise fall back to OpenCode's primary OpenAI protocol. Copilot Hydrogen is
not in the verified source and is not invented. Other gateway vendors are
outside this small supplement.

Prices are a snapshot, not a billing guarantee. Carbon's Google/Vertex/Copilot
source rates are introductory through **2026-11-30**; the documented December
increase, Vertex region/tier multipliers, grounding charges and upstream v2
capabilities are not represented by these legacy catalog rows. Hydrogen's
published base rates do not have that introductory increase. Automatic updates,
full v2 cost/capability support, and paid provider calls are deferred.

## Validation

Passed locally:

```sh
cargo test --offline --locked -p rig-model-catelog --lib
cargo clippy --offline --locked -p rig-model-catelog --all-targets -- -D warnings
cargo fmt -p rig-model-catelog --check
git diff --check -- Cargo.toml Cargo.lock README.md crates/rig-model-catelog
```

Three tests exercise observable contracts: every bundled provider/model entry
and unchanged builtin rows, without a fixed addition count; native Gemini's
effective reasoning limit versus OpenRouter Hydrogen's fourth level; and custom
Google/Anthropic definitions and handlers surviving with both Google models
in the reachable collection consumed by ACP.
The scripted provider sends no model request. No editor ACP handshake or live
provider inference was performed.

The original Gemini-named supplement was added to the user's existing
`~/.rig/plugins.toml` with `rig plugin add`; its `rig plugin check` passed with
**38 plugins, all valid**. The subsequent offline `rig build` request was denied
by the command-approval policy before execution. It was not retried, so that
installation did not build or stage a new global binary.

The provider-neutral rename replaced the old configuration entry with
`rig_model_catelog::ModelCatalogPlugin` at `crates/rig-model-catelog`. No global
rebuild was retried. Run `rig build` explicitly and restart the editor's ACP
connection to activate it; source edits do not update a running process.
