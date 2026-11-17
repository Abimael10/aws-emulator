// SPDX-License-Identifier: CC0-1.0

//! How Config refuses a call made on a resource an AWS SERVICE owns.
//!
//! Two resource classes of this service are not the account's to act on even though the
//! account can see them: the configuration recorder Config keeps for a service it is
//! linked to, and the Config rule a service created
//! (`ConfigRule$CreatedBy`: "Service principal name of the service that created the
//! rule … populated only if the service-linked rule is created by a service"). Each
//! refusal IDENTIFIES what was refused — the operation the caller used, the recorder it
//! named, the service that owns the rule — so a caller holding several of them can tell
//! which call and which resource each one is about. The sentences live together here
//! because they are one rule worded per class, and because the operations that answer
//! them must not drift apart.

use super::*;

/// The clause every `UnmodifiableEntityException` about a SERVICE-LINKED configuration
/// recorder opens with: the operation that was refused, and the recorder it was refused
/// on.
///
/// `operation` is the operation's own name as the API spells it (the handler's
/// `Operation::NAME`) and `recorder_name` is the recorder the request named, so the
/// opening reads the same however the caller reached the recorder.
fn does_not_apply_to_service_linked(operation: &str, recorder_name: &str) -> String {
    format!(
        "{operation} operation does not apply to a service-linked configuration recorder \
         {recorder_name}"
    )
}

/// How `StartConfigurationRecorder` and `StopConfigurationRecorder` word
/// `UnmodifiableEntityException` for a SERVICE-LINKED configuration recorder.
///
/// The reason these two operations do not apply is the property the recorder has: it is
/// ALWAYS RECORDING (`UnmodifiableEntityException`: "For StartConfigurationRecorder and
/// StopConfigurationRecorder, you will see this exception because these operations do
/// not affect service-linked configuration recorders. Service-linked configuration
/// recorders are always recording"), so there is neither a recording to start nor one
/// the caller may stop. RECORDED behaviour renders that as the operation and the
/// recorder followed by the reason, which is the form this builds.
pub fn service_linked_recorder_always_recording_message(
    operation: &str,
    recorder_name: &str,
) -> String {
    format!(
        "{} because it's always recording",
        does_not_apply_to_service_linked(operation, recorder_name)
    )
}

/// How `DeleteConfigurationRecorder` words `UnmodifiableEntityException` for a
/// SERVICE-LINKED configuration recorder.
///
/// The reason this operation does not apply is that such a recorder is a PROTECTED
/// resource of the service it is linked to, and the delete that does apply to it is
/// `DeleteServiceLinkedConfigurationRecorder` (`UnmodifiableEntityException`: "For
/// DeleteConfigurationRecorder, you will see this exception because you cannot use this
/// operation to delete a service-linked configuration recorder. Use the
/// DeleteServiceLinkedConfigurationRecorder operation"). RECORDED behaviour renders that
/// as the same opening clause as
/// [`service_linked_recorder_always_recording_message`], followed by the rejection and
/// the operation to use instead.
pub fn service_linked_recorder_is_protected_message(
    operation: &str,
    recorder_name: &str,
) -> String {
    format!(
        "{}. The request was rejected because service-linked configuration recorders are \
         protected AWS resources. Please use DeleteServiceLinkedConfigurationRecorder API \
         to attempt the deletion.",
        does_not_apply_to_service_linked(operation, recorder_name)
    )
}

/// How Config refuses a call that would delete or redefine a Config rule an AWS SERVICE
/// owns, or destroy the verdicts recorded against it.
///
/// `owner` is the owning service as the rule itself records it
/// ([`ConfigRuleEntry::owning_service`]) — never a value the caller sent, because what
/// is refused is who owns the rule rather than anything about the request. RECORDED
/// behaviour: the refusal states that a service owns the rule, names that owner, and
/// says the caller holds no permission to act on it.
pub fn service_owned_config_rule_denied_message(owner: &str) -> String {
    format!("An AWS service owns {owner}. You do not have permissions to take action on this rule.")
}

impl ConfigRuleEntry {
    /// The AWS service that owns this rule, or `None` for a rule this account created
    /// itself.
    ///
    /// The ONE predicate for that question, so every operation that refuses a call on a
    /// rule this account does not own agrees with what `DescribeConfigRules` reports in
    /// `CreatedBy` ([`ConfigRuleEntry::created_by`]).
    pub fn owning_service(&self) -> Option<&str> {
        self.created_by.as_deref()
    }
}
