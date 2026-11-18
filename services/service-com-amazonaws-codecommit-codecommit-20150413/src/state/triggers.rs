// SPDX-License-Identifier: CC0-1.0

//! What a repository TRIGGER is, and what a repository can hold one of.
//!
//! One concern, split out of [`super`] so the state root holds the service's records
//! while the trigger vocabulary lives together: how many triggers a repository may
//! hold and how much each may carry, what a destination ARN has to name
//! ([`trigger_destination`]), the ONE judging of a `RepositoryTriggersList`
//! ([`trigger_configuration_problem`]) that `PutRepositoryTriggers` and
//! `TestRepositoryTriggers` share, the problem it names ([`TriggerProblem`]) with the
//! sentence each carries, and the id a write of a configuration mints
//! ([`trigger_configuration_id`]).
//!
//! What a trigger SENDS once it is stored is [`super::trigger_notifications`]; the
//! mapping from a problem to the modelled error for the member at fault is
//! `crate::trigger_refusal!`, which stays in the state root because a `macro_rules!`
//! is in scope only after it.
//!
//! Everything here is reached through [`super`], which re-exports it, so the module
//! adds no new paths.

use super::*;

/// How many triggers one repository's configuration holds.
///
/// The model declares `MaximumRepositoryTriggersExceededException` ("The number
/// of triggers allowed for the repository was exceeded"), so a bound has to
/// exist; this is the one this emulator holds, and it is the number the
/// CodeCommit quota for triggers on a single repository publishes.
pub const MAX_TRIGGERS_PER_REPOSITORY: usize = 10;

/// How many branches ONE trigger may name.
///
/// Likewise the bound `MaximumBranchesExceededException` ("The number of
/// branches for the trigger was exceeded") exists for, at the number the
/// CodeCommit quota for branches in a single trigger publishes. A trigger that
/// names NO branch is not at this bound: an empty list "applies to all
/// branches" (`smithy_query(query="doc",shape="RepositoryTrigger",member="branches")`).
pub const MAX_BRANCHES_PER_TRIGGER: usize = 10;

/// The longest trigger name, and the longest `customData`, this emulator stores.
///
/// `RepositoryTriggerName` and `RepositoryTriggerCustomData` carry no modelled
/// length (`smithy_query(query="shape",shape="RepositoryTriggerName")`), and both
/// are caller-supplied text kept for the life of the repository, so these are the
/// emulator's OWN bounds on what one configuration can cost. A value past either
/// is refused as the modelled "not valid" error for that member, and every record
/// the configuration holds is released by `DeleteRepository`.
pub const MAX_TRIGGER_NAME_CHARACTERS: usize = 100;
pub const MAX_TRIGGER_CUSTOM_DATA_CHARACTERS: usize = 1_000;

/// The one `RepositoryTriggerEventEnum` value that stands for every repository
/// event. The model documents that it cannot be combined: "The valid value
/// \"all\" cannot be used with any other values."
/// (`smithy_query(query="doc",shape="RepositoryTrigger",member="events")`).
pub const REPOSITORY_TRIGGER_EVENT_ALL: &str = "all";

/// The `RepositoryTriggerEventEnum` values that each name ONE class of reference
/// write (`smithy_query(query="shape",shape="RepositoryTriggerEventEnum")`): a
/// reference the repository did not hold, a reference that moved from one commit to
/// another, and a reference the repository no longer holds.
///
/// Named one by one as well as listed in the enum's value set, because which of
/// them a write of a reference IS decides which triggers that write runs
/// ([`reference_changes::ReferenceEvent`]), so both readings come from one
/// declaration.
pub const REPOSITORY_TRIGGER_EVENT_CREATE_REFERENCE: &str = "createReference";
/// See [`REPOSITORY_TRIGGER_EVENT_CREATE_REFERENCE`].
pub const REPOSITORY_TRIGGER_EVENT_UPDATE_REFERENCE: &str = "updateReference";
/// See [`REPOSITORY_TRIGGER_EVENT_CREATE_REFERENCE`].
pub const REPOSITORY_TRIGGER_EVENT_DELETE_REFERENCE: &str = "deleteReference";

