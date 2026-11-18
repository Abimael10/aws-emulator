// SPDX-License-Identifier: CC0-1.0

//! What a `Task` state's EVENT PUBLICATION carries beyond the document the state built,
//! and what a publication that REJECTED an entry makes of the task.
//!
//! The publication itself is made by THIS service, as the execution's work: the entries
//! are the state's own document, but the call is Step Functions' — so every event it puts
//! names the two resources the publication is attributable to, the execution that made it
//! and the state machine that execution runs ([`publication_request`]). `Resources` is
//! "Amazon Web Services resources, identified by Amazon Resource Name (ARN), which the
//! event primarily concerns. Any number, including zero, may be present"
//! (`smithy_query(query="shape",shape="PutEventsRequestEntry",model_path="services/
//! service-com-amazonaws-eventbridge-awsevents/smithy-model/
//! com-amazonaws-eventbridge-awsevents.json")`), which is what lets a consumer attribute
//! a delivered event to the run that produced it and what a rule pattern constraining
//! `resources` is matched against.
//!
//! The answer is read the same way: a publication EventBridge served but which rejected
//! an entry did not do the task's work, so it fails the task
//! ([`rejected_entries`]) instead of being reported as a result whose
//! `FailedEntryCount` nobody looked at.
//!
//! Both are facts of the integration rather than of one `Resource` spelling, so they hold
//! for either family that names it — the optimized `events:putEvents` and the AWS SDK
//! `aws-sdk:eventbridge:putEvents` — and for the callback form of each, whose call is the
//! same call.
//!
//! Split out of `super`'s file so that file stays inside the project's length limit.

use super::*;

/// The ARN namespace of Amazon EventBridge, whose buses a `Task` state publishes onto
/// (`awsService.arnNamespace`), and the operation of its API a publication is one call of.
const EVENTS_NAMESPACE: &str = "events";
/// See [`EVENTS_NAMESPACE`].
const PUT_EVENTS_OPERATION: &str = "PutEvents";

/// The member of that operation's input carrying the events to publish, and the member of
/// each one of them naming the resources the event concerns — both as the invoked model
/// spells them (`smithy_query(query="operation",operation="PutEvents",model_path=
/// "services/service-com-amazonaws-eventbridge-awsevents/smithy-model/
/// com-amazonaws-eventbridge-awsevents.json")`). One spelling serves both integration
/// families: the AWS SDK one's parameters are the operation's own members, and the
/// optimized one's are spelled the same way — the PascalCase this integration's entry in
/// [`APIS`] states.
const ENTRIES_MEMBER: &str = "Entries";
/// See [`ENTRIES_MEMBER`].
const RESOURCES_MEMBER: &str = "Resources";

/// The member of that operation's OUTPUT carrying "the number of failed entries", which is
/// what says a publication rejected an entry (`PutEventsResponse$FailedEntryCount`).
const FAILED_ENTRY_COUNT_MEMBER: &str = "FailedEntryCount";

/// The error name a `Task` whose publication rejected an entry fails with, which a
/// definition's `Retry` and `Catch` match on.
///
/// Named the way every error of this integration is ([`Api::task_error`]): the service as
/// Step Functions names it, then what went wrong — here the entry the publication would
/// not take, which is not a refusal of the call and so has no SDK exception of its own.
pub const FAILED_ENTRY_ERROR: &str = "EventBridge.FailedEntry";

/// Whether `call` is an event publication: the `PutEvents` operation of EventBridge's
/// API, named by either integration family.
fn publishes_events(call: &Call) -> bool {
    call.api.namespace == EVENTS_NAMESPACE && call.operation == PUT_EVENTS_OPERATION
}

/// `request` as the publication is really MADE — every entry of it naming the execution
/// `execution_arn` of the state machine `state_machine_arn` among its `Resources` — or
/// `None` where there is nothing to add, so the caller sends the document the state built.
///
/// `None` for every call that is not a publication, and for a publication whose request
/// carries no `Entries` LIST: a request the invoked operation cannot be called with is
/// refused by that operation, and nothing is decorated into it here.
///
/// The ARNs are ADDED to the resources the entry names rather than put in their place: the
/// member is the caller's to fill ("any number, including zero, may be present"), and what
/// this service contributes is the attribution of the publication to the run that made it.
/// An entry already naming one of them keeps the one entry it has, so the list carries each
/// ARN once however the state wrote it.
#[must_use]
pub fn publication_request(
    call: &Call,
    request: &Value,
    execution_arn: &str,
    state_machine_arn: &str,
) -> Option<Value> {
    if !publishes_events(call) {
        return None;
    }
    let entries = request.get(ENTRIES_MEMBER)?.as_array()?;
    let members = request.as_object()?;
    let published: Vec<Value> = entries
        .iter()
        .map(|entry| attributed(entry, execution_arn, state_machine_arn))
        .collect();
    let mut decorated = members.clone();
    decorated.insert(ENTRIES_MEMBER.to_string(), Value::Array(published));
    Some(Value::Object(decorated))
}

/// One entry carrying `execution_arn` and `state_machine_arn` among the resources it
/// names, and the entry UNCHANGED where that cannot be said of it: an entry that is not an
/// object, and one whose `Resources` is not a list, name no resource list to add to — both
/// are requests the invoked operation answers for, so neither is rewritten here.
fn attributed(entry: &Value, execution_arn: &str, state_machine_arn: &str) -> Value {
    let Some(members) = entry.as_object() else {
        return entry.clone();
    };
    let mut named = match members.get(RESOURCES_MEMBER) {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(named)) => named.clone(),
        Some(_) => return entry.clone(),
    };
    for arn in [execution_arn, state_machine_arn] {
        let carried = Value::String(arn.to_string());
        if !named.contains(&carried) {
            named.push(carried);
        }
    }
    let mut members = members.clone();
    members.insert(RESOURCES_MEMBER.to_string(), Value::Array(named));
    Value::Object(members)
}

/// The failure a publication that REJECTED an entry is to the `Task` state that made it,
/// or `None` for a publication every entry of which was taken — and for every call that is
/// not a publication.
///
/// The call was SERVED: `PutEvents` answers an entry it would not take on that entry's own
/// row rather than refusing the request, so the work the task asked for — publishing those
/// events — was not done, and the state fails instead of carrying on with a result whose
/// `FailedEntryCount` nobody read. The entries the publication DID take are published
/// already: the answer is what it is, and the failure is about what became of the batch.
///
/// The cause is WHAT THE PUBLICATION ANSWERED, exactly as this task's own result would have
/// reported it: the answer's `FailedEntryCount` and its per-entry rows, each carrying the
/// `ErrorCode` and message the invoked service gave for the entry it would not take. So a
/// `Catch` on this error reads which entries were rejected and why out of the cause,
/// in the publication's own words, and nothing is said that the answer did not say.
#[must_use]
pub fn rejected_entries(call: &Call, answered: &Value) -> Option<Failed> {
    if !publishes_events(call) {
        return None;
    }
    let rejected = answered
        .get(FAILED_ENTRY_COUNT_MEMBER)
        .and_then(Value::as_i64)
        .unwrap_or_default();
    if rejected <= 0 {
        return None;
    }
    Some(Failed {
        error: FAILED_ENTRY_ERROR.to_string(),
        cause: answered.to_string(),
    })
}
