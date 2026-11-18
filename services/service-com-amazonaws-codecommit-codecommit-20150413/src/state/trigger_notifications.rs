// SPDX-License-Identifier: CC0-1.0

//! The information a repository trigger SENDS to its destination, and the one place
//! this crate sends it.
//!
//! # The rule this module implements
//!
//! A trigger exists to be acted on: `RepositoryTrigger$destinationArn` is "the ARN of
//! the resource that is the target for a trigger (for example, the ARN of a topic in
//! Amazon SNS)", and `RepositoryTrigger$events` are "the repository events that cause
//! the trigger to RUN ACTIONS IN ANOTHER SERVICE, such as sending a notification
//! through Amazon SNS"
//! (`smithy_query(query="doc",shape="RepositoryTrigger",member="events")`). So a write
//! that moves a reference a stored trigger is in scope for — one whose `branches` name
//! the moved branch, or which names none at all, since "if you specify an empty array,
//! the trigger applies to all branches" — sends ONE record to the destination that
//! trigger names, labelled [`TRIGGER_EVENT_NAME`]; a write of a reference no stored
//! trigger is in scope for sends nothing. Which triggers one write is in scope for is
//! [`super::reference_changes`].
//!
//! `TestRepositoryTriggers` "tests the functionality of repository triggers by SENDING
//! INFORMATION TO THE TRIGGER TARGET. If real data is available in the repository, the
//! test sends data from the last commit. If no data is available, sample data is
//! generated"
//! (`smithy_query(query="operation",operation="TestRepositoryTriggers",model_path="services/service-com-amazonaws-codecommit-codecommit-20150413/smithy-model/com-amazonaws-codecommit-codecommit-20150413.json")`).
//! So each trigger a caller submits is tested by DELIVERING one record to the
//! destination it names, and what the test reports per trigger is whether that
//! delivery was accepted: a test that only read the configuration back would leave
//! both sentences unimplemented — a subscriber of the configured topic would receive
//! nothing at all, and a destination nothing can be sent to could not be told apart
//! from one that received the record. That record is labelled as the test it is
//! ([`TRIGGER_TEST_EVENT_NAME`]), so a consumer tells a probe apart from a reference
//! that really moved.
//!
//! # What the destination receives
//!
//! One record, in the envelope a CodeCommit trigger delivers
//! ([`TriggerNotification::payload`]): a single `Records` entry naming the repository
//! the trigger is on (`eventSourceARN`), the trigger that produced it
//! (`eventTriggerName`), the configuration it belongs to (`eventTriggerConfigId`), the
//! identity the call was made by (`userIdentityARN`), the trigger's own `customData`
//! when it carries any, and the reference the record is about with the commit that
//! reference points at (`codecommit.references`). Every value is read off the STORED
//! repository, so a consumer of the record and a later `GetBranch`/`GetCommit` of the
//! same repository report one fact — except for the repository that holds no commit at
//! all, which has no real data to send and is sent the generated sample the operation's
//! own documentation calls for ([`sample_commit_id`]).
//!
//! # How it is sent
//!
//! Over the IN-PROCESS callback into this same emulator, the way every emulated service
//! reaches another: Amazon SNS through the one client construction this workspace holds
//! ([`::service_common::sns_client`]) and Lambda through [`lambda_client`]. Nothing here
//! opens a socket or leaves the host — the ARN a caller supplied is not a URL this code
//! dials, it names a destination the request is routed to INSIDE this process, so the
//! topic or function has to be one this emulator itself holds or its own service answers
//! that it does not exist. What the destination IS, and which Region of this emulator the
//! delivery is addressed in, come from this crate's ONE reading of a destination ARN
//! ([`super::trigger_destination`]) — the same one the configuration judging uses — and
//! that reading admits a Region only as a label under the workspace's one Region-label
//! grammar, so no part of a request can be an address.
//!
//! Every trigger a caller submits is answered for: a destination that cannot be sent to
//! at all is reported as a trigger whose record was not sent, rather than left out of
//! both of the operation's lists. A destination's refusal is not an operation failure
//! either — it is what that trigger's `failedExecutions` entry reports, carrying the
//! answer the service that owns the destination gave
//! ([`TriggerNotification::failure_message`]).

