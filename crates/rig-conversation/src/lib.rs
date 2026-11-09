//! Conversational progress guidance, independent of tools and presentation.
//!
//! Only the exact stock Rig role on ordinary agent turns is changed. Custom
//! roles, structured side calls, project sections and execution policy stay owned
//! by their existing plugins. Rig already streams and retains assistant text
//! alongside tool calls; this plugin neither manufactures messages nor promotes
//! reasoning content into the transcript.

use rig_basics::prompt::{DEFAULT_SYSTEM_PROMPT, DefaultPromptPlugin};
use rig_harness::prelude::*;

const QUIET_RULE: &str = "- Between tool calls, write only a finding or a blocker the user should know about, unless \
    they ask for more. Do not narrate routine reads, searches and edits.";
const PROGRESS_GUIDANCE: &str = include_str!("progress.md");

/// Replaces the stock no-narration rule with brief, natural progress updates.
///
/// Installs Rig's default prompt if absent. Explicit custom roles are left alone,
/// even if they contain similar wording. Load this alongside [`AgentPlugin`].
#[derive(Default, Debug)]
pub struct ConversationPlugin;

/// Other prompt editors can order their changes without depending on the
/// private transformation implementation.
#[derive(SystemSet, Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ConversationPrompt;

impl Plugin for ConversationPlugin {
    fn build(&self, app: &mut App) {
        rig_harness::load::<DefaultPromptPlugin>(app, "rig-basics", "version 0.46.0");
        app.add_systems(
            AgentPass,
            conversational_prompt
                .in_set(PrepareSystems::Edit)
                .in_set(ConversationPrompt),
        );
    }
}

fn conversational_prompt(mut calls: Query<&mut ModelRequest, (Added<Admitted>, With<TurnCall>)>) {
    for mut call in &mut calls {
        if let Some(prompt) = &mut call.prompt
            && prompt.role == DEFAULT_SYSTEM_PROMPT
        {
            prompt.role = DEFAULT_SYSTEM_PROMPT.replacen(QUIET_RULE, PROGRESS_GUIDANCE.trim(), 1);
        }
    }
}

#[cfg(test)]
mod tests;
