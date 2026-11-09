//! Full-context forks over Rig's ordinary spawn effects, without copying live work.
//!
//! Insert [`Checkpoint`] beside a [`SpawnSubagent`] before Gate. The existing
//! subagent plugin still owns spawning, settings, ancestry, reports and stopping.
//! This plugin owns the conversation snapshot and inherited local prompt sections.

use rig_basics::subagents::{Role, SpawnSubagent, SpawnedAgent, SubagentsPlugin};
use rig_harness::prelude::*;
use rig_harness::rig_core::{
    conversation::Entry, message::Message, tool::feedback::tool_result_message,
};
use serde::{Deserialize, Serialize};

const ORIGIN: &str = "rig-checkpoint";
const ROLE: &str = "You are a checkpoint agent continuing the exact conversation prefix above. \
    The next message assigns your task; complete only that task under the inherited user \
    instructions. Earlier tool calls belong to the parent: checkpoint placeholders are not \
    execution results or authorization to replay them. Your final answer reports to the parent; \
    include concrete findings, changed paths and verification. Use message for questions and \
    wait when necessary. The workspace is shared: coordinate edits with the parent.";
const PENDING: &str = "Checkpoint boundary: this unresolved call belongs to the parent. Its \
    outcome is unknown here; this fork did not schedule it. This placeholder is not an \
    execution result or permission to retry. Inspect actual state or ask the parent if needed.";

/// Extends Rig's subagent lifecycle without defining a new tool or execution policy.
#[derive(Default, Debug)]
pub struct CheckpointPlugin;

impl Plugin for CheckpointPlugin {
    fn build(&self, app: &mut App) {
        rig_harness::load::<SubagentsPlugin>(app, "rig-basics", "version 0.46.0");
        app.keeps_saved::<CheckpointContext>()
            .add_systems(AgentPass, capture.in_set(GateSystems::After))
            .add_observer(install)
            .add_observer(follow_successor::<Checkpoint>)
            .add_systems(Update, inherit_sections.before(AgentLoop));
    }
}

/// Requests a full delivered-conversation checkpoint for one spawn effect.
///
/// Supports task agents with inherited system prompt and environment. Explicit
/// model/effort/tool choices still use Rig's validation and may reduce cache reuse.
#[derive(Component, Clone, Copy, Debug, Default)]
pub struct Checkpoint;

/// Saved provenance and local prompt sections of a checkpoint agent.
///
/// It contains no execution permit, open request, process, or parent conversation.
/// Rig saves the child's independent conversation and ancestry through its log.
#[derive(Component, Reflect, Clone, Debug, Serialize, Deserialize)]
#[reflect(opaque, Component, Saved, Clone, Debug, Serialize, Deserialize)]
pub struct CheckpointContext {
    source: AgentId,
    sections: Vec<InheritedSection>,
}

impl CheckpointContext {
    /// The stable id of the agent whose delivered history was copied.
    pub fn source(&self) -> &AgentId {
        &self.source
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct InheritedSection {
    order: i32,
    tag: String,
    text: String,
}

#[derive(Component)]
struct Snapshot {
    conversation: Conversation,
    context: CheckpointContext,
    cache: CacheKey,
}

// Capture again on every unheld attempt to start. A held request must not retain
// a stale snapshot if the parent received steering before the hold was released.
fn capture(
    mut requests: Query<
        (Entity, &SpawnSubagent, &ForAgent, &mut Decision),
        (ReadyToStart, PluginEffects, With<Checkpoint>),
    >,
    parents: Query<(&AgentId, &Conversation, Option<&CacheKey>)>,
    sections: Query<(&PromptSection, &SectionOf)>,
    mut commands: Commands,
) {
    for (entity, request, &ForAgent(parent), mut decision) in &mut requests {
        if decision.is_decided() {
            continue;
        }
        if request.role != Role::Task
            || request.system_prompt.is_some()
            || request.environment.is_some()
        {
            decision.decide(
                Decider::plugin(ORIGIN),
                Outcome::refused(
                    "A checkpoint requires a task with inherited system prompt and environment. \
                     Use a fresh-context task for overrides.",
                ),
            );
            continue;
        }
        let Ok((id, conversation, cache)) = parents.get(parent) else {
            decision.decide(
                Decider::plugin(ORIGIN),
                Outcome::refused("The checkpoint source has no conversation."),
            );
            continue;
        };
        // Message payloads remain Arc-shared. The view owns its indices and has
        // no unsaved parent edits or turn asides to import into the child's log.
        let mut copy = conversation.request_view();
        for call in copy.pending_calls() {
            copy.push(
                Entry::new(Message::User {
                    content: vec![tool_result_message(
                        call.id,
                        call.function.name,
                        PENDING.into(),
                    )],
                })
                .by(Author::plugin(ORIGIN)),
            );
        }
        if let Err(error) = copy.validate() {
            decision.decide(
                Decider::plugin(ORIGIN),
                Outcome::refused(format!("Cannot checkpoint invalid tool history: {error}")),
            );
            continue;
        }
        copy.push(Entry::new(Message::user(ROLE)).by(Author::plugin(ORIGIN)));
        let sections = sections
            .iter()
            .filter(|(_, owner)| owner.0 == parent)
            .map(|(section, _)| InheritedSection {
                order: section.order,
                tag: section.tag.clone(),
                text: section.text.clone(),
            })
            .collect();
        commands.entity(entity).insert(Snapshot {
            conversation: Conversation(copy),
            context: CheckpointContext {
                source: id.clone(),
                sections,
            },
            cache: CacheKey(CacheKey::of(cache, id).to_owned()),
        });
    }
}

// SpawnedAgent is inserted after the child and all its ordinary components.
// Install history before the next Update sends Rig's deferred task brief, so no
// model call or child effect can run against a fresh, summary-only conversation.
fn install(
    added: On<Add<SpawnedAgent>>,
    requests: Query<(&SpawnedAgent, &Snapshot)>,
    mut commands: Commands,
) {
    if let Ok((child, snapshot)) = requests.get(added.entity) {
        commands.entity(child.0).insert((
            snapshot.conversation.clone(),
            snapshot.context.clone(),
            snapshot.cache.clone(),
        ));
        commands.entity(added.entity).remove::<Snapshot>();
    }
}

// Rig synthesizes a fresh subagent role/environment when spawning or restoring.
// Replace those generated sections with the parent's captured sections before the
// first model request. Global sections stay live, including project restrictions.
// The checkpoint role is a conversation suffix, keeping the system prefix shared.
fn inherit_sections(
    children: Query<(Entity, &CheckpointContext), Added<CheckpointContext>>,
    sections: Query<(Entity, &PromptSection, &SectionOf)>,
    mut commands: Commands,
) {
    for (child, context) in &children {
        for (entity, section, owner) in &sections {
            // These tags belong to the pinned SubagentsPlugin. Keep any new
            // child-specific sections from other plugins, including restrictions.
            if owner.0 == child
                && matches!(
                    section.tag.as_str(),
                    "subagent_role" | "peer_role" | "known_environment"
                )
            {
                commands.entity(entity).despawn();
            }
        }
        for section in &context.sections {
            commands.spawn((
                PromptSection::new(section.order, section.tag.clone(), section.text.clone()),
                SectionOf(child),
            ));
        }
    }
}