/// The ARN namespaces a trigger destination may name: a repository trigger
/// "runs actions in another service, such as sending a notification through
/// Amazon SNS" (`RepositoryTrigger.events`), and the two targets CodeCommit
/// publishes for a trigger are an Amazon SNS topic and a Lambda function. An ARN
/// of any other service is not a destination a trigger can run.
///
/// Named one by one as well as listed, because what a destination is reached
/// THROUGH follows from its namespace ([`trigger_notifications`]), so both
/// readings come from this one declaration.
pub const TRIGGER_DESTINATION_TOPIC_NAMESPACE: &str = "sns";
/// See [`TRIGGER_DESTINATION_TOPIC_NAMESPACE`].
pub const TRIGGER_DESTINATION_FUNCTION_NAMESPACE: &str = "lambda";
const TRIGGER_DESTINATION_NAMESPACES: &[&str] = &[
    TRIGGER_DESTINATION_TOPIC_NAMESPACE,
    TRIGGER_DESTINATION_FUNCTION_NAMESPACE,
];

/// `RepositoryTriggersListRequiredException`'s message: the shape's own
/// documented sentence, for the one refusal about the trigger LIST rather than
/// about a trigger in it — the list is `@required`, so this is what a request
/// that carries none is answered with.
pub const REPOSITORY_TRIGGERS_LIST_REQUIRED_MESSAGE: &str =
    "The list of triggers for the repository is required, but was not specified.";

/// `InvalidRepositoryTriggerDestinationArnException`'s message: the text the
/// service itself answers a destination that is not an ARN with, which is
/// entirely FIXED — it names neither the offending value, nor the trigger, nor
/// the repository, so nothing about it is read off the request.
///
/// It is not the error shape's documented sentence: that sentence is prose about
/// the error class for a reader of the API reference, and a client branching on
/// the message a trigger refusal carries matches this one.
pub const TRIGGER_DESTINATION_ARN_MALFORMED_MESSAGE: &str =
    "Repository trigger destination arn is incorrectly formatted";

/// Why a trigger in a `RepositoryTriggersList` cannot be stored for a
/// repository. Each variant is the modelled error the trigger operations answer
/// for it, and [`TriggerProblem::message`] is the text that comes with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerProblem {
    /// The configuration holds more triggers than one repository may:
    /// `MaximumRepositoryTriggersExceededException`.
    TooManyTriggers,
    /// A trigger carries no name at all: `RepositoryTriggerNameRequiredException`.
    NameMissing,
    /// A name past [`MAX_TRIGGER_NAME_CHARACTERS`], or one carrying a control
    /// character: `InvalidRepositoryTriggerNameException`.
    NameInvalid,
    /// Two triggers in one configuration carry the same name, which would leave
    /// the repository with two triggers one name addresses:
    /// `InvalidRepositoryTriggerNameException`.
    NameNotUnique,
    /// A trigger carries no destination:
    /// `RepositoryTriggerDestinationArnRequiredException`.
    DestinationArnMissing,
    /// The destination is not the ARN of a service a trigger can run:
    /// `InvalidRepositoryTriggerDestinationArnException`.
    DestinationArnInvalid,
    /// The destination is in another Region than the repository:
    /// `InvalidRepositoryTriggerRegionException`.
    DestinationRegion,
    /// A trigger names no event, so nothing would ever run it:
    /// `RepositoryTriggerEventsListRequiredException`.
    EventsMissing,
    /// An event outside `RepositoryTriggerEventEnum`, or one named twice:
    /// `InvalidRepositoryTriggerEventsException`.
    EventsInvalid,
    /// [`REPOSITORY_TRIGGER_EVENT_ALL`] named beside another event type, which the
    /// one value standing for every event cannot be: the same
    /// `InvalidRepositoryTriggerEventsException`, worded for the combination
    /// rather than for an event name the service does not recognise.
    EventsAllCombined,
    /// A trigger names more branches than one trigger may:
    /// `MaximumBranchesExceededException`.
    TooManyBranches,
    /// A branch name a repository cannot hold:
    /// `InvalidRepositoryTriggerBranchNameException`.
    BranchNameInvalid,
    /// Custom data past [`MAX_TRIGGER_CUSTOM_DATA_CHARACTERS`], or carrying a
    /// control character: `InvalidRepositoryTriggerCustomDataException`.
    CustomDataInvalid,
}

