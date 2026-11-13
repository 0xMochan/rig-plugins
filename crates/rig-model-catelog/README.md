# rig-model-catelog

A provider-neutral home for **day-one model definitions** missing from the pinned
Rig release. Add verified rows to `src/models.json` without changing the loader
or waiting for a Rig release. ACP and the terminal model picker use this same
shared catalog.

The initial additions are Gemini 4 Carbon and Hydrogen. They are data, not a
restriction on which model families this plugin can register.

## Install

Add this entry to your existing `~/.rig/plugins.toml`:

```toml
[[plugin]]
crate = "rig-model-catelog"
path = "/absolute/path/to/rig-plugins/crates/rig-model-catelog"
plugin = "rig_model_catelog::ModelCatalogPlugin"
```

When migrating from the original Gemini-only package, replace its plugin entry
rather than enabling both. The old crate path and `GeminiCatalogPlugin` type no
longer exist.

Run `rig plugin check` and `rig build`, then restart the ACP server/editor
connection (or reload the terminal agent). Already-running binaries do not gain
new entries from source edits alone. Keep your existing sign-ins and defaults.

## Add a model on release day

1. Verify the exact provider/model ID, supported wire protocol, capabilities,
   reasoning controls, limits and prices against a pinned source. A model's
   metadata can differ between providers; do not copy a sibling's definition.
2. Add its row under that provider's `models` object in `src/models.json`, using
   Rig 0.46.0's models.dev-shaped JSON schema. New rows must explicitly declare
   `reasoning`, `tool_call` and `modalities`. Keep unknown limits/prices absent
   instead of inventing values. No Rust registration changes are needed.
3. Record the source revision and any compatibility adaptations in
   [the catalog notes](../../docs/model-catelog.md). Add a focused regression
   check when a new model introduces a meaningful behavior or failure boundary.
   The existing registration test automatically covers every bundled row.
4. Run the checks below, rebuild the existing Rig agent and restart ACP.

This can add definitions for **any provider already supported by the pinned Rig
build**. It cannot add a new provider implementation or make an unsupported wire
protocol/reasoning control work. Existing vendor/model rows always win: this
supplement fills gaps, not overrides or corrections to existing definitions.

## Registration and access

Registration uses the existing `Models` connector during plugin build, before
session startup. Existing definitions always win, including custom metadata and
newer upstream entries. Other catalog rows, provider handlers, sign-ins and
transports remain unchanged. Invalid bundled data fails before modifying the
connector; it is not silently skipped.

Catalog membership is not access: **ACP lists reachable models only**, so you
still need the corresponding sign-in/API key and provider support. There are no
tools, model requests, network refreshes, credentials, settings changes or
execution/approval policy changes in this plugin. Day-one support means curated
additions to this file, not automatic discovery or downloading unreviewed data.

## Initial additions

| Provider | Carbon | Hydrogen |
| --- | --- | --- |
| Google Gemini | `gcp.gemini/gemini-4-carbon` | `gcp.gemini/gemini-4-hydrogen` |
| Vertex AI | `vertexai/gemini-4-carbon` | `vertexai/gemini-4-hydrogen` |
| OpenRouter | `openrouter/google/gemini-4-carbon` | `openrouter/google/gemini-4-hydrogen` |
| GitHub Copilot | `copilot/gemini-4-carbon` | Not published in the verified source |

### Initial model compatibility limits

- Carbon offers `low`, `medium`, `high`. Hydrogen also supports `xhigh` upstream,
  but Rig 0.46.0's native Gemini translator rejects that level unconditionally.
  The Google Hydrogen entry therefore advertises only `low`, `medium`, `high`
  rather than exposing a broken ACP choice. OpenRouter retains `xhigh`. Vertex
  AI retains the published levels; it requires a separately registered provider
  that has not been validated here.
- OpenCode's published rows use Google's wire protocol, which Rig 0.46.0 cannot
  select for OpenCode. They are deliberately omitted rather than misrouted
  through its OpenAI transport. Copilot Hydrogen is also not invented.
- Provider-specific limits and prices come from the verified catalog source,
  not from copying Argon's definition. Carbon's Google/Vertex/Copilot prices are
  introductory through 2026-11-30; scheduled increases and region/tier pricing
  are not modeled. No paid live requests or editor ACP handshake were performed
  during validation.

[Source revisions, deliberate adaptations and validation](../../docs/model-catelog.md).

## Check

```sh
cargo test --offline --locked -p rig-model-catelog --lib
cargo clippy --offline --locked -p rig-model-catelog --all-targets -- -D warnings
cargo fmt -p rig-model-catelog --check
```

Tests verify every bundled addition, preservation of builtin/custom rows and
registered handlers, initial model-specific metadata and reasoning choices, and
both Google models in the `reachable()` collection ACP consumes, using a scripted
provider without sending a model request.
