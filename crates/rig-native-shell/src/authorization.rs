//! Root-user messages establish authority; loaded project instructions restrict it.

use bevy_ecs::system::SystemParam;
use rig_harness::prelude::*;
use rig_harness::rig_core::conversation::{EntryRef, MessageKey, attached_file};
use rig_harness::rig_core::message::{Message, UserContent};
use rig_harness::rig_memory::{COMPACTED, summary_author};
use rig_harness::rig_tools::attach::typed_prompts;
use serde::Serialize;
use std::fmt;
use thiserror::Error;

/// Captures authorization without treating tool output or agent text as permission.
#[derive(SystemParam)]
pub struct Authorization<'w, 's> {
    conversations: Query<'w, 's, (&'static Conversation, &'static AuthorizationGeneration)>,
    spawners: Query<'w, 's, &'static SpawnedBy>,
    instructions:
        Query<'w, 's, (&'static PromptSection, Option<&'static Name>), Without<SectionOf>>,
    scoped_instructions: Query<'w, 's, &'static ScopedProjectInstructions>,
    agent_ids: Query<'w, 's, &'static AgentId>,
    pending: Query<
        'w,
        's,
        (&'static Incoming, &'static ForAgent),
        (Without<DeliveredIn>, Without<Outcome>),
    >,
}

/// Trusted, agent-specific project restrictions for an approved workspace.
///
/// This component carries restrictions, never execution permission. A workspace
/// owner must install the same text in the agent's model prompt after explicit
/// directory trust. It is deliberately not deserializable or saved: restoring a
/// session must establish its workspace scope again. The stable owner identity
/// prevents copying it to another agent from silently changing that agent's
/// restriction evidence.
#[derive(Component, Clone, Debug)]
#[component(immutable)]
pub struct ScopedProjectInstructions {
    owner: AgentId,
    text: String,
}

impl ScopedProjectInstructions {
    /// Construct restrictions in trusted host code, not from a model permission flag.
    pub fn new(owner: AgentId, text: String) -> Self {
        Self { owner, text }
    }

    pub fn owner(&self) -> &AgentId {
        &self.owner
    }

    pub fn text(&self) -> &str {
        &self.text
    }
}

/// An owned snapshot, compared again before executing an approved command.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct UserEvidence {
    #[serde(skip)]
    root: Entity,
    generation: u64,
    message_keys: Vec<MessageKey>,
    messages: Vec<String>,
    project_restrictions: Vec<String>,
    #[serde(skip)]
    limit: usize,
}

impl UserEvidence {
    /// The evidence bound used for this snapshot and every later revalidation.
    pub fn max_bytes(&self) -> usize {
        self.limit
    }
}

/// Numeric accounting of the complete evidence, without message contents.
///
/// Bytes use the same trimmed text and inter-part separators as `typed_prompts`.
/// This is not the size of the serialized model request or its token count.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct EvidenceSize {
    pub required_bytes: usize,
    pub user_messages: usize,
    pub user_message_bytes: usize,
    pub project_restrictions: usize,
    pub project_restriction_bytes: usize,
}

impl fmt::Display for EvidenceSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} bytes ({} user messages: {} bytes; {} project restrictions: {} bytes)",
            self.required_bytes,
            self.user_messages,
            self.user_message_bytes,
            self.project_restrictions,
            self.project_restriction_bytes,
        )
    }
}

#[derive(Debug, Error)]
pub enum AuthorizationError {
    #[error("the agent ancestry is missing or cyclic")]
    Ancestry,
    #[error("the root agent has no user conversation")]
    NoConversation,
    #[error(
        "scoped project instructions do not belong to this agent; workspace trust must be re-established"
    )]
    ScopedInstructions,
    #[error("new user input is pending; review must wait for the updated conversation")]
    PendingInput,
    #[error(
        "authorization and project restrictions exceed the configured evidence bound of {limit} bytes; required {size}; nothing was truncated"
    )]
    TooLarge { limit: usize, size: EvidenceSize },
    #[error("authorization evidence size cannot be represented; escalation was denied")]
    SizeOverflow,
    #[error("there are no user messages to establish authorization")]
    Empty,
    #[error("authorization generation is exhausted; restart this agent before reviewing")]
    Exhausted,
    #[error(
        "earlier user messages were compacted; automatic escalation requires an uncompacted session"
    )]
    Compacted,
}