impl TriggerProblem {
    /// The message the error for this problem carries.
    ///
    /// The text is the one the service answers for that error CLASS, stated here
    /// once so the operations that take a `RepositoryTriggersList` cannot word
    /// the same code differently — a malformed destination ARN carries
    /// [`TRIGGER_DESTINATION_ARN_MALFORMED_MESSAGE`], which is fixed text with no
    /// part of it read off the request. For the problems whose own wording is not
    /// recorded anywhere this crate can read it, the text is the documented
    /// sentence of the error shape the problem names (the `//! ## Errors` list in
    /// `src/ops/put_repository_triggers.rs`); the two bounded problems state the
    /// bound they tripped over, as this crate's other quota refusals do.
    pub fn message(self) -> String {
        match self {
            Self::TooManyTriggers => format!(
                "The number of triggers allowed for the repository was exceeded. The maximum \
                 number of triggers for a repository is {MAX_TRIGGERS_PER_REPOSITORY}."
            ),
            Self::NameMissing => {
                "A name for the trigger is required, but was not specified.".to_string()
            }
            Self::NameInvalid => "The name of the trigger is not valid.".to_string(),
            Self::NameNotUnique => "The name of the trigger is not valid. Trigger names must be \
                                    unique within a repository."
                .to_string(),
            Self::DestinationArnMissing => "A destination ARN for the target service for the \
                                            trigger is required, but was not specified."
                .to_string(),
            Self::DestinationArnInvalid => {
                TRIGGER_DESTINATION_ARN_MALFORMED_MESSAGE.to_string()
            }
            Self::DestinationRegion => {
                "The Amazon Web Services Region for the trigger target does not match the Amazon \
                 Web Services Region for the repository. Triggers must be created in the same \
                 Amazon Web Services Region as the target for the trigger."
                    .to_string()
            }
            Self::EventsMissing => {
                "At least one event for the trigger is required, but was not specified."
                    .to_string()
            }
            Self::EventsInvalid => "One or more events specified for the trigger is not valid. \
                                    Check to make sure that all events specified match the \
                                    requirements for allowed events."
                .to_string(),
            // The two reasons an events list is refused are distinguishable only by
            // this text, so the combination has the service's own sentence for it
            // rather than the one that reports an unrecognised event name. It ends
            // without a period, as the service writes it.
            Self::EventsAllCombined => format!(
                "Repository trigger events cannot contain '{REPOSITORY_TRIGGER_EVENT_ALL}' and \
                 additional event types simultaneously"
            ),
            Self::TooManyBranches => format!(
                "The number of branches for the trigger was exceeded. The maximum number of \
                 branches for a trigger is {MAX_BRANCHES_PER_TRIGGER}."
            ),
            Self::BranchNameInvalid => {
                "One or more branch names specified for the trigger is not valid.".to_string()
            }
            Self::CustomDataInvalid => {
                "The custom data provided for the trigger is not valid.".to_string()
            }
        }
    }
}

/// The NAMESPACE and the Region the ARN of a trigger destination names, or `None`
/// where the value is not the ARN of a service a repository trigger can target.
///
/// The ONE reading of a destination ARN in this crate: the judging below asks it
/// which Region a destination is in, and [`trigger_notifications`] asks it what
/// the destination IS, so neither can admit a destination the other does not.
///
/// An ARN is six colon-separated fields, `arn:<partition>:<vendor>:<region>:
/// <account>:<resource>`; a Lambda function's resource portion carries further
/// colons, which `splitn` leaves in the last field. Region and account are
/// checked for FORM only, and the destination itself is not resolved: which
/// topics and functions exist is not something this service holds.
pub fn trigger_destination(raw: &str) -> Option<(&str, &str)> {
    let [prefix, partition, vendor, region, account, resource] =
        raw.splitn(6, ':').collect::<Vec<_>>()[..]
    else {
        return None;
    };
    if prefix != "arn"
        || partition.is_empty()
        // An ARN namespace is a fixed lowercase token, so a value differing in
        // case names another vendor.
        || !TRIGGER_DESTINATION_NAMESPACES.contains(&vendor)
        // The Region field has to be a Region LABEL, under the workspace's one
        // reading of one (`service_common::arn::region_label`): the Region a
        // destination names is what a delivery to it is addressed in
        // ([`trigger_notifications`]), so a value carrying anything a label
        // cannot names no destination at all.
        || ::service_common::arn::region_label(region).is_none()
        || account.len() != ACCOUNT_ID_DIGITS
        || !account.bytes().all(|byte| byte.is_ascii_digit())
        || resource.is_empty()
    {
        return None;
    }
    Some((vendor, region))
}

