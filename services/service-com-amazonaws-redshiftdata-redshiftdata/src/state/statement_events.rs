// SPDX-License-Identifier: CC0-1.0

//! The EventBridge event a statement publishes when its submission asked to be
//! told how it ended.
//!
//! # The rule this module implements
//!
//! A submit may ask for an event instead of being polled for: `WithEvent` is "a
//! value that indicates whether to send an event to the Amazon EventBridge event
//! bus after the SQL statement runs"
//! (`smithy_query(query="doc",shape="ExecuteStatementInput",member="WithEvent")`).
//! So a submission that asked for one has the status change it makes when it stops
//! published onto the account's default event bus ([`DEFAULT_EVENT_BUS`]) under the
//! `source` this service owns ([`EVENT_SOURCE`]) and the `detail-type` of a
//! statement's status change ([`DETAIL_TYPE`]), naming the warehouse the statement
//! ran on in `resources` and carrying the statement's own identity and outcome in
//! `detail`. A rule on that bus whose `EventPattern` matches then delivers it to
//! the rule's targets, so a client driven by the event observes the statement
//! without ever calling this service again.
//!
//! Accepting the member and publishing nothing is the one thing that must not
//! happen: it leaves such a client waiting for ever on an event that was asked for
//! and never sent. A submission that asked for NOTHING publishes nothing, so the
//! member is read off the stored record and nowhere else
//! ([`crate::state::Statement::with_event`]).
//!
//! The envelope and the `detail` member names are not in any Smithy model —
//! EventBridge events never are — so they are stated once here, and every value in
//! them is read off the statement's own record.
//!
//! # How a status change becomes an event
//!
//! Nothing here decides WHEN a statement stops: its own execution does, and the
//! two places that record a terminal status queue the event for it
//! ([`crate::state::ComAmazonawsRedshiftdataRedshiftdataState::complete`] for the
//! outcome an execution reported, and that store's `cancel` for a statement the
//! caller stopped), so one status change is queued exactly once and the queue is
//! bounded ([`MAX_PENDING_EVENTS`], oldest dropped first).
//!
//! A queued event is published with NO store lock held ([`publish_pending`]):
//! EventBridge is reached the way every emulated service reaches another, through
//! the framework's in-process callback, so this crate names no other service's
//! crate. Publication never changes the statement's own record: whatever the bus
//! answers, the record stands and the operations that report the statement answer
//! the same.
//!
//! A status change stays OWED until the bus TAKES it. A publication the bus did
//! not accept — a request that failed, or an entry it refused or said nothing
//! about — puts its status changes back at the head of the queue
//! ([`publish_pending`]), so the next drain publishes them: the queue is what
//! records that the announcement a submission asked for has not been made yet.
//! Dropping them there instead would make a submission that asked for an event one
//! that never gets it and never can, which is the one outcome this module exists
//! to prevent, and it would be invisible beside a submission that asked for
//! nothing. A drain ABANDONED mid-request gives its whole batch back
//! ([`TakenEvents`]), so an announcement the bus may already have taken can be
//! offered a second time: AT LEAST once, deliberately, because a client waiting on
//! an event it asked for is stopped by a lost announcement and not by a repeated
//! one.
//!
//! WHICH call publishes: the task that runs the statement, as soon as it has
//! recorded the outcome, so the event arrives whether or not anybody polls; and
//! the cancel that stopped a running statement, for the same reason.
//!
//! A BATCH is one submission holding one `Id` — the `Id` its caller was answered
//! with and correlates by — so its status change is announced once under that
//! `Id`, rather than once per statement of it.

use std::time::{SystemTime, UNIX_EPOCH};

use ::smithy_server_runtime::EmbeddedEmulator;

use super::Statement;

/// The `source` of every event this service publishes.
///
/// EventBridge names the publishing service in an event's `source` as
/// `aws.<service>`, and this service's own name on the wire is `redshift-data`
/// (`smithy_query(query="service_info")`: its `sdkId` and `endpointPrefix`).
pub const EVENT_SOURCE: &str = "aws.redshift-data";

/// The `detail-type` a statement's status change is published under — the value a
/// rule's `EventPattern` matches on.
pub const DETAIL_TYPE: &str = "Redshift Data Statement Status Change";

/// The event bus this service publishes onto.
///
/// The events an AWS service generates go to the account's default event bus,
/// which is the bus a rule carrying no `EventBusName` is created on.
pub const DEFAULT_EVENT_BUS: &str = "default";

