// SPDX-License-Identifier: CC0-1.0

//! The ONE place this service publishes an event onto the account's default event bus.
//!
//! CodeCommit publishes the changes it makes without any opt-in: there is no
//! notification configuration to switch on and no trigger to register — a change this
//! service accepts is published onto the account's DEFAULT bus, under the `source` this
//! service owns ([`EVENT_SOURCE`]) and the `detail-type` of the thing that changed. Each
//! envelope names the REPOSITORY's ARN as the single entry of `resources`, so a rule
//! whose pattern pins `resources` to one repository's ARN is delivered that repository's
//! events and no other repository's.
//!
//! Both kinds of change this service publishes — a repository's references
//! ([`super::repository_state_events`]) and a pull request's lifecycle
//! ([`super::pull_request_events`]) — are rendered into the one [`PublishedEvent`] here
//! and published by the one routine below, so the two cannot drift apart in how they
//! reach the bus, how many publications one caller can have in flight, or what happens
//! when the bus does not answer.
//!
//! Publication happens only AFTER the write that made the change was accepted: a refused
//! call changes nothing, so it publishes nothing. It is handed to a task of its own
//! ([`publish_all_detached`]) rather than awaited while the handler still holds this
//! service's store guard, because delivery to a rule's targets is not part of the answer
//! the write gives its caller. EventBridge is reached the way every emulated service
//! reaches another — the AWS SDK over the framework's in-process callback, through the
//! ONE client construction [`::service_common::eventbridge_client`] — so nothing here
//! opens a socket. The only thing the address is built from is the Region the repository
//! lives in, so no part of a caller's request is a destination. Publication is BEST
//! EFFORT: an emulator with no EventBridge mounted has nowhere to publish, so a failure
//! is traced and the operation's own answer stands.

/// The `source` of every event CodeCommit publishes.
///
/// EventBridge names the publishing service in an event's `source` as `aws.<service>`,
/// and this service's namespace is the one its ARNs carry
/// (`smithy_query(query="service_info", model_path="services/service-com-amazonaws-codecommit-codecommit-20150413/smithy-model/com-amazonaws-codecommit-codecommit-20150413.json")`:
/// `arnNamespace` `codecommit`).
pub const EVENT_SOURCE: &str = "aws.codecommit";

/// The event bus this service publishes onto.
///
/// The events an AWS service generates go to the account's default event bus, which is
/// the bus a rule carrying no `EventBusName` is created on.
pub const DEFAULT_EVENT_BUS: &str = "default";

/// One rendered event waiting to be published onto the default bus.
///
/// Rendered from STORED state while the store guard is held, so every value in it is one
/// a later read of the same repository reports, and published once the guard is gone.
#[derive(Debug, Clone)]
#[must_use = "a change this service accepted is published onto the default event bus"]
pub struct PublishedEvent {
    /// The repository's ARN, which the envelope carries as the ONE entry of `resources`:
    /// the event names the resource it is about, which is what a rule filtering on
    /// `resources` reads.
    pub repository_arn: String,
    /// The Region the event is published in: the Region the repository lives in.
    pub region: String,
    /// WHEN the change happened, in epoch milliseconds — the instant the write recorded,
    /// not the instant the publication was made.
    pub at: i64,
    /// The `detail-type` this change is published under: which kind of change it is.
    pub detail_type: &'static str,
    /// The rendered `detail` of the event.
    pub detail: ::serde_json::Value,
}

impl PublishedEvent {
    /// Publish this event onto the account's default event bus in the repository's
    /// Region.
    ///
    /// Call it with NO store guard held. Nothing about the answer changes what the caller
    /// of the operation is told, for the reason the module docs give.
    pub async fn publish(self, callback: &::smithy_server_runtime::EmbeddedEmulator) {
        let client = ::service_common::eventbridge_client::eventbridge_client(callback, &self.region);
        let entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
            .event_bus_name(DEFAULT_EVENT_BUS)
            .source(EVENT_SOURCE)
            .detail_type(self.detail_type)
            // The event's `time` is WHEN the change happened, not when it was published:
            // `PutEventsRequestEntry$Time` is "the time stamp of the event, per RFC3339.
            // If no time stamp is provided, the time stamp of the PutEvents call is
            // used."
            .time(::aws_sdk_eventbridge::primitives::DateTime::from_millis(
                self.at,
            ))
            .resources(&self.repository_arn)
            .detail(self.detail.to_string())
            .build();
        match client.put_events().entries(entry).send().await {
            Ok(answer) if answer.failed_entry_count() > 0 => {
                ::tracing::warn!(
                    repository = %self.repository_arn,
                    detail_type = %self.detail_type,
                    "a CodeCommit event was not accepted by the default event bus"
                );
            }
            Ok(_) => {}
            Err(error) => {
                ::tracing::warn!(
                    repository = %self.repository_arn,
                    detail_type = %self.detail_type,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "a CodeCommit event could not be published"
                );
            }
        }
    }
}

/// Publish `events` in the order they were rendered.
///
/// The one place a handler hands over what its write recorded, so no caller of it orders
/// the publication of two changes of one call differently.
pub async fn publish_all(
    events: Vec<PublishedEvent>,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
) {
    for event in events {
        event.publish(callback).await;
    }
}

/// How many publications run at once, over the whole store.
///
/// A write that changes something starts one publication, so a caller driving writes
/// drives tasks: this permit is what keeps the work in flight bounded however many writes
/// arrive, exactly as the rest of this crate bounds what one request can grow. The task
/// waits for a permit rather than dropping the event, because a change this service
/// accepted is a change a consumer is owed.
const CONCURRENT_PUBLICATIONS: usize = 16;

/// The permits those publications take, one per publication in flight.
static PUBLICATIONS: ::tokio::sync::Semaphore =
    ::tokio::sync::Semaphore::const_new(CONCURRENT_PUBLICATIONS);

/// The same publication, on a task of its OWN, for a handler that still holds this
/// service's store guard.
///
/// EventBridge delivery is not part of the answer the caller of the write is given — the
/// event reaches a rule's targets after the call that caused it, which is what a consumer
/// polling for it observes — so the publication is handed to its own task rather than
/// awaited under the guard: a guard held across an await would make a handler's future
/// non-`Send`, and the write's answer waits for nothing that is not part of it. Nothing
/// to publish starts no task, and a task that starts takes one of
/// [`CONCURRENT_PUBLICATIONS`] permits before it publishes.
pub fn publish_all_detached(
    events: Vec<PublishedEvent>,
    callback: &::smithy_server_runtime::EmbeddedEmulator,
) {
    if events.is_empty() {
        return;
    }
    let callback = callback.clone();
    ::tokio::spawn(async move {
        // One permit per publication in flight; a closed semaphore is not something this
        // process creates, so a permit that cannot be taken leaves the events untouched
        // rather than published twice.
        let Ok(_permit) = PUBLICATIONS.acquire().await else {
            return;
        };
        publish_all(events, &callback).await;
    });
}