// Conversation revision also counts kernel-owned tool-start metadata. It is not
// an authorization revision. Component replacement is a distinct revocation,
// including replacing a conversation with a clone containing identical text.
#[derive(Component)]
pub(crate) struct AuthorizationGeneration(u64);

pub(crate) fn track_generation(
    inserted: On<Insert<Conversation>>,
    versions: Query<&AuthorizationGeneration>,
    mut commands: Commands,
) {
    let next = versions
        .get(inserted.entity)
        .map_or(1, |version| version.0.saturating_add(1));
    commands
        .entity(inserted.entity)
        .insert(AuthorizationGeneration(next));
}

impl Authorization<'_, '_> {
    /// Capture root-user text, entry identities, generation and loaded AGENTS restrictions.
    ///
    /// # Errors
    /// Refuses missing/cyclic ancestry, absent, compacted or oversized evidence,
    /// exhausted generations, and pending input. No authorization text is truncated.
    pub fn capture(&self, agent: Entity, limit: usize) -> Result<UserEvidence, AuthorizationError> {
        let (root, conversation, generation) = self.source(agent)?;
        let sections = self.project_sections(agent)?;
        let size = self.measure_conversation(conversation, &sections)?;
        if size.required_bytes > limit {
            return Err(AuthorizationError::TooLarge { limit, size });
        }
        // Only allocate the owned text after measuring all of it. In particular,
        // an oversized single user message must not be copied just to reject it.
        let messages = typed_prompts(conversation).collect();
        Ok(UserEvidence {
            root,
            generation,
            message_keys: conversation
                .entries()
                .filter(|entry| entry.is_users_own())
                .map(|entry| entry.key())
                .collect(),
            messages,
            project_restrictions: sections.into_iter().map(str::to_owned).collect(),
            limit,
        })
    }

    /// Measure complete, uncompacted authorization without copying its text.
    ///
    /// Uses the same provenance, ancestry and pending-input checks as capture.
    /// A successful measurement is not an execution permit or model approval.
    pub fn measure(&self, agent: Entity) -> Result<EvidenceSize, AuthorizationError> {
        let (_, conversation, _) = self.source(agent)?;
        let sections = self.project_sections(agent)?;
        self.measure_conversation(conversation, &sections)
    }

    fn source(&self, agent: Entity) -> Result<(Entity, &Conversation, u64), AuthorizationError> {
        let mut root = agent;
        let mut ancestors = Vec::new();
        while let Ok(&SpawnedBy(parent)) = self.spawners.get(root) {
            if ancestors.contains(&root) || ancestors.len() >= 64 {
                return Err(AuthorizationError::Ancestry);
            }
            ancestors.push(root);
            root = parent;
        }
        if self.pending.iter().any(|(message, &ForAgent(for_agent))| {
            for_agent == root
                && message.author.kind == AuthorKind::User
                && message.author.title.is_none()
        }) {
            return Err(AuthorizationError::PendingInput);
        }
        let (conversation, generation) = self
            .conversations
            .get(root)
            .map_err(|_| AuthorizationError::NoConversation)?;
        if generation.0 == u64::MAX {
            return Err(AuthorizationError::Exhausted);
        }
        // Compaction's original entries live in the session log, not this
        // conversation. Its model-written summary cannot grant permission, and
        // using only the remaining user text could silently drop a restriction.
        let compactor = summary_author();
        if conversation
            .entries()
            .any(|entry| entry.author == Some(&compactor))
            || conversation
                .asides()
                .any(|(_, aside)| aside.kind == COMPACTED && aside.author == compactor)
        {
            return Err(AuthorizationError::Compacted);
        }
        Ok((root, conversation, generation.0))
    }

