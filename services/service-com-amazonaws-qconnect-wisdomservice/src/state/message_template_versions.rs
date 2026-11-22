// SPDX-License-Identifier: CC0-1.0

//! The message-template VERSION family of this service's state: the record one published
//! version of a message template is persisted as, the projections the operations that
//! report one answer out of, and the lookups that reach a version inside its template.
//!
//! A version is an IMMUTABLE snapshot of the template's working copy, taken when
//! `CreateMessageTemplateVersion` published it — "Creates a new Amazon Q in Connect
//! message template version from the current content and configuration of a message
//! template. Versions are immutable and monotonically increasing"
//! (`smithy_query(query="doc",shape="CreateMessageTemplateVersion")`) — so a later write
//! to the working copy leaves every published version as it was. The snapshot is a whole
//! [`MessageTemplate`] record, so every member a version reports is rendered by the SAME
//! projection a read of the working copy renders, and the two cannot disagree about what
//! was published.
//!
//! At most one version of a template is activated at a time: activating one deactivates
//! the one that was active, which is what the `$ACTIVE_VERSION` qualifier
//! ([`VERSION_QUALIFIER_ACTIVE`]) resolves to.

use std::collections::HashMap;

use super::*;
use crate::ops::types::{ExtendedMessageTemplateData, MessageTemplateVersionSummary};

/// The message a request addressed to a version of a message template this store holds no
/// such version of is refused with, worded as this endpoint words the other not-found
/// resources of this family ([`MESSAGE_MESSAGE_TEMPLATE_NOT_FOUND`]), with the resource
/// token spelled as the API itself names the resource.
pub const MESSAGE_MESSAGE_TEMPLATE_VERSION_NOT_FOUND: &str =
    "Message template version does not exist.";

/// The most versions one message template may hold at a time; a publish beyond it is
/// refused with the modelled `ServiceQuotaExceededException` (HTTP 402), which
/// `CreateMessageTemplateVersion` declares.
///
/// CHOSEN: AWS quotas message-template versions but does not publish the figure. The bound
/// is also what keeps this store finite — a version is released only by the delete of the
/// template that holds it, or of the knowledge base that holds the template.
pub const MAX_MESSAGE_TEMPLATE_VERSIONS_PER_TEMPLATE: usize = 100;

/// The sentence a publish past [`MAX_MESSAGE_TEMPLATE_VERSIONS_PER_TEMPLATE`] is refused
/// with, stating the RULE — the ceiling and what to do about it — and carrying nothing
/// from the request or from what the template already holds.
pub fn message_message_template_version_quota() -> String {
    format!(
        "The maximum number of message template versions \
         ({MAX_MESSAGE_TEMPLATE_VERSIONS_PER_TEMPLATE}) for this message template has been \
         reached. Delete a message template version before creating another."
    )
}

/// The sentence a publish whose `messageTemplateContentSha256` is not the checksum of the
/// working copy is refused with.
///
/// The operation's own documentation states the rule — "An error is displayed if the
/// supplied messageTemplateContentSha256 is different from the
/// messageTemplateContentSha256 of the message template with $LATEST qualifier"
/// (`smithy_query(query="doc",shape="CreateMessageTemplateVersion")`) — so the sentence
/// states that, and its one variable part is the checksum the working copy actually
/// carries, read off the stored record.
pub fn message_content_checksum_mismatch(content_sha256: &str) -> String {
    format!(
        "The supplied messageTemplateContentSha256 does not match the \
         messageTemplateContentSha256 {content_sha256} of the message template referenced by \
         the {VERSION_QUALIFIER_LATEST} qualifier."
    )
}

/// The expression this endpoint ENFORCES for a message-template label on an operation whose
/// own documentation says it "cannot contain any qualifier": the model's `UuidOrArn` shape
/// (`smithy_query(query="shape",shape="UuidOrArn")`), which admits a uuid or an ARN and no
/// `:<QUALIFIER>` group at all.
///
/// The member itself targets `UuidOrArnOrEitherWithQualifier`, whose own expression DOES
/// admit `(:[A-Z0-9_$]+){0,1}`, so the stricter requirement is the service's rather than the
/// model's — and it is the one a refusal of such a label states, as the expression that
/// member's value had to satisfy.
const MESSAGE_TEMPLATE_ID_WITHOUT_QUALIFIER_PATTERN: &str = concat!(
    "^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$|",
    "^arn:[a-z-]*?:wisdom:[a-z0-9-]*?:[0-9]{12}:[a-z-]*?/",
    "[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}",
    "(?:/[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}){0,2}$",
);