/// How many statement events the queue holds at once.
///
/// This module's OWN ceiling, not an AWS quota: the queue lives in memory and
/// every status change of every statement that asked for an event appends to it,
/// so without a bound a caller submitting without end could grow it without end.
/// The oldest event is dropped first, so the status changes the newest statements
/// just made are the ones that survive. It is far above what a realistic burst
/// produces, because a submission contributes one event and this service bounds
/// how many submissions are in flight at once
/// ([`crate::state::MAX_ACTIVE_STATEMENTS`]).
pub const MAX_PENDING_EVENTS: usize = 2_000;

/// How many entries one `PutEvents` request carries.
///
/// The bound the service this publishes to declares: its `PutEventsRequestEntryList`
/// is `@length(min: 1, max: 10)`, so a drain holding more events than that is
/// published in several requests rather than one the service would refuse.
const MAX_PUT_EVENTS_ENTRIES: usize = 10;

/// How many events ONE drain publishes.
///
/// The queue itself is bounded ([`MAX_PENDING_EVENTS`]), but one call draining all
/// of it would fan out to a rule's targets that many times: this caps one drain,
/// and [`publish_pending`] repeats a drain until the queue is empty, so nothing
/// waits for a later call.
const MAX_PUBLISHED_PER_DRAIN: usize = 50;

/// How many drains ONE call makes.
///
/// A call publishes at most a FULL QUEUE's worth and then returns, so the work one
/// call does is bounded even while other statements are still stopping and
/// appending: what arrives after it started is left to the next drain, which every
/// terminal transition makes. Without this the call-level work would be unbounded
/// however tightly one drain is capped.
const MAX_DRAINS_PER_CALL: usize = MAX_PENDING_EVENTS.div_ceil(MAX_PUBLISHED_PER_DRAIN);

/// The IAM principal an event names as the identity that submitted the statement.
///
/// This emulator publishes exactly one long-term credential, which speaks for the
/// account's single IAM user — the identity STS reports as the caller and IAM holds
/// among the account's users — so that user's ARN is who a submitted statement
/// belongs to. Stated through the one definition of it
/// ([`service_common::caller_principal`]) rather than built here, so the principal
/// an event names is the principal every other service reports for the same call.
#[must_use]
pub fn submitting_principal() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// One statement event waiting to be published: WHICH statement, WHAT it ended as,
/// WHEN that happened, and the Region and warehouse it happened on.
///
/// Self-contained on purpose — everything the event carries is taken when the
/// status change is queued, so a drain does not depend on the statement still
/// being retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementEvent {
    /// The `Id` the submit answered with, which is what a client correlates the
    /// event with the statement it submitted by.
    pub statement_id: String,
    /// The `StatementName` the submission carried, when it carried one.
    pub statement_name: Option<String>,
    /// The status the statement stopped at.
    pub state: String,
    /// Why it stopped, for a statement that stopped WITHOUT finishing: the
    /// backend's own reason, exactly as the record reports it.
    pub error: Option<String>,
    /// The IAM principal that submitted the statement ([`submitting_principal`]).
    pub principal: String,
    /// The ARN of the warehouse the statement ran on, when this service could
    /// name it — the envelope's `resources` entry.
    pub resource_arn: Option<String>,
    /// WHEN the statement stopped, which is the instant the event reports.
    pub at: SystemTime,
    /// The Region the statement was submitted in: an event is published in the
    /// Region the work happened in.
    pub region: String,
}

impl StatementEvent {
    /// The event reporting the status change `statement` has just made, taken off
    /// the record itself so the event and the statement cannot disagree.
    pub(super) fn of(statement: &Statement) -> Self {
        Self {
            statement_id: statement.id.clone(),
            statement_name: statement.statement_name.clone(),
            state: statement.status.as_str().to_string(),
            error: statement.error(),
            principal: statement.principal.clone(),
            resource_arn: statement.resource_arn.clone(),
            at: statement.completed_at,
            region: statement.region.clone(),
        }
    }

    /// The event's `detail`: which statement this is about, what the submission
    /// called it, what it ended as, who submitted it — and, for a statement that
    /// stopped without finishing, the reason the record carries.
    ///
    /// A member the record cannot ground is LEFT OUT rather than written as null:
    /// a statement submitted without a name has no name, and one that finished has
    /// no error.
    fn detail(&self) -> ::serde_json::Value {
        let mut detail = ::serde_json::Map::new();
        detail.insert(
            "statementId".to_string(),
            ::serde_json::Value::String(self.statement_id.clone()),
        );
        if let Some(name) = &self.statement_name {
            detail.insert(
                "statementName".to_string(),
                ::serde_json::Value::String(name.clone()),
            );
        }
        detail.insert(
            "state".to_string(),
            ::serde_json::Value::String(self.state.clone()),
        );
        detail.insert(
            "principal".to_string(),
            ::serde_json::Value::String(self.principal.clone()),
        );
        if let Some(error) = &self.error {
            detail.insert(
                "errorMessage".to_string(),
                ::serde_json::Value::String(error.clone()),
            );
        }
        ::serde_json::Value::Object(detail)
    }
}