    fn measure_conversation(
        &self,
        conversation: &Conversation,
        sections: &[&str],
    ) -> Result<EvidenceSize, AuthorizationError> {
        let mut size = EvidenceSize::default();
        for entry in conversation.entries() {
            let mut bytes = 0;
            for text in typed_text(entry) {
                if bytes != 0 {
                    account_bytes(&mut bytes, 2)?; // typed_prompts joins with a blank line
                }
                account_bytes(&mut bytes, text.len())?;
            }
            if bytes != 0 {
                account_bytes(&mut size.user_messages, 1)?;
                account_bytes(&mut size.user_message_bytes, bytes)?;
                account_bytes(&mut size.required_bytes, bytes)?;
            }
        }
        if size.user_messages == 0 {
            return Err(AuthorizationError::Empty);
        }
        for section in sections {
            account_bytes(&mut size.project_restrictions, 1)?;
            account_bytes(&mut size.project_restriction_bytes, section.len())?;
            account_bytes(&mut size.required_bytes, section.len())?;
        }
        Ok(size)
    }

    fn project_sections(&self, agent: Entity) -> Result<Vec<&str>, AuthorizationError> {
        // Match the effective per-agent model prompt. A joined workspace must
        // not be judged under the original launcher's unrelated project rules.
        // The ancestry was validated by source(); repeat its bounded walk only
        // to find the nearest explicitly installed restriction scope.
        let mut current = agent;
        for _ in 0..=64 {
            if let Ok(scope) = self.scoped_instructions.get(current) {
                if self.agent_ids.get(current).ok() != Some(scope.owner()) {
                    return Err(AuthorizationError::ScopedInstructions);
                }
                let mut sections = if scope.text().is_empty() {
                    Vec::new()
                } else {
                    vec![scope.text()]
                };
                sections.extend(self.global_project_sections(true));
                return Ok(sections);
            }
            let Ok(&SpawnedBy(parent)) = self.spawners.get(current) else {
                break;
            };
            current = parent;
        }
        // Ordinary agents keep the existing startup-loaded global restrictions.
        Ok(self.global_project_sections(false))
    }

    fn global_project_sections(&self, exclude_startup: bool) -> Vec<&str> {
        // Only the pinned AgentsMdPlugin's named source is replaced. An unrelated
        // policy plugin may publish additional same-tag global restrictions;
        // changing workspace must never silently remove those constraints.
        let mut sections: Vec<_> = self
            .instructions
            .iter()
            .filter(|(section, name)| {
                section.tag == "project_instructions"
                    && !section.text.is_empty()
                    && !(exclude_startup
                        && name.is_some_and(|name| name.as_str() == "prompt:project_instructions"))
            })
            .map(|(section, _)| section)
            .collect();
        sections.sort_by(|left, right| (left.order, &left.text).cmp(&(right.order, &right.text)));
        sections
            .into_iter()
            .map(|section| section.text.as_str())
            .collect()
    }
}