/// The message a request whose resource label carries a version qualifier, on an operation
/// whose own documentation says it may not, is refused with.
///
/// Those operations address the working copy (or name the version in a member of their
/// own), and each documents the label as one that "cannot contain any qualifier"
/// (`smithy_query(query="operation",operation="ActivateMessageTemplate")` and the other
/// message-template writes), so the label is held to the expression that admits no
/// qualifier ([`MESSAGE_TEMPLATE_ID_WITHOUT_QUALIFIER_PATTERN`]) and a value carrying one is
/// refused as that member's PATTERN-constraint violation — the same clause, envelope and
/// `@pattern` rendering this endpoint states for every other constrained member
/// (`service_common::validation_message`, `service_common::validation::unanchored`), rather
/// than prose of its own. Its variable parts are the member the request carried the value in
/// and the expression that member's value had to satisfy.
///
/// The value half is DROPPED: the requirement is the expression the label had to match, and
/// a clause that states it names the member and the constraint alone.
pub fn message_qualifier_not_accepted(member: &str) -> String {
    let violation = ::smithy_server_runtime::Violation {
        path: member.to_string(),
        kind: ::smithy_server_runtime::ViolationKind::Pattern(::service_common::validation::unanchored(
            MESSAGE_TEMPLATE_ID_WITHOUT_QUALIFIER_PATTERN,
        )),
        value: None,
        sensitive: false,
    };
    let clause = ::service_common::validation_message::valueless_clause(&violation)
        .unwrap_or_else(|| violation.message());
    ::service_common::validation_message::envelope_over(std::slice::from_ref(&clause))
        .unwrap_or(clause)
}

/// The sentence a `DeactivateMessageTemplate` naming a version that is not the template's
/// ACTIVE one is refused with.
///
/// The operation deactivates "a specific version of the Amazon Q in Connect message
/// template" whose status is active — the counterpart of `ActivateMessageTemplate`, after
/// which "you can no longer use the $ACTIVE_VERSION qualifier to reference the version in
/// active status" (`smithy_query(query="doc",shape="DeactivateMessageTemplate")`) — so a
/// version the template never activated, or one a later activation deactivated, is not a
/// version this call can deactivate. Its one variable part is the `versionNumber` the
/// request named.
pub fn message_message_template_version_not_active(version_number: i64) -> String {
    format!("Version {version_number} is not an active version")
}

/// One published version of a message template: the snapshot that was taken of the working
/// copy, the number it was published under, and whether it is the activated one.
#[derive(Debug, Clone)]
pub struct MessageTemplateVersion {
    /// The `versionNumber` this version was published under: 1 for a template's first
    /// version and the previous one plus 1 for each later publish, which is what
    /// "monotonically increasing" states and what the modelled `Version` range (`min 1`)
    /// admits.
    pub version_number: i64,
    /// Whether this version is the ACTIVATED one. At most one version of a template
    /// carries it: `ActivateMessageTemplate` sets it here and clears it on the version
    /// that held it.
    pub is_active: bool,
    /// The working copy as it was when this version was published. Immutable: a later
    /// write to the template leaves this record alone.
    pub snapshot: MessageTemplate,
}

impl MessageTemplateVersion {
    /// The version qualifier that names this version: its number, which is what a label
    /// addresses it by — "you can reference a specific version of the message template by
    /// passing in <message-template-id>:<versionNumber>".
    pub fn qualifier(&self) -> String {
        self.version_number.to_string()
    }

    /// The ARN of this version: the template's own ARN with this version's qualifier
    /// appended ([`version_qualified_arn`]), which is the `messageTemplateArn` every
    /// operation that answers about a version reports.
    pub fn message_template_arn(&self) -> String {
        version_qualified_arn(&self.snapshot.message_template_arn, &self.qualifier())
    }

    /// This version as the `ExtendedMessageTemplateData` shape a read of it answers with:
    /// the snapshot's own projection, carrying this version's number and activation state,
    /// so a version reports exactly what was published under it.
    pub fn to_extended_message_template_data(&self) -> ExtendedMessageTemplateData {
        self.snapshot.to_extended_message_template_data(
            &self.qualifier(),
            Some(self.version_number),
            Some(self.is_active),
        )
    }

    /// This version as the `MessageTemplateVersionSummary` shape
    /// `ListMessageTemplateVersions` answers with, rendered out of the snapshot.
    pub fn to_summary(&self) -> MessageTemplateVersionSummary {
        MessageTemplateVersionSummary {
            message_template_arn: self.message_template_arn(),
            message_template_id: self.snapshot.message_template_id.clone(),
            knowledge_base_arn: self.snapshot.knowledge_base_arn.clone(),
            knowledge_base_id: self.snapshot.knowledge_base_id.clone(),
            name: self.snapshot.name.clone(),
            // The channel this version applies to, which is the one the snapshot's channel
            // subtype names ([`channel_of_subtype`]).
            channel: Some(channel_of_subtype(&self.snapshot.channel_subtype)),
            channel_subtype: self.snapshot.channel_subtype.clone(),
            is_active: self.is_active,
            version_number: self.version_number,
        }
    }
}