use super::*;

/// The `eventSource` every record CodeCommit sends to a trigger destination carries:
/// the service principal form a consumer matches a CodeCommit record on.
pub const TRIGGER_EVENT_SOURCE: &str = "aws:codecommit";

/// The `eventName` of a record about references that REALLY changed, which is what a
/// repository trigger is about: `RepositoryTriggerEventEnum` is the reference events
/// `createReference`, `updateReference` and `deleteReference`
/// (`smithy_query(query="shape",shape="RepositoryTriggerEventEnum")`).
pub const TRIGGER_EVENT_NAME: &str = "ReferenceChanges";

/// The `eventName` of the record a TEST of a trigger sends.
///
/// `TestRepositoryTriggers` "tests the functionality of repository triggers by sending
/// information to the trigger target. If real data is available in the repository, the
/// test sends data from the last commit. If no data is available, sample data is
/// generated"
/// (`smithy_query(query="operation",operation="TestRepositoryTriggers")`): the record it
/// sends is a PROBE rather than a report of a reference that moved — the reference and
/// commit it names may be data the test generated, and the repository's references are
/// exactly where they were. So it is labelled as the test it is, and a consumer that
/// branches on `eventName` tells the probe apart from [`TRIGGER_EVENT_NAME`] rather
/// than building, notifying or deploying for a change nothing made.
pub const TRIGGER_TEST_EVENT_NAME: &str = "TriggerEventTest";

/// The `eventVersion` of the record layout rendered here.
pub const TRIGGER_EVENT_VERSION: &str = "1.0";

/// The branch a record names for a repository that holds no reference at all.
///
/// Such a repository has no branch to name, so the reference is part of the SAMPLE
/// data the operation generates when no real data is available. `main` is the branch
/// this crate already treats as the one a client checks out
/// (`crate::service::git_http`'s choice of a default branch prefers it), so the sample
/// names the reference a first commit would create.
pub const SAMPLE_BRANCH: &str = "main";

/// What a destination is, and so how its record reaches it.
///
/// Read off the destination ARN's NAMESPACE, which is the one thing that says which
/// service owns the destination ([`super::trigger_destination`]), so the two
/// namespaces a trigger may name and the two ways a record is sent cannot drift
/// apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// An Amazon SNS topic: the record is PUBLISHED to it.
    Topic,
    /// A Lambda function: the record is the PAYLOAD it is invoked with.
    Function,
}

impl Destination {
    /// The destination an ARN namespace names, or `None` for a namespace no trigger
    /// destination belongs to.
    fn of(vendor: &str) -> Option<Self> {
        match vendor {
            super::TRIGGER_DESTINATION_TOPIC_NAMESPACE => Some(Self::Topic),
            super::TRIGGER_DESTINATION_FUNCTION_NAMESPACE => Some(Self::Function),
            _ => None,
        }
    }
}

/// One record waiting to be sent to one trigger's destination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TriggerNotification {
    /// The `name` of the trigger this record is sent for: what the operation reports
    /// in `successfulExecutions` or in a `failedExecutions` entry.
    pub trigger_name: String,
    /// The `destinationArn` the trigger named.
    pub destination_arn: String,
    /// WHERE the record goes: what the destination is, and the Region of this emulator
    /// the delivery is addressed in — the one the destination's own ARN names.
    ///
    /// `None` for an ARN that names no destination a trigger can be sent to, which the
    /// configuration judging ([`super::trigger_configuration_problem`]) has already
    /// refused, so a stored configuration cannot hold one. Such a trigger is still
    /// REPORTED — as one whose record could not be sent — rather than left out of both
    /// of the operation's lists, because every trigger a caller submits is answered
    /// for.
    addressed: Option<(Destination, String)>,
    /// The record itself, as the destination receives it.
    pub payload: String,
}