/// `at` as the epoch milliseconds the event's `time` carries.
fn epoch_millis(at: SystemTime) -> i64 {
    i64::try_from(
        at.duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or_default()
}

/// Publish every statement event this store has queued onto the account's default
/// EventBridge bus, and leave queued exactly what the bus did not take.
///
/// Called with NO store lock held: each drain takes its events out in one critical
/// section, so nothing is published twice, and the in-process `PutEvents` runs
/// outside it. The queue is drained until it is EMPTY — one drain is capped
/// ([`MAX_PUBLISHED_PER_DRAIN`]) and the number of drains is capped
/// ([`MAX_DRAINS_PER_CALL`]), so no single call is unbounded, and repeating the
/// drain is what guarantees the tail of a burst is published by the call that
/// produced it rather than waiting for a later one.
///
/// What the bus did not take of a drain is OWED again: it goes back at the head of
/// the queue, so the next drain — which every terminal transition makes — offers it
/// once more, and this call makes no further drain, because a bus that is not
/// taking announcements at this instant would do no better with the rest of the
/// queue. Nothing about the answer an operation gives changes either way.
///
/// The batch a drain has taken is held by a guard that gives it back when this
/// future ends however it ends ([`TakenEvents`]), so a drain abandoned at its
/// `await` — a timeout, a lost `select!` branch, a task aborted at shutdown —
/// leaves the status change owed rather than consumed.
pub async fn publish_pending<Eng: Send + Sync + 'static>(
    callback: &EmbeddedEmulator,
    engine: &Eng,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    for _drain in 0..MAX_DRAINS_PER_CALL {
        let taken = state
            .lock()
            .take_statement_events(MAX_PUBLISHED_PER_DRAIN);
        if taken.is_empty() {
            return;
        }
        let mut held = TakenEvents {
            state: ::engine::ServiceState::clone(&state),
            events: taken,
        };
        let unpublished = publish_batch(callback, &held.events).await;
        // What the bus took is no longer owed; what it did not is what the guard
        // gives back when this scope ends.
        held.events = unpublished;
        if !held.events.is_empty() {
            return;
        }
    }
}

/// The batch ONE drain took out of the store, held for as long as that drain is
/// being made and given back when this value is dropped.
///
/// A drain takes its events out before it offers them to the bus, so between those
/// two instants the store does not hold them: this is what makes that window safe
/// to abandon. Whatever is left in `events` when the drain ends is still owed, on
/// the cancellation path as much as on the ordinary one, which is what keeps a
/// status change a submission asked for from being consumed by an attempt that
/// never completed.
///
/// A drain abandoned mid-request gives the WHOLE batch back, including an entry
/// the request it travelled in may already have delivered, so such an
/// announcement can be published twice. That is the trade this guard makes on
/// purpose: a client driven by an event it asked for is stopped by an
/// announcement that never comes and not by one that comes twice.
struct TakenEvents {
    state: ::engine::ServiceState<super::ComAmazonawsRedshiftdataRedshiftdataState>,
    events: Vec<StatementEvent>,
}

impl Drop for TakenEvents {
    fn drop(&mut self) {
        let owed = ::std::mem::take(&mut self.events);
        if owed.is_empty() {
            return;
        }
        self.state.lock().requeue_statement_events(owed);
    }
}

// By hand, because what is diagnosable about a taken batch is which statements'
// status changes it holds rather than the store handle it gives them back through.
impl ::std::fmt::Debug for TakenEvents {
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("TakenEvents")
            .field("events", &self.events)
            .finish()
    }
}

