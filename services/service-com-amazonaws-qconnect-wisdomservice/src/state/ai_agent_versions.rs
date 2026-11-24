// SPDX-License-Identifier: CC0-1.0

//! The AI-agent VERSION family of this service's state: the record one published version
//! of an AI agent is persisted as, the projections the operations that report one answer
//! out of, and the lookups that reach a version inside the agent that published it.
//!
//! A version is an IMMUTABLE snapshot of the agent's working copy, taken when
//! `CreateAIAgentVersion` published it — the operation "creates an Amazon Q in Connect AI
//! Agent version" of the agent its label names
//! (`smithy_query(query="doc",shape="CreateAIAgentVersion")`) — so a later write to the
//! working copy leaves every published version as it was, and a label carrying a numbered
//! qualifier reads the snapshot rather than the working copy. The snapshot is a whole
//! [`AiAgent`] record, so every member a version reports is rendered by the SAME
//! projection a read of the working copy renders, and the two cannot disagree about what
//! was published. The publish adds ONE entry of its own to that record's tag map, naming
//! the agent the snapshot was taken of — see [`AI_AGENT_VERSION_RESOURCE_TAG_KEY`]; every
//! other member is the agent's as it stood, the caller's own tags included.
//!
//! The numbering is the one the modelled `Version` range (`min 1`) admits: 1 for an
//! agent's first version and the previous number plus 1 for each later publish. A number a
//! delete released is never handed out again, so a version number names one snapshot for
//! as long as the agent lives.

use std::collections::HashMap;

use super::*;
use crate::ops::types::AIAgentVersionSummary;

/// The message a request addressed to a version of an AI agent this store holds no such
/// version of is refused with, worded as this endpoint words the other not-found resources
/// of this family ([`MESSAGE_AI_AGENT_NOT_FOUND`]), with the resource token spelled as the
/// API itself names the resource.
pub const MESSAGE_AI_AGENT_VERSION_NOT_FOUND: &str = "AI Agent version does not exist.";

/// The key of the tag entry the publish writes into a version's tag map, valued with the
/// UNQUALIFIED ARN of the AI agent the version was published from.
///
/// RECORDED BEHAVIOUR of this endpoint, and nothing a caller supplies: in the wire
/// recording of an AI-agent lifecycle (corpus `simparity-wire-aws-v1`, fixture
/// `qconnect-ai-agent-lifecycle`) the client applies no tag at all — its `CreateAIAgent`
/// carries no `tags` member and it never calls `TagResource` — and the publish of that
/// agent answers 200 with `aiAgent.tags` holding exactly this one entry, valued with the
/// agent's own ARN without a version qualifier; the listing of that agent's versions
/// reports the same map under the version's `aiAgentSummary.tags`, while the read of the
/// WORKING COPY in the same recording reports no tag whatever. So the entry belongs to the
/// version RECORD the publish materialises, is rendered out of the agent's own identity,
/// and is no echo of a request: a version reports it whether or not its agent carries tags
/// of its own, and the working copy — and so `ListTagsForResource`, which reads the tags
/// the agent itself holds — never reports it.
///
/// It is written LAST, so a version reports the agent's ARN under this key even where the
/// agent carries a tag of its own under it: the entry states which agent the snapshot was
/// taken of, which is the record's own fact rather than the caller's.
pub const AI_AGENT_VERSION_RESOURCE_TAG_KEY: &str = "resourceArn";

/// The most versions one AI agent may hold at a time; a publish beyond it is refused with
/// the modelled `ServiceQuotaExceededException` (HTTP 402), which `CreateAIAgentVersion`
/// declares.
///
/// CHOSEN: AWS quotas AI agent versions but does not publish the figure. The bound is also
/// what keeps this store finite — a version is released only by `DeleteAIAgentVersion`, by
/// the delete of the agent that published it, or by the delete of the assistant that holds
/// that agent.
pub const MAX_AI_AGENT_VERSIONS_PER_AGENT: usize = 100;

/// The sentence a publish past [`MAX_AI_AGENT_VERSIONS_PER_AGENT`] is refused with,
/// stating the RULE — the ceiling and what to do about it — and carrying nothing from the
/// request or from what the agent already holds, the way [`message_ai_agent_quota`] does
/// for the family with a per-assistant ceiling.
pub fn message_ai_agent_version_quota() -> String {
    format!(
        "The maximum number of AI Agent versions ({MAX_AI_AGENT_VERSIONS_PER_AGENT}) for \
         this AI Agent has been reached. Delete an AI Agent version before creating \
         another."
    )
}

