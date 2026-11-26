// SPDX-License-Identifier: CC0-1.0

//! The AI-prompt VERSION family of this service's state: the record one published version
//! of an AI prompt is persisted as, the projections the operations that report one answer
//! out of, and the lookups that reach a version inside the prompt that published it.
//!
//! A version is an IMMUTABLE snapshot of the prompt's working copy, taken when
//! `CreateAIPromptVersion` published it — the operation "creates an Amazon Q in Connect AI
//! Prompt version" of the prompt its label names
//! (`smithy_query(query="doc",shape="CreateAIPromptVersion")`) — so a later write to the
//! working copy leaves every published version as it was, and a label carrying a numbered
//! qualifier reads the snapshot rather than the working copy. The snapshot is a whole
//! [`AiPrompt`] record, so every member a version reports is rendered by the SAME
//! projection a read of the working copy renders, and the two cannot disagree about what
//! was published.
//!
//! The numbering is the one the modelled `Version` range (`min 1`) admits: 1 for a
//! prompt's first version and the previous number plus 1 for each later publish. The
//! number is allocated on this record, under the store's own lock, so two callers
//! publishing at once are handed two different numbers rather than one number twice; a
//! number is never handed out twice for as long as the prompt lives.

use std::collections::HashMap;

use super::*;
use crate::ops::types::{AIPromptData, AIPromptVersionSummary};

/// The message a request addressed to a version of an AI prompt this store holds no such
/// version of is refused with, worded as this endpoint words the other not-found resources
/// of this family ([`MESSAGE_AI_PROMPT_NOT_FOUND`]), with the resource token spelled as
/// the API itself names the resource.
pub const MESSAGE_AI_PROMPT_VERSION_NOT_FOUND: &str = "AI Prompt version does not exist.";

/// The most versions one AI prompt may hold at a time; a publish beyond it is refused with
/// the modelled `ServiceQuotaExceededException` (HTTP 402), which `CreateAIPromptVersion`
/// declares.
///
/// CHOSEN: AWS quotas AI prompt versions but does not publish the figure, so this is the
/// figure the sibling version family already uses
/// ([`MAX_AI_AGENT_VERSIONS_PER_AGENT`]). The bound is also what keeps this store finite —
/// a version is released only by the delete of the prompt that published it, or by the
/// delete of the assistant that holds that prompt.
pub const MAX_AI_PROMPT_VERSIONS_PER_PROMPT: usize = 100;

/// The sentence a publish past [`MAX_AI_PROMPT_VERSIONS_PER_PROMPT`] is refused with,
/// stating the RULE — the ceiling and what to do about it — and carrying nothing from the
/// request or from what the prompt already holds, the way
/// [`message_ai_agent_version_quota`] does for the sibling version family.
pub fn message_ai_prompt_version_quota() -> String {
    format!(
        "The maximum number of AI Prompt versions ({MAX_AI_PROMPT_VERSIONS_PER_PROMPT}) for \
         this AI Prompt has been reached. Delete an AI Prompt version before creating \
         another."
    )
}

/// The sentence a publish whose `modifiedTime` precondition no longer holds is refused
/// with.
///
/// The member states the instant "the AI Prompt was last modified"
/// (`smithy_query(query="operation",operation="CreateAIPromptVersion")`), which a caller
/// reads off the create or update of the prompt and sends back to say WHICH working copy
/// it means to publish — the rule the sibling publish's own member spells out — so a
/// publish of a working copy that has changed since is refused rather than numbering a
/// version of something the caller has not seen. The sentence states that rule and carries
/// nothing from the request.
pub fn message_ai_prompt_modified_since() -> String {
    "The AI Prompt has been modified after the specified modifiedTime, so no version was \
     created. Read the AI Prompt again and retry with the modifiedTime that read reports."
        .to_string()
}

/// One published version of an AI prompt: the snapshot that was taken of the working copy
/// and the number it was published under.
#[derive(Debug, Clone)]
pub struct AiPromptVersion {
    /// The `versionNumber` this version was published under: 1 for a prompt's first
    /// version and the previous one plus 1 for each later publish, which is what the
    /// modelled `Version` range (`min 1`) admits.
    pub version_number: i64,
    /// The working copy as it was when this version was published. Immutable: a later
    /// write to the prompt leaves this record alone.
    pub snapshot: AiPrompt,
}

impl AiPromptVersion {
    /// The version qualifier that names this version: its number, which is what a label
    /// addresses it by — `<aiPromptId>:<versionNumber>`, the form the modelled
    /// `UuidOrArnOrEitherWithQualifier` admits.
    pub fn qualifier(&self) -> String {
        self.version_number.to_string()
    }

    /// This version as the `AIPromptData` an operation that reports it answers with: the
    /// snapshot's own projection, with the ARN naming the version addressed
    /// ([`version_qualified_arn`]), so a version reports exactly what was published under
    /// it.
    pub fn to_ai_prompt_data(&self) -> AIPromptData {
        self.snapshot.to_ai_prompt_data_at_version(&self.qualifier())
    }

    /// This version as the `AIPromptVersionSummary` shape `ListAIPromptVersions` answers
    /// with, rendered out of the snapshot.
    pub fn to_summary(&self) -> AIPromptVersionSummary {
        AIPromptVersionSummary {
            ai_prompt_summary: Some(
                self.snapshot
                    .to_ai_prompt_summary_at_version(&self.qualifier()),
            ),
            version_number: Some(self.version_number),
        }
    }
}