/// The record each trigger of `triggers` is tested with, ONE PER SUBMITTED TRIGGER, in
/// the order they were submitted.
///
/// Read off the STORED repository under its guard, so nothing is sent that the
/// repository does not say; the sending itself happens with the guard released
/// ([`TriggerNotification::send`]).
#[must_use]
pub fn test_notifications(
    repository: &Repository,
    triggers: &[RepositoryTrigger],
    caller_arn: &str,
    at: SystemTime,
) -> Vec<TriggerNotification> {
    triggers
        .iter()
        .map(|trigger| test_notification(repository, trigger, caller_arn, at))
        .collect()
}

/// The record one trigger is tested with.
fn test_notification(
    repository: &Repository,
    trigger: &RepositoryTrigger,
    caller_arn: &str,
    at: SystemTime,
) -> TriggerNotification {
    let (reference, commit_id) = tested_reference(repository, trigger);
    notification(
        repository,
        trigger,
        &RecordedChange {
            configuration_id: &configuration_id(repository, at),
            event_name: TRIGGER_TEST_EVENT_NAME,
            reference: &reference,
            commit_id: &commit_id,
            caller_arn: Some(caller_arn),
        },
        at,
    )
}

/// What one record is ABOUT: the configuration it belongs to, the class of event it
/// reports, the reference and commit it names, and whose call produced it.
///
/// Stated once, so the two things a trigger's destination is sent a record for — a
/// reference that really moved and a test of the trigger — differ only in the values
/// here and are rendered by the one [`record`] that writes the envelope.
pub struct RecordedChange<'a> {
    /// The `eventTriggerConfigId` the record carries: the id of the configuration the
    /// trigger belongs to.
    pub configuration_id: &'a str,
    /// The record's `eventName`: [`TRIGGER_EVENT_NAME`] for a reference that changed,
    /// [`TRIGGER_TEST_EVENT_NAME`] for a test of the trigger.
    pub event_name: &'a str,
    /// The FULLY QUALIFIED reference the record is about, as
    /// `codecommit.references[0].ref` names it ([`qualified_reference`]).
    pub reference: &'a str,
    /// The commit that reference points at.
    pub commit_id: &'a str,
    /// The identity whose call produced the record (`userIdentityARN`), for a transport
    /// that carries one: the git transport authenticates with git credentials rather
    /// than a caller's own, so a push names none and the member is left out rather than
    /// sent empty.
    pub caller_arn: Option<&'a str>,
}

/// One record of `change`, addressed to the destination `trigger` names.
///
/// The ONE construction of a record waiting to be sent, so what a reference change and
/// a test deliver reach their destination the same way.
#[must_use]
pub fn notification(
    repository: &Repository,
    trigger: &RepositoryTrigger,
    change: &RecordedChange<'_>,
    at: SystemTime,
) -> TriggerNotification {
    let addressed = super::trigger_destination(&trigger.destination_arn).and_then(
        |(vendor, region)| Destination::of(vendor).map(|kind| (kind, region.to_string())),
    );
    TriggerNotification {
        trigger_name: trigger.name.clone(),
        destination_arn: trigger.destination_arn.clone(),
        addressed,
        payload: record(repository, trigger, change, at),
    }
}

/// The reference the test sends data from, and the commit it points at.
///
/// "If real data is available in the repository, the test sends data from the LAST
/// COMMIT": among the branches the trigger is in scope for — the ones it names, or
/// every branch for a trigger that names none, since "if you specify an empty array,
/// the trigger applies to all branches"
/// (`smithy_query(query="doc",shape="RepositoryTrigger",member="branches")`) — the one
/// whose tip is the repository's most recently written commit. "If no data is
/// available, sample data is generated": a repository with no commit on any branch in
/// scope is sent its first named branch, or [`SAMPLE_BRANCH`], at a generated commit id
/// ([`sample_commit_id`]).
fn tested_reference(repository: &Repository, trigger: &RepositoryTrigger) -> (String, String) {
    let named = trigger.branches.as_deref().unwrap_or_default();
    let last_commit = repository
        .branches
        .values()
        .filter(|branch| named.is_empty() || named.contains(&branch.name))
        .filter_map(|branch| {
            let commit = repository.commits.get(&branch.commit_id)?;
            Some((commit.sequence, branch))
        })
        .max_by_key(|(sequence, _)| *sequence)
        .map(|(_, branch)| branch);
    match last_commit {
        Some(branch) => (
            qualified_reference(&branch.name),
            branch.commit_id.clone(),
        ),
        None => {
            let branch = named
                .first()
                .map(String::as_str)
                .or(repository.default_branch.as_deref())
                .unwrap_or(SAMPLE_BRANCH);
            (
                qualified_reference(branch),
                sample_commit_id(repository, branch),
            )
        }
    }
}