// Borrowed equivalent of Rig 0.46's typed_prompts, used only for measurement.
// Keep extraction in Rig; a parity test defends trimming, attachments and joins.
// Never strip transport-looking tags: prose is not authorship metadata.
fn typed_text(entry: EntryRef<'_>) -> impl Iterator<Item = &str> {
    let content = match entry.message {
        Message::User { content } if entry.is_users_own() => content.as_slice(),
        _ => &[],
    };
    content.iter().filter_map(|content| match content {
        UserContent::Text(text) if attached_file(&text.text).is_none() => {
            let text = text.text.trim();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    })
}

fn account_bytes(bytes: &mut usize, added: usize) -> Result<(), AuthorizationError> {
    *bytes = bytes
        .checked_add(added)
        .ok_or(AuthorizationError::SizeOverflow)?;
    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod scoped_tests {
    use super::*;
    use bevy_ecs::system::SystemState;

    fn evidence(
        world: &mut World,
        agent: Entity,
        limit: usize,
    ) -> Result<UserEvidence, AuthorizationError> {
        SystemState::<Authorization>::new(world)
            .get(world)
            .unwrap()
            .capture(agent, limit)
    }

    #[test]
    fn scoped_restrictions_replace_globals_and_remain_bounded_for_descendants() {
        let mut world = World::new();
        world.add_observer(track_generation);
        world.spawn((
            Name::new("prompt:project_instructions"),
            PromptSection::new(
                PromptSection::ORDER_PROJECT,
                "project_instructions",
                "Original launch project",
            ),
        ));
        let owner = AgentId("joined-root".into());
        let root = world
            .spawn((
                owner.clone(),
                Conversation::from(
                    rig_harness::rig_core::conversation::Conversation::from_messages([
                        Message::user("Run checks."),
                    ]),
                ),
            ))
            .id();
        let baseline = evidence(&mut world, root, 4096).unwrap();
        assert_eq!(baseline.project_restrictions, ["Original launch project"]);
        world.spawn(PromptSection::new(
            PromptSection::ORDER_PROJECT,
            "project_instructions",
            "Global policy remains",
        ));
        world
            .entity_mut(root)
            .insert(ScopedProjectInstructions::new(
                owner.clone(),
                "Joined worktree rule".into(),
            ));
        let joined = evidence(&mut world, root, 4096).unwrap();
        assert_eq!(
            joined.project_restrictions,
            ["Joined worktree rule", "Global policy remains"]
        );
        assert_ne!(joined, baseline);
        let child = world.spawn(SpawnedBy(root)).id();
        assert_eq!(
            evidence(&mut world, child, 4096)
                .unwrap()
                .project_restrictions,
            joined.project_restrictions
        );
        let mut state = SystemState::<Authorization>::new(&mut world);
        let measured = state.get(&world).unwrap().measure(child).unwrap();
        assert_eq!(
            measured.required_bytes,
            "Run checks.".len() + "Joined worktree rule".len() + "Global policy remains".len()
        );
        assert!(matches!(
            evidence(&mut world, child, measured.required_bytes - 1),
            Err(AuthorizationError::TooLarge { .. })
        ));
        assert!(evidence(&mut world, child, measured.required_bytes).is_ok());
        world
            .entity_mut(root)
            .insert(ScopedProjectInstructions::new(
                owner,
                "Changed joined rule".into(),
            ));
        assert_ne!(evidence(&mut world, child, 4096).unwrap(), joined);
    }

    #[test]
    fn a_copied_scoped_restriction_cannot_fall_back_to_unrelated_globals() {
        let mut world = World::new();
        world.add_observer(track_generation);
        world.spawn((
            Name::new("prompt:project_instructions"),
            PromptSection::new(
                PromptSection::ORDER_PROJECT,
                "project_instructions",
                "Original launch project",
            ),
        ));
        let root = world
            .spawn((
                AgentId("new-root".into()),
                Conversation::from(
                    rig_harness::rig_core::conversation::Conversation::from_messages([
                        Message::user("Run checks."),
                    ]),
                ),
                ScopedProjectInstructions::new(AgentId("other-agent".into()), "Copied rule".into()),
            ))
            .id();
        assert!(matches!(
            evidence(&mut world, root, 4096),
            Err(AuthorizationError::ScopedInstructions)
        ));
        let child = world.spawn(SpawnedBy(root)).id();
        assert!(matches!(
            evidence(&mut world, child, 4096),
            Err(AuthorizationError::ScopedInstructions)
        ));
    }
}