/// Every published version of one message template, keyed by `messageTemplateId` and held
/// in ascending `versionNumber` order — the order a publish appends in, so what a listing
/// reports does not depend on map iteration order.
pub type MessageTemplateVersions = HashMap<String, Vec<MessageTemplateVersion>>;

/// What the store found when `DeactivateMessageTemplate` asked it to deactivate one
/// published version of a template.
///
/// The three answers are what the operation's two refusals and its success are decided
/// from, so the handler never has to re-read the record to tell them apart: a version the
/// template never published names no resource, and one it published but did not activate is
/// not a version in active status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageTemplateDeactivation {
    /// The version was the template's ACTIVE one, and is active no longer.
    Deactivated,
    /// The template published this version, but it was not the active one: nothing was
    /// written.
    NotActive,
    /// The template published no version under that number: nothing was written.
    NoSuchVersion,
}

impl ComAmazonawsQconnectWisdomserviceState {
    /// Every published version of one message template, in ascending `versionNumber`
    /// order. Empty while the template has published none, which is the correct "no
    /// versions" answer for a template that exists.
    pub fn message_template_versions_of(&self, message_template_id: &str) -> &[MessageTemplateVersion] {
        self.message_template_versions
            .get(message_template_id)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    /// The version of this template published under `version_number`, or `None` when it
    /// published none under that number — the lookup a request that names a version
    /// resolves through.
    pub fn message_template_version(
        &self,
        message_template_id: &str,
        version_number: i64,
    ) -> Option<&MessageTemplateVersion> {
        self.message_template_versions_of(message_template_id)
            .iter()
            .find(|version| version.version_number == version_number)
    }

    /// The ACTIVATED version of this template, which is what the `$ACTIVE_VERSION`
    /// qualifier names, or `None` while no version of it is activated.
    pub fn active_message_template_version(
        &self,
        message_template_id: &str,
    ) -> Option<&MessageTemplateVersion> {
        self.message_template_versions_of(message_template_id)
            .iter()
            .find(|version| version.is_active)
    }

    /// The most recently published version of this template, or `None` while it has
    /// published none.
    pub fn latest_message_template_version(
        &self,
        message_template_id: &str,
    ) -> Option<&MessageTemplateVersion> {
        self.message_template_versions_of(message_template_id).last()
    }

    /// Publish `snapshot` as the template's next version, answering with the
    /// `versionNumber` it was published under: the previous one plus 1, and 1 for a
    /// template's first version.
    ///
    /// The published version is NOT activated: activation is `ActivateMessageTemplate`'s
    /// own call.
    pub fn publish_message_template_version(&mut self, snapshot: MessageTemplate) -> i64 {
        let versions = self
            .message_template_versions
            .entry(snapshot.message_template_id.clone())
            .or_default();
        let version_number = versions
            .last()
            .map_or(1, |previous| previous.version_number + 1);
        versions.push(MessageTemplateVersion {
            version_number,
            is_active: false,
            snapshot,
        });
        version_number
    }

    /// Activate one published version of a template, deactivating the version that was
    /// active — "After the version is activated, the previous active version will be
    /// deactivated automatically" — and answering whether the template published a version
    /// under that number at all. A template that published none is left untouched.
    pub fn activate_message_template_version(
        &mut self,
        message_template_id: &str,
        version_number: i64,
    ) -> bool {
        let Some(versions) = self.message_template_versions.get_mut(message_template_id) else {
            return false;
        };
        if !versions
            .iter()
            .any(|version| version.version_number == version_number)
        {
            return false;
        }
        for version in versions.iter_mut() {
            version.is_active = version.version_number == version_number;
        }
        true
    }

    /// Deactivate one published version of a template, answering what the store found —
    /// whether the template published a version under that number at all, and whether that
    /// version was the ACTIVE one, which is the only version this call deactivates
    /// ([`MessageTemplateDeactivation`]). Nothing is written unless the version was active,
    /// and the other versions are left as they were either way.
    pub fn deactivate_message_template_version(
        &mut self,
        message_template_id: &str,
        version_number: i64,
    ) -> MessageTemplateDeactivation {
        let Some(versions) = self.message_template_versions.get_mut(message_template_id) else {
            return MessageTemplateDeactivation::NoSuchVersion;
        };
        let Some(version) = versions
            .iter_mut()
            .find(|version| version.version_number == version_number)
        else {
            return MessageTemplateDeactivation::NoSuchVersion;
        };
        if !version.is_active {
            return MessageTemplateDeactivation::NotActive;
        }
        version.is_active = false;
        MessageTemplateDeactivation::Deactivated
    }

    /// Forget every published version of one message template — what the delete of the
    /// template, and of the knowledge base that holds it, calls so a template that is gone
    /// takes its versions with it.
    pub fn forget_message_template_versions_of(&mut self, message_template_id: &str) {
        self.message_template_versions.remove(message_template_id);
    }
}