/// The commit id of GENERATED sample data: the id a record names for a repository
/// that holds no commit to send data from.
///
/// A git object name is a 40-character SHA-1 digest ([`git_object`]), so the sample is
/// one too, derived from the repository and the reference it is generated for: a second
/// test of the same empty repository therefore sends the same sample, and the id names
/// no object the repository holds, which is exactly what "no data is available" means.
#[must_use]
pub fn sample_commit_id(repository: &Repository, branch: &str) -> String {
    ::hex::encode(git_object::sha1_digest(
        format!("codecommit sample commit/{}/{branch}", repository.arn).as_bytes(),
    ))
}

/// The record one trigger's destination receives.
fn record(
    repository: &Repository,
    trigger: &RepositoryTrigger,
    change: &RecordedChange<'_>,
    at: SystemTime,
) -> String {
    let mut changed = ::serde_json::Map::new();
    changed.insert(
        "commit".to_string(),
        ::serde_json::Value::String(change.commit_id.to_string()),
    );
    changed.insert(
        "ref".to_string(),
        ::serde_json::Value::String(change.reference.to_string()),
    );
    let mut entry = ::serde_json::Map::new();
    let mut put = |member: &str, value: &str| {
        entry.insert(
            member.to_string(),
            ::serde_json::Value::String(value.to_string()),
        );
    };
    put("awsRegion", &repository.region);
    put("eventId", &event_id(repository, trigger, at));
    put("eventName", change.event_name);
    put("eventSource", TRIGGER_EVENT_SOURCE);
    put("eventSourceARN", &repository.arn);
    put("eventTime", &event_time(at));
    put("eventTriggerConfigId", change.configuration_id);
    put("eventTriggerName", &trigger.name);
    put("eventVersion", TRIGGER_EVENT_VERSION);
    if let Some(caller_arn) = change.caller_arn {
        put("userIdentityARN", caller_arn);
    }
    // "Any custom data associated with the trigger to be INCLUDED IN THE INFORMATION
    // SENT TO THE TARGET of the trigger"
    // (`smithy_query(query="doc",shape="RepositoryTrigger",member="customData")`), so a
    // trigger carrying none leaves the member out rather than sending an empty one.
    if let Some(custom_data) = trigger.custom_data.as_deref() {
        put("customData", custom_data);
    }
    // One record, carrying the whole event: the part numbering says so, and a consumer
    // reads it to know whether it holds everything.
    entry.insert(
        "eventPartNumber".to_string(),
        ::serde_json::Value::from(1),
    );
    entry.insert("eventTotalParts".to_string(), ::serde_json::Value::from(1));
    entry.insert(
        "codecommit".to_string(),
        ::serde_json::json!({ "references": [::serde_json::Value::Object(changed)] }),
    );
    ::serde_json::json!({ "Records": [::serde_json::Value::Object(entry)] }).to_string()
}

/// The `eventTriggerConfigId` a record carries: the id of the configuration the
/// repository holds, when a write has stored one.
///
/// A test is made against a configuration a caller SUBMITTED rather than one this
/// operation stores, so a repository no write has configured has no stored id to name
/// and the record carries the id this test's own instant mints
/// ([`super::trigger_configuration_id`]) — the same form every other id of a
/// configuration takes. Nothing is persisted either way: a test leaves the
/// configuration the repository holds exactly as it was.
fn configuration_id(repository: &Repository, at: SystemTime) -> String {
    repository
        .trigger_configuration
        .as_ref()
        .map(|configuration| configuration.configuration_id.clone())
        .unwrap_or_else(|| trigger_configuration_id(&repository.name, at))
}

