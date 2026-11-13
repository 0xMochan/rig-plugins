//! Capacity diagnostics use the live capture rules, never a shell or a reviewer.

use rig_harness::prelude::*;
use rig_harness::rig_core::tool::{ToolOutput, ToolResult};
use rig_native_shell::{Authorization, NativeShellStatus};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::json;

use crate::review::{MAX_EVIDENCE_BYTES, ReviewConfig};

pub(crate) fn install(app: &mut App) {
    app.add_open_tool(
        "auto_review_status",
        "Measure automatic-review evidence capacity for this agent's root conversation without requesting execution or calling a model. Reports configured and required bytes/counts, never message contents. This is not approval. Incomplete or compacted history remains fail-closed; no settings are changed.",
        ToolOptions { footprint: Some(Touches::Independent) },
        report,
    );
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct Request {}

fn report(
    called: On<ToolCalled<Request>>,
    config: Res<ReviewConfig>,
    native: Option<Res<NativeShellStatus>>,
    tools: Query<&ToolDef>,
    authorization: Authorization,
    mut commands: Commands,
) {
    let measurement = if native.is_none() {
        Err("Native execution is disabled; no authorization snapshot is available.".to_owned())
    } else {
        authorization
            .measure(called.agent)
            .map_err(|error| error.to_string())
    };
    let evidence = match measurement {
        Ok(size) => json!({
            "state": if size.required_bytes <= config.max_evidence_bytes {
                "within_bound"
            } else {
                "too_large"
            },
            "size": size,
            "fits_supported_maximum": size.required_bytes <= MAX_EVIDENCE_BYTES,
        }),
        Err(error) => json!({"state": "unavailable", "reason": error}),
    };
    let result = json!({
        "measurement_only": true,
        "configured_max_evidence_bytes": config.max_evidence_bytes,
        "supported_max_evidence_bytes": MAX_EVIDENCE_BYTES,
        "configuration_error": config.validate().err(),
        "native_execution": match native.as_deref() {
            None => "disabled",
            Some(NativeShellStatus::Ready { .. }) => "ready",
            Some(NativeShellStatus::Unavailable { .. }) => "unavailable",
        },
        "native_shell_tool_registered": tools.iter().any(|tool| tool.0.name.as_str() == rig_native_shell::NATIVE_SHELL),
        "evidence": evidence,
    });
    commands
        .entity(called.call)
        .insert(Outcome::from(ToolResult::success(ToolOutput::json(result))));
}