/// What is wrong with a `RepositoryTriggersList` sent for a repository in
/// `repository_region`, or `None` where every trigger in it can be stored.
///
/// The ONE place this crate judges a trigger configuration: `PutRepositoryTriggers`
/// and `TestRepositoryTriggers` take the same list and model the same errors for
/// it, so neither can accept a configuration the other refuses. Each trigger is
/// judged in the order its members are modelled, and the first problem found is
/// the one answered — a configuration is stored whole or not at all.
pub fn trigger_configuration_problem(
    triggers: &[RepositoryTrigger],
    repository_region: &str,
) -> Option<TriggerProblem> {
    if triggers.len() > MAX_TRIGGERS_PER_REPOSITORY {
        return Some(TriggerProblem::TooManyTriggers);
    }
    let mut named: BTreeSet<&str> = BTreeSet::new();
    for trigger in triggers {
        if trigger.name.trim().is_empty() {
            return Some(TriggerProblem::NameMissing);
        }
        if trigger.name.chars().count() > MAX_TRIGGER_NAME_CHARACTERS
            || trigger.name.chars().any(char::is_control)
        {
            return Some(TriggerProblem::NameInvalid);
        }
        if !named.insert(trigger.name.as_str()) {
            return Some(TriggerProblem::NameNotUnique);
        }
        if trigger.destination_arn.trim().is_empty() {
            return Some(TriggerProblem::DestinationArnMissing);
        }
        match trigger_destination(&trigger.destination_arn) {
            None => return Some(TriggerProblem::DestinationArnInvalid),
            // "Triggers must be created in the same Amazon Web Services Region
            // as the target for the trigger", which is the Region the repository
            // itself lives in.
            Some((_, region)) if region != repository_region => {
                return Some(TriggerProblem::DestinationRegion);
            }
            Some(_) => {}
        }
        if trigger.events.is_empty() {
            return Some(TriggerProblem::EventsMissing);
        }
        let names_all = trigger
            .events
            .iter()
            .any(|event| event == REPOSITORY_TRIGGER_EVENT_ALL);
        // Each event a trigger names is one of the modelled repository events, and
        // names it ONCE: a list that repeats a value names the same event twice,
        // which also keeps what one trigger stores bounded by the enum's size.
        let mut distinct: BTreeSet<&str> = BTreeSet::new();
        if trigger.events.iter().any(|event| {
            !crate::ops::types::REPOSITORY_TRIGGER_EVENT_ENUM_VALUES.contains(&event.as_str())
                || !distinct.insert(event.as_str())
        }) {
            return Some(TriggerProblem::EventsInvalid);
        }
        // "The valid value \"all\" cannot be used with any other values": a list
        // naming it beside another event type is refused for the COMBINATION, which
        // the service words differently from an event it does not recognise.
        if names_all && trigger.events.len() > 1 {
            return Some(TriggerProblem::EventsAllCombined);
        }
        let branches = trigger.branches.as_deref().unwrap_or_default();
        if branches.len() > MAX_BRANCHES_PER_TRIGGER {
            return Some(TriggerProblem::TooManyBranches);
        }
        // A trigger names branches of the repository it is on, so a name it
        // names is one a repository could hold: git's own reference-name rule,
        // the same one `CreateBranch` holds a name to.
        if branches.iter().any(|branch| !is_valid_branch_name(branch)) {
            return Some(TriggerProblem::BranchNameInvalid);
        }
        if trigger.custom_data.as_ref().is_some_and(|data| {
            data.chars().count() > MAX_TRIGGER_CUSTOM_DATA_CHARACTERS
                || data.chars().any(char::is_control)
        }) {
            return Some(TriggerProblem::CustomDataInvalid);
        }
    }
    None
}

/// A trigger configuration's `configurationId`: the same kind of version-8 UUID
/// as every other id this service mints, over the account, the repository the
/// configuration is on and the instant it was written. A later write of the same
/// repository's configuration therefore mints a different id, which is what
/// "the system-generated unique ID for the create or update operation" is.
pub fn trigger_configuration_id(repository: &str, written: SystemTime) -> String {
    let nanos = nanos_since_epoch(written);
    uuid_v8(&format!(
        "codecommit/{}/{repository}/triggers/{nanos}",
        ::engine::emulator_account_id()
    ))
}