/// The sentence a publish addressed to a built-in ([`AI_AGENT_ORIGIN_SYSTEM`]-origin) AI
/// agent is refused with: the same rule [`message_system_ai_agent_not_modifiable`] states,
/// for the write that would publish a snapshot of a record Amazon Q in Connect provisions
/// with the assistant — the versions of those agents are the service's own, and an account
/// publishes versions of an agent it created.
///
/// CHOSEN WORDING: AWS states the rule (a default AI agent is the service's, not an
/// account's) but publishes no sentence for this refusal, so this one states that rule and
/// carries nothing from the request. The class is the modelled `ValidationException`
/// (HTTP 400), which the model declares for a request the service's own constraints refuse.
pub fn message_system_ai_agent_not_versionable() -> String {
    "A default AI Agent created by Amazon Q in Connect cannot be versioned. Create an AI \
     Agent of your own to publish versions of."
        .to_string()
}

/// The sentence a publish whose `modifiedTime` precondition no longer holds is refused
/// with.
///
/// The member's own documentation states the rule — it "should be specified to avoid
/// version creation when simultaneous update to the underlying AI Agent are possible. The
/// value should be the modifiedTime returned from the request to create or update an AI
/// Agent so that version creation can fail if an update to the AI Agent post the specified
/// modification time has been made"
/// (`smithy_query(query="operation",operation="CreateAIAgentVersion")`) — so the sentence
/// states that, and it carries nothing from the request.
pub fn message_ai_agent_modified_since() -> String {
    "The AI Agent has been modified after the specified modifiedTime, so no version was \
     created. Read the AI Agent again and retry with the modifiedTime that read reports."
        .to_string()
}

/// One published version of an AI agent: the snapshot that was taken of the working copy
/// and the number it was published under.
#[derive(Debug, Clone)]
pub struct AiAgentVersion {
    /// The `versionNumber` this version was published under: 1 for an agent's first
    /// version and the previous one plus 1 for each later publish, which is what the
    /// modelled `Version` range (`min 1`) admits.
    pub version_number: i64,
    /// The working copy as it was when this version was published, carrying the tag map
    /// this version REPORTS: the set the agent held at that instant, plus the entry
    /// [`AI_AGENT_VERSION_RESOURCE_TAG_KEY`] naming the agent it was published from.
    /// Immutable: a
    /// later write to the agent leaves this record alone.
    pub snapshot: AiAgent,
}

impl AiAgentVersion {
    /// The version qualifier that names this version: its number, which is what a label
    /// addresses it by — `<aiAgentId>:<versionNumber>`, the form the modelled
    /// `UuidOrArnOrEitherWithQualifier` admits.
    pub fn qualifier(&self) -> String {
        self.version_number.to_string()
    }

    /// This version as the `AIAgentData` an operation that reports it answers with: the
    /// snapshot's own projection, with the ARN naming the version addressed
    /// ([`version_qualified_arn`]), so a version reports exactly what was published under
    /// it.
    pub fn to_ai_agent_data(&self) -> AIAgentData {
        self.snapshot.to_ai_agent_data_at_version(&self.qualifier())
    }

    /// This version as the `AIAgentVersionSummary` shape `ListAIAgentVersions` answers
    /// with, rendered out of the snapshot.
    pub fn to_summary(&self) -> AIAgentVersionSummary {
        AIAgentVersionSummary {
            ai_agent_summary: Some(
                self.snapshot
                    .to_ai_agent_summary_at_version(&self.qualifier()),
            ),
            version_number: Some(self.version_number),
        }
    }
}

/// What one AI agent has published: the versions it still holds and the highest number it
/// ever handed out.
#[derive(Debug, Clone, Default)]
pub struct AiAgentVersionHistory {
    /// The highest `versionNumber` this agent has ever published, whether that version is
    /// still held or a delete has since released it. The next publish follows THIS number,
    /// so one number names one snapshot for as long as the agent lives.
    pub last_version_number: i64,
    /// The versions this agent still holds, in ascending `versionNumber` order — the order
    /// a publish appends in, so what a listing reports does not depend on map iteration
    /// order.
    pub versions: Vec<AiAgentVersion>,
}

/// What every AI agent of one assistant has published, keyed by `aiAgentId`. An agent that
/// has published nothing holds no entry, which is the correct "no versions" answer for it.
pub type AiAgentVersions = HashMap<String, AiAgentVersionHistory>;

impl Assistant {
    /// Every published version of one AI agent of this assistant, in ascending
    /// `versionNumber` order. Empty while the agent has published none, which is the
    /// correct "no versions" answer for an agent that exists.
    pub fn ai_agent_versions_of(&self, ai_agent_id: &str) -> &[AiAgentVersion] {
        self.ai_agent_versions
            .get(ai_agent_id)
            .map(|history| history.versions.as_slice())
            .unwrap_or_default()
    }

    /// The version of this agent published under `version_number`, or `None` when it holds
    /// none under that number — the lookup a request that names a version resolves through.
    pub fn ai_agent_version(
        &self,
        ai_agent_id: &str,
        version_number: i64,
    ) -> Option<&AiAgentVersion> {
        self.ai_agent_versions_of(ai_agent_id)
            .iter()
            .find(|version| version.version_number == version_number)
    }