/// What one AI prompt has published: the versions it still holds and the highest number it
/// ever handed out.
#[derive(Debug, Clone, Default)]
pub struct AiPromptVersionHistory {
    /// The highest `versionNumber` this prompt has ever published, whether that version is
    /// still held or a delete has since released it. The next publish follows THIS number,
    /// so one number names one snapshot for as long as the prompt lives.
    pub last_version_number: i64,
    /// The versions this prompt still holds, in ascending `versionNumber` order — the
    /// order a publish appends in, so what a listing reports does not depend on map
    /// iteration order.
    pub versions: Vec<AiPromptVersion>,
}

/// What every AI prompt of one assistant has published, keyed by `aiPromptId`. A prompt
/// that has published nothing holds no entry, which is the correct "no versions" answer
/// for it.
pub type AiPromptVersions = HashMap<String, AiPromptVersionHistory>;

impl Assistant {
    /// Every published version of one AI prompt of this assistant, in ascending
    /// `versionNumber` order. Empty while the prompt has published none, which is the
    /// correct "no versions" answer for a prompt that exists.
    pub fn ai_prompt_versions_of(&self, ai_prompt_id: &str) -> &[AiPromptVersion] {
        self.ai_prompt_versions
            .get(ai_prompt_id)
            .map(|history| history.versions.as_slice())
            .unwrap_or_default()
    }

    /// The version of this prompt published under `version_number`, or `None` when it
    /// holds none under that number — the lookup a request that names a version resolves
    /// through.
    pub fn ai_prompt_version(
        &self,
        ai_prompt_id: &str,
        version_number: i64,
    ) -> Option<&AiPromptVersion> {
        self.ai_prompt_versions_of(ai_prompt_id)
            .iter()
            .find(|version| version.version_number == version_number)
    }

    /// The AI prompt REVISION a `UuidOrArnOrEitherWithQualifier` label names: the working
    /// copy when the label carries no qualifier or the [`VERSION_QUALIFIER_LATEST`] alias,
    /// and the immutable snapshot of a published version when it carries that version's
    /// number.
    ///
    /// This is the lookup of every operation that REPORTS the prompt a label names, since
    /// the model's own wording makes a numbered qualifier name a version of the resource
    /// rather than the resource itself. A number this prompt holds no version under — and
    /// a qualifier that is neither a number nor the managed alias — names nothing, so the
    /// caller gets the operation's not-found answer. A caller that WRITES resolves through
    /// [`Self::ai_prompt_mut`] instead, which reaches the working copy alone: a version is
    /// a snapshot, not something an update changes.
    pub fn ai_prompt_revision(&self, label: &str) -> Option<&AiPrompt> {
        let (id, qualifier) = resolve_qualified_id(label);
        match qualifier {
            Some(qualifier) if qualifier != VERSION_QUALIFIER_LATEST => self
                .ai_prompt_version(id, qualifier.parse::<i64>().ok()?)
                .map(|version| &version.snapshot),
            _ => self.ai_prompts.get(id),
        }
    }

    /// Publish `snapshot` as the prompt's next version, answering with the `versionNumber`
    /// it was published under: the previous highest plus 1, and 1 for a prompt's first
    /// version.
    ///
    /// The allocation and the append happen together here, and every caller reaches this
    /// through the store's lock, so two publishes of one prompt are handed two different
    /// numbers however they are interleaved.
    ///
    /// `None` — nothing published — when this prompt already holds
    /// [`MAX_AI_PROMPT_VERSIONS_PER_PROMPT`] versions, which is what keeps this store
    /// finite.
    pub fn publish_ai_prompt_version(&mut self, snapshot: AiPrompt) -> Option<i64> {
        let history = self
            .ai_prompt_versions
            .entry(snapshot.ai_prompt_id.clone())
            .or_default();
        if history.versions.len() >= MAX_AI_PROMPT_VERSIONS_PER_PROMPT {
            return None;
        }
        history.last_version_number += 1;
        let version_number = history.last_version_number;
        history.versions.push(AiPromptVersion {
            version_number,
            snapshot,
        });
        Some(version_number)
    }

    /// Forget everything one AI prompt published — what the delete of the prompt calls, so
    /// a prompt that is gone takes its versions with it.
    pub fn forget_ai_prompt_versions_of(&mut self, ai_prompt_id: &str) {
        self.ai_prompt_versions.remove(ai_prompt_id);
    }
}

impl ComAmazonawsQconnectWisdomserviceState {
    /// The AI prompt revision a (`assistantId` label, `aiPromptId` label) pair names, with
    /// the same contract as [`Assistant::ai_prompt_revision`]: a prompt, and every version
    /// of it, is reachable only through its assistant, so neither is found under another
    /// one.
    pub fn ai_prompt_revision(
        &self,
        assistant_label: &str,
        prompt_label: &str,
    ) -> Option<&AiPrompt> {
        self.assistant(assistant_label)?
            .ai_prompt_revision(prompt_label)
    }

    /// One published version of one AI prompt, reached through the assistant the route
    /// named — [`Assistant::ai_prompt_version`] for a caller that holds the store.
    pub fn ai_prompt_version(
        &self,
        assistant_label: &str,
        ai_prompt_id: &str,
        version_number: i64,
    ) -> Option<&AiPromptVersion> {
        self.assistant(assistant_label)?
            .ai_prompt_version(ai_prompt_id, version_number)
    }
}