/// The `eventId` of one record: unique per record, so a consumer can tell two
/// deliveries apart — over the repository, the trigger and the instant it was sent.
fn event_id(repository: &Repository, trigger: &RepositoryTrigger, at: SystemTime) -> String {
    super::uuid_v8(&format!(
        "codecommit/{}/triggers/{}/{}",
        repository.arn,
        trigger.name,
        super::nanos_since_epoch(at)
    ))
}

/// The `eventTime` of a record whose instant the date-time form cannot render: the
/// epoch, in that same form.
///
/// A clock does not reach outside the range RFC 3339 covers, so this is the
/// unreachable arm of [`event_time`] — rendered as a valid instant rather than as an
/// empty member, because a consumer reads `eventTime` as a date and an empty string is
/// not one.
const EPOCH_EVENT_TIME: &str = "1970-01-01T00:00:00Z";

/// The `eventTime` of one record: the instant it was sent, in the one date-time form
/// this emulator writes into a document it renders itself.
fn event_time(at: SystemTime) -> String {
    to_date_time(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_else(|_| EPOCH_EVENT_TIME.to_string())
}

impl TriggerNotification {
    /// Send this record to the destination it names, answering why the trigger did not
    /// run when it could not be sent.
    ///
    /// Call it with NO store guard held: the delivery reaches another service's handler,
    /// which takes that service's own lock.
    pub async fn send(
        &self,
        callback: &::smithy_server_runtime::EmbeddedEmulator,
    ) -> Result<(), String> {
        let Some((destination, region)) = self.addressed.as_ref() else {
            // The configuration judging has already refused such a destination, so no
            // configuration holds one; the trigger is answered for all the same.
            return Err(format!(
                "Unable to send the notification to {}: the destination is not the Amazon \
                 Resource Name of an Amazon SNS topic or of a Lambda function",
                self.destination_arn
            ));
        };
        match destination {
            Destination::Topic => self.publish(callback, region).await,
            Destination::Function => self.invoke(callback, region).await,
        }
    }

    /// Publish the record to the Amazon SNS topic the destination names.
    async fn publish(
        &self,
        callback: &::smithy_server_runtime::EmbeddedEmulator,
        region: &str,
    ) -> Result<(), String> {
        ::service_common::sns_client::sns_client(callback, region)
            .publish()
            .topic_arn(&self.destination_arn)
            .message(&self.payload)
            .send()
            .await
            .map(|_| ())
            .map_err(|error| self.failure_message(&error))
    }

    /// Invoke the Lambda function the destination names with the record as its payload.
    ///
    /// ASYNCHRONOUSLY: a trigger notification is not a call whose answer CodeCommit
    /// waits for, and "a success response (202 Accepted) indicates that the request is
    /// queued for invocation" (`Invoke$InvocationType`'s `Event` value). So what a test
    /// reports for a function destination is whether Lambda ACCEPTED the invocation,
    /// which is what CodeCommit can observe of it.
    async fn invoke(
        &self,
        callback: &::smithy_server_runtime::EmbeddedEmulator,
        region: &str,
    ) -> Result<(), String> {
        lambda_client(callback, region)
            .invoke()
            .function_name(&self.destination_arn)
            .invocation_type(::aws_sdk_lambda::types::InvocationType::Event)
            .payload(::aws_sdk_lambda::primitives::Blob::new(
                self.payload.clone().into_bytes(),
            ))
            .send()
            .await
            .map(|_| ())
            .map_err(|error| self.failure_message(&error))
    }

    /// The `failureMessage` of this trigger's `failedExecutions` entry: the destination
    /// the record could not be sent to, and the answer the service that owns it gave.
    ///
    /// The destination's own refusal is what says why the trigger did not run — a topic
    /// or function that does not exist, or one that refused the delivery — so it is
    /// reported rather than replaced by wording of this service's own.
    fn failure_message<E>(&self, error: &E) -> String
    where
        E: ::aws_smithy_types::error::metadata::ProvideErrorMetadata,
    {
        use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;
        let code = ProvideErrorMetadata::code(error).unwrap_or_default();
        let message = ProvideErrorMetadata::message(error).unwrap_or_default();
        let reason = match (code.is_empty(), message.is_empty()) {
            (false, false) => format!("{code}: {message}"),
            (false, true) => code.to_string(),
            (true, false) => message.to_string(),
            (true, true) => "the destination did not answer".to_string(),
        };
        ::tracing::debug!(
            destination = %self.destination_arn,
            trigger = %self.trigger_name,
            "a repository trigger's destination did not accept the notification sent to it"
        );
        format!(
            "Unable to send the notification to {}: {reason}",
            self.destination_arn
        )
    }
}

/// The Lambda client a function destination is invoked through: the IN-PROCESS callback
/// into this same emulator.
///
/// The `endpoint_url` is an ADDRESS the in-process router reads, never a host any
/// resolver looks up, and `region` is the label read out of the destination's own ARN,
/// so a caller cannot steer that authority. There is nothing to retry, time out, cache
/// an identity for or watch for a stalled stream over that transport — each would need a
/// sleep implementation this client is not given — and the credentials the callback hands
/// over are the emulator's own, which only sign this operation's in-process calls and are
/// never put in a response or a log.
fn lambda_client(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_lambda::Client {
    let config = ::aws_sdk_lambda::Config::builder()
        .behavior_version(::aws_sdk_lambda::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_lambda::config::Region::new(region.to_string()))
        .endpoint_url(format!(
            "https://{}.{region}.amazonaws.com",
            super::TRIGGER_DESTINATION_FUNCTION_NAMESPACE
        ))
        .retry_config(::aws_sdk_lambda::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_lambda::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_lambda::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_lambda::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_lambda::Client::from_conf(config)
}

/// How many trigger deliveries run at once, over the whole store.
///
/// A write that moves a reference a trigger is in scope for starts one delivery, so a
/// caller driving writes drives tasks: this permit is what keeps the work in flight
/// bounded however many writes arrive, exactly as this crate bounds what one request
/// can grow. A delivery waits for a permit rather than being dropped, because a record
/// the configuration owes a destination is a record a consumer is owed.
const CONCURRENT_DELIVERIES: usize = 16;

/// The permits those deliveries take, one per delivery in flight.
static DELIVERIES: ::tokio::sync::Semaphore =
    ::tokio::sync::Semaphore::const_new(CONCURRENT_DELIVERIES);

/// Send `notifications` to the destinations they name, on a task of its OWN, for a
/// handler whose answer does not wait for the delivery.
///
/// A reference change's records reach the destination AFTER the write that caused them
/// — which is what a consumer polling a subscribed queue observes — so they are handed
/// to their own task rather than awaited under this service's store guard: a guard held
/// across an await would make a handler's future non-`Send`, and the write's answer
/// waits for nothing that is not part of it. That is also why a destination's refusal
/// is TRACED here instead of reported: unlike `TestRepositoryTriggers`, whose whole
/// answer is the per-trigger report, a write that moved a reference has already
/// happened and nothing about it changes with what the destination said.
///
/// Nothing to send starts no task; a task that starts takes one of
/// [`CONCURRENT_DELIVERIES`] permits before it sends, and sends in the order the
/// records were rendered.
pub fn deliver_all_detached(
    notifications: Vec<TriggerNotification>,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
) {
    if notifications.is_empty() {
        return;
    }
    let callback = callback.clone();
    ::tokio::spawn(async move {
        // A closed semaphore is not something this process creates, so a permit that
        // cannot be taken leaves the records unsent rather than sent twice.
        let Ok(_permit) = DELIVERIES.acquire().await else {
            return;
        };
        for notification in notifications {
            if let Err(message) = notification.send(&callback).await {
                ::tracing::warn!(
                    trigger = %notification.trigger_name,
                    destination = %notification.destination_arn,
                    %message,
                    "a repository trigger's record could not be sent to its destination"
                );
            }
        }
    });
}