    /// The AI agent REVISION a `UuidOrArnOrEitherWithQualifier` label names: the working
    /// copy when the label carries no qualifier or the [`VERSION_QUALIFIER_LATEST`] alias,
    /// and the immutable snapshot of a published version when it carries that version's
    /// number.
    ///
    /// This is the lookup of every operation that REPORTS the agent a label names, since
    /// the model's own wording makes a numbered qualifier name a version of the resource
    /// rather than the resource itself. A number this agent holds no version under — and a
    /// qualifier that is neither a number nor the managed alias — names nothing, so the
    /// caller gets the operation's not-found answer. A caller that WRITES resolves through
    /// [`Self::ai_agent_mut`] instead, which reaches the working copy alone: a version is a
    /// snapshot, not something an update changes.
    pub fn ai_agent_revision(&self, label: &str) -> Option<&AiAgent> {
        let (id, qualifier) = resolve_qualified_id(label);
        match qualifier {
            Some(qualifier) if qualifier != VERSION_QUALIFIER_LATEST => self
                .ai_agent_version(id, qualifier.parse::<i64>().ok()?)
                .map(|version| &version.snapshot),
            _ => self.ai_agents.get(id),
        }
    }

    /// Publish `snapshot` as the agent's next version, answering with the `versionNumber`
    /// it was published under: the previous highest plus 1, and 1 for an agent's first
    /// version.
    ///
    /// `None` — nothing published — when this agent already holds
    /// [`MAX_AI_AGENT_VERSIONS_PER_AGENT`] versions, which is what keeps this store finite.
    ///
    /// What is stored is `snapshot` as it stands, with the one entry the publish
    /// materialises written into its tag map ([`AI_AGENT_VERSION_RESOURCE_TAG_KEY`]), so
    /// every operation that later reports this version renders that map out of the stored
    /// record instead of restating the rule.
    pub fn publish_ai_agent_version(&mut self, mut snapshot: AiAgent) -> Option<i64> {
        let history = self
            .ai_agent_versions
            .entry(snapshot.ai_agent_id.clone())
            .or_default();
        if history.versions.len() >= MAX_AI_AGENT_VERSIONS_PER_AGENT {
            return None;
        }
        // The agent's own ARN, which carries no version qualifier: the entry names the AI
        // agent this snapshot was taken OF, not the version it is published as.
        let published_from = snapshot.ai_agent_arn.clone();
        snapshot
            .tags
            .insert(AI_AGENT_VERSION_RESOURCE_TAG_KEY.to_string(), published_from);
        history.last_version_number += 1;
        let version_number = history.last_version_number;
        history.versions.push(AiAgentVersion {
            version_number,
            snapshot,
        });
        Some(version_number)
    }

    /// Forget one published version of an AI agent, as `DeleteAIAgentVersion` does,
    /// answering whether the agent held a version under that number at all.
    ///
    /// Nothing else is consulted: a version an assistant's `aiAgentConfiguration` still
    /// names is removed all the same, so the binding is left naming a version that is gone
    /// and the assistant keeps the status it had.
    pub fn remove_ai_agent_version(&mut self, ai_agent_id: &str, version_number: i64) -> bool {
        let Some(history) = self.ai_agent_versions.get_mut(ai_agent_id) else {
            return false;
        };
        let before = history.versions.len();
        history
            .versions
            .retain(|version| version.version_number != version_number);
        history.versions.len() != before
    }

    /// Forget everything one AI agent published — what the delete of the agent calls, so an
    /// agent that is gone takes its versions with it.
    pub fn forget_ai_agent_versions_of(&mut self, ai_agent_id: &str) {
        self.ai_agent_versions.remove(ai_agent_id);
    }
}

impl ComAmazonawsQconnectWisdomserviceState {
    /// The AI agent revision a (`assistantId` label, `aiAgentId` label) pair names, with
    /// the same contract as [`Assistant::ai_agent_revision`]: an agent, and every version
    /// of it, is reachable only through its assistant, so neither is found under another
    /// one.
    pub fn ai_agent_revision(&self, assistant_label: &str, agent_label: &str) -> Option<&AiAgent> {
        self.assistant(assistant_label)?
            .ai_agent_revision(agent_label)
    }

    /// Every published version of one AI agent, reached through the assistant the route
    /// named — [`Assistant::ai_agent_versions_of`] for a caller that holds the store.
    pub fn ai_agent_versions_of(
        &self,
        assistant_label: &str,
        ai_agent_id: &str,
    ) -> &[AiAgentVersion] {
        self.assistant(assistant_label)
            .map(|assistant| assistant.ai_agent_versions_of(ai_agent_id))
            .unwrap_or_default()
    }

    /// One published version of one AI agent, reached through the assistant the route
    /// named — [`Assistant::ai_agent_version`] for a caller that holds the store.
    pub fn ai_agent_version(
        &self,
        assistant_label: &str,
        ai_agent_id: &str,
        version_number: i64,
    ) -> Option<&AiAgentVersion> {
        self.assistant(assistant_label)?
            .ai_agent_version(ai_agent_id, version_number)
    }
}