/// Publish one drain, in `PutEvents` requests of at most [`MAX_PUT_EVENTS_ENTRIES`]
/// entries, one request per Region the drained events belong to, and answer the
/// status changes the bus did NOT take — in the order they were queued in.
///
/// Every Region's request is made: an event is published in the Region its
/// statement ran in, so a bus that did not take one Region's request says nothing
/// about another's. What is answered is addressed BY POSITION in `events`, so the
/// answer is in queue order whichever request each event travelled in.
async fn publish_batch(
    callback: &EmbeddedEmulator,
    events: &[StatementEvent],
) -> Vec<StatementEvent> {
    let mut unpublished: Vec<usize> = Vec::new();
    for (chunk, window) in events.chunks(MAX_PUT_EVENTS_ENTRIES).enumerate() {
        let first = chunk * MAX_PUT_EVENTS_ENTRIES;
        for region in regions_of(window) {
            let client = ::service_common::eventbridge_client::eventbridge_client(callback, &region);
            let mut request = client.put_events();
            // The entries of this request, in the order they are added to it,
            // each beside its position in `events`: `PutEvents` answers one
            // outcome PER ENTRY in that same order, so this is what an outcome
            // is read back against.
            let offered: Vec<(usize, &StatementEvent)> = window
                .iter()
                .enumerate()
                .filter(|(_, event)| event.region == region)
                .map(|(position, event)| (first + position, event))
                .collect();
            for (_, event) in &offered {
                let mut entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
                    .event_bus_name(DEFAULT_EVENT_BUS)
                    .source(EVENT_SOURCE)
                    .detail_type(DETAIL_TYPE)
                    // The event's `time` is WHEN the statement stopped, not when
                    // the queue was drained: `PutEventsRequestEntry$Time` is "the
                    // time stamp of the event, per RFC3339. If no time stamp is
                    // provided, the time stamp of the PutEvents call is used."
                    .time(::aws_sdk_eventbridge::primitives::DateTime::from_millis(
                        epoch_millis(event.at),
                    ))
                    .detail(event.detail().to_string());
                if let Some(resource_arn) = &event.resource_arn {
                    entry = entry.resources(resource_arn);
                }
                request = request.entries(entry.build());
            }
            match request.send().await {
                // Nothing in this request reached the bus, so every status
                // change it carried is still owed.
                Err(error) => {
                    ::tracing::warn!(
                        region = %region,
                        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                        statements = ?statement_ids(&offered),
                        "a statement status change could not be published to EventBridge, so it \
                         stays queued"
                    );
                    unpublished.extend(offered.iter().map(|(at, _)| *at));
                }
                // The bus answers one outcome PER ENTRY, so a request it
                // accepted can still have refused some of the events in it. An
                // event nobody will ever receive is exactly what this module
                // exists to prevent, so a refused entry is said out loud —
                // naming the statements whose status change did not land — and
                // that status change stays queued rather than passing as a
                // published one.
                //
                // An entry the answer says NOTHING about — an answer carrying
                // fewer outcomes than the request carried entries — is not one
                // the bus said it took either, so it stays queued as well: the
                // only entries treated as published are those the bus answered
                // for, without an error code.
                Ok(answered) => {
                    let results = answered.entries();
                    let refused: Vec<(usize, &StatementEvent)> = offered
                        .iter()
                        .enumerate()
                        .filter(|(slot, _)| {
                            results
                                .get(*slot)
                                .is_none_or(|result| result.error_code().is_some())
                        })
                        .map(|(_, offer)| *offer)
                        .collect();
                    if refused.is_empty() {
                        continue;
                    }
                    ::tracing::warn!(
                        region = %region,
                        refused = refused.len(),
                        answered_for = results.len(),
                        offered = offered.len(),
                        reasons = ?results
                            .iter()
                            .filter_map(|result| {
                                result.error_code().map(|code| {
                                    (code.to_string(), result.error_message().map(str::to_string))
                                })
                            })
                            .collect::<Vec<_>>(),
                        statements = ?statement_ids(&refused),
                        "EventBridge did not take a statement status change, so nothing was \
                         published for it and it stays queued"
                    );
                    unpublished.extend(refused.iter().map(|(at, _)| *at));
                }
            }
        }
    }
    // In the order the status changes were queued in, whichever request each of
    // them travelled in, and each of them once.
    unpublished.sort_unstable();
    unpublished.dedup();
    unpublished
        .into_iter()
        .filter_map(|at| events.get(at).cloned())
        .collect()
}

/// The statements one group of offered events is about, for the log.
fn statement_ids(offered: &[(usize, &StatementEvent)]) -> Vec<String> {
    offered
        .iter()
        .map(|(_, event)| event.statement_id.clone())
        .collect()
}

/// The Regions the events of one window belong to, each once and in the order they
/// first appear: an event is published in the Region the statement ran in, so a
/// window holding two Regions' statements is published to both buses.
fn regions_of(window: &[StatementEvent]) -> Vec<String> {
    let mut regions: Vec<String> = Vec::new();
    for event in window {
        if !regions.iter().any(|region| region == &event.region) {
            regions.push(event.region.clone());
        }
    }
    regions
}
