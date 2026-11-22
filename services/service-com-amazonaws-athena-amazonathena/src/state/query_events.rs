// SPDX-License-Identifier: CC0-1.0

//! The EventBridge events this service publishes as a QUERY EXECUTION moves through its
//! states.
//!
//! # The rule this module implements
//!
//! Athena is an event SOURCE, not only a control plane a client polls: every state a query
//! execution MOVES INTO is published onto the account's default event bus
//! ([`DEFAULT_EVENT_BUS`]) under the `source` this service owns ([`EVENT_SOURCE`]) and the
//! `detail-type` of a query's state change ([`QUERY_STATE_CHANGE_DETAIL_TYPE`]), carrying in
//! the event's `detail` the transition itself — the state the query reached, the state it
//! came from, the execution it is about, the workgroup it ran in, the kind of statement it
//! carries and which event of that query's sequence this is — and, for a query whose current
//! state is [`QUERY_STATE_FAILED`] and for no other state, the error that query reports.
//! A rule on that bus whose `EventPattern` matches then delivers it to that rule's targets,
//! which is how a client reacts to a query settling — routing the failed ones to a queue,
//! alarming on them, reading the failure off the envelope — without calling Athena again,
//! and a rule patterned on another state receives nothing.
//!
//! # How a transition becomes an event
//!
//! Nothing here decides WHEN a query moves: the instants its own record holds do, and the
//! state they put it in at a given instant is [`QueryExecutionRecord::state_at`]. This
//! module only COMPARES the states a query has passed through ([`state_timeline`]) against
//! the one last published for it, so every state it moved into is published exactly once —
//! whichever call or wake-up observed it — and no writer has to remember to announce
//! itself. A state nothing moved publishes nothing, so two calls at the same instant publish
//! the same single event.
//!
//! WHO publishes: the call that reports a query's state, once it has released this service's
//! store ([`announce_query_state_changes`]), and the in-process CLOCK PUMP that waits for the
//! instants a query's own record holds ([`drive_query_clock`]) — because a client that keys
//! off these events makes no further Athena call, so a transition published only by the next
//! reader would never be published at all. The pump does not change when anything happens:
//! it materialises nothing a stored instant has not already made due, and a caller reading
//! before that instant still sees the transient state.
//!
//! The event's `time` is the instant the query reached that state, not the instant the
//! publication happened to be made, so an event and a read of the same execution report one
//! timeline.
//!
//! Publication is made with NO store lock held: EventBridge is reached the way every
//! emulated service reaches another, through the framework's in-process callback
//! (`service_common::eventbridge_client`), so this crate names no other service's crate. It
//! is best effort — an emulator with no EventBridge mounted has nowhere to publish — so a
//! failure is logged and the operation's own answer stands; the transitions of a publication
//! that failed are given back ([`ComAmazonawsAthenaAmazonathenaState::forget_published_query_states`]),
//! so the next call or wake-up publishes them rather than losing them.

use ::std::time::UNIX_EPOCH;

use super::*;

/// The `source` of every event Athena publishes.
///
/// EventBridge names the publishing service in an event's `source` as `aws.<service>`, and
/// this service's own namespace is the one its ARNs and its IAM actions carry
/// ([`authorization::ACTION_PREFIX`]).
pub const EVENT_SOURCE: &str = "aws.athena";

/// The `detail-type` of the event published when a QUERY EXECUTION changes state.
pub const QUERY_STATE_CHANGE_DETAIL_TYPE: &str = "Athena Query State Change";

/// The event bus this service publishes onto.
///
/// The events an AWS service generates go to the account's default event bus, which is the
/// bus a rule carrying no `EventBusName` is created on.
pub const DEFAULT_EVENT_BUS: &str = "default";

/// How many entries one `PutEvents` request carries.
///
/// The bound the service this publishes to declares: its `PutEventsRequestEntryList` is
/// `@length(min: 1, max: 10)`, so a call that observed more transitions than that publishes
/// them in several requests rather than one the service would refuse.
const MAX_PUT_EVENTS_ENTRIES: usize = 10;

/// How many transitions ONE call records and publishes.
///
/// THIS CEILING IS THE EMULATOR'S, NOT AWS'S: each published event fans out to the targets of
/// every rule that matches it, so the work one request can set off is bounded here. A
/// transition past it stays unpublished and is published by the next call or wake-up, so
/// nothing is lost. A query owes at most three transitions and the execution history is
/// itself bounded ([`MAX_QUERY_EXECUTIONS`]), so no realistic call reaches it.
const MAX_RECORDED_CHANGES: usize = 64;

/// The shortest one wake-up of the clock pump waits, so an instant already past cannot cost
/// a CPU-bound spin.
const MIN_CLOCK_WAIT: Duration = Duration::from_millis(5);

/// The longest one wake-up waits before it looks at the store again: a backstop, so a due
/// instant far in the future cannot park the pump indefinitely.
const MAX_CLOCK_WAIT: Duration = Duration::from_secs(5);

/// How many wake-ups one pump makes before it stands down. A backstop, not a budget: a query
/// owes three transitions and the history is bounded ([`MAX_QUERY_EXECUTIONS`]), so a pump
/// normally stands down for want of work.
const MAX_CLOCK_WAKE_UPS: usize = 10_000;

/// One query-execution transition this service observed, waiting to be published.
///
/// Every field is read off the execution record this service's own reads are projected from,
/// so the event reports the query a `GetQueryExecution` of the same instant reports and never
/// a second, invented copy of it.
#[derive(Debug, Clone)]
pub struct QueryStateChange {
    /// The execution that moved.
    pub query_execution_id: String,
    /// The `QueryExecutionState` it moved INTO.
    pub current_state: String,
    /// The `QueryExecutionState` it moved OUT OF — the one last published for it, and for
    /// the first event of a query the state its submission created it in
    /// ([`QUERY_STATE_QUEUED`]).
    pub previous_state: String,
    /// The workgroup the query runs in.
    pub work_group: String,
    /// The `StatementType` the submitted SQL parses as, where it parses as one at all.
    pub statement_type: Option<String>,
    /// Which event of this query's own sequence this is, counted from one: a client that
    /// receives two events for one query reads the order they were published in off it.
    pub sequence_number: u32,
    /// The error the query reports, present only where [`Self::current_state`] is
    /// [`QUERY_STATE_FAILED`] — the same `AthenaError` a read of the execution reports.
    pub athena_error: Option<crate::ops::types::AthenaError>,
    /// WHEN the query reached that state — the instant its own record puts the transition
    /// at, not the instant this event happened to be published.
    pub at: SystemTime,
}

impl QueryStateChange {
    /// The event `detail` this transition is reported in.
    ///
    /// Public because it IS the contract: the members a rule's `EventPattern` filters on and
    /// a target reads, which is what a suite of this crate holds the published event to.
    #[must_use]
    pub fn detail(&self) -> ::serde_json::Value {
        let mut detail = ::serde_json::Map::new();
        detail.insert(
            "currentState".to_string(),
            ::serde_json::Value::String(self.current_state.clone()),
        );
        detail.insert(
            "previousState".to_string(),
            ::serde_json::Value::String(self.previous_state.clone()),
        );
        detail.insert(
            "queryExecutionId".to_string(),
            ::serde_json::Value::String(self.query_execution_id.clone()),
        );
        detail.insert(
            "workgroupName".to_string(),
            ::serde_json::Value::String(self.work_group.clone()),
        );
        // A submission that parses as no statement at all has no kind to name, so the member
        // is absent for it rather than carrying a guess.
        if let Some(statement_type) = &self.statement_type {
            detail.insert(
                "statementType".to_string(),
                ::serde_json::Value::String(statement_type.clone()),
            );
        }
        // The count is reported as the DIGITS of the number, which is the form this member
        // takes on the wire.
        detail.insert(
            "sequenceNumber".to_string(),
            ::serde_json::Value::String(self.sequence_number.to_string()),
        );
        // Only a query whose current state is FAILED reports an error, so every other
        // transition carries no such member at all rather than an empty one.
        if let Some(error) = &self.athena_error {
            let mut reported = ::serde_json::Map::new();
            if let Some(category) = error.error_category {
                reported.insert(
                    "errorCategory".to_string(),
                    ::serde_json::Value::from(category),
                );
            }
            if let Some(error_type) = error.error_type {
                reported.insert("errorType".to_string(), ::serde_json::Value::from(error_type));
            }
            reported.insert(
                "retryable".to_string(),
                ::serde_json::Value::Bool(error.retryable),
            );
            if let Some(message) = &error.error_message {
                reported.insert(
                    "errorMessage".to_string(),
                    ::serde_json::Value::String(message.clone()),
                );
            }
            detail.insert(
                "athenaError".to_string(),
                ::serde_json::Value::Object(reported),
            );
        }
        ::serde_json::Value::Object(detail)
    }
}

impl QueryExecutionRecord {
    /// The states this query has passed through at `now`, in the order it reached them, each
    /// with the instant it reached it.
    ///
    /// Derived from the same stored instants [`QueryExecutionRecord::state_at`] derives the
    /// current state from, so the last state here is always the state a read of this
    /// execution reports: a query starts QUEUED when it was submitted, is RUNNING from the
    /// instant its queue span passed, and is terminal from the instant its completion fell
    /// due — or CANCELLED from the instant a stop interrupted it, having passed only the
    /// states it had reached by then.
    fn state_timeline(&self, now: SystemTime) -> Vec<(&'static str, SystemTime)> {
        let mut timeline = vec![(QUERY_STATE_QUEUED, self.submitted_at)];
        let running_due_at = self.running_due_at();
        if let Some(cancelled_at) = self.cancelled_at {
            if cancelled_at >= running_due_at {
                timeline.push((QUERY_STATE_RUNNING, running_due_at));
            }
            timeline.push((QUERY_STATE_CANCELLED, cancelled_at));
            return timeline;
        }
        if now < running_due_at {
            return timeline;
        }
        timeline.push((QUERY_STATE_RUNNING, running_due_at));
        // A query whose run has reported nothing yet has not reached a terminal state
        // however long ago it was submitted, which is the rule `state_at` reads too.
        let Some(outcome) = &self.outcome else {
            return timeline;
        };
        if now < self.completion_due_at {
            return timeline;
        }
        timeline.push((
            match outcome {
                query_engine::Outcome::Succeeded { .. } => QUERY_STATE_SUCCEEDED,
                query_engine::Outcome::Failed { .. } => QUERY_STATE_FAILED,
                // A run a LIMIT in force stopped was interrupted, which is the state
                // `state_at` derives for it too.
                query_engine::Outcome::Aborted { .. } => QUERY_STATE_CANCELLED,
            },
            self.completion_due_at,
        ));
        timeline
    }

    /// The transitions of this query that have not been published yet, recorded as
    /// published, at most `limit` of them.
    ///
    /// The comparison is against what was PUBLISHED rather than against who wrote the state,
    /// so a transition the clock materialised on a read is published exactly like one a call
    /// wrote, and publishing one twice is impossible. The states a query passed through
    /// without anybody looking are published too, in the order it reached them, so a client
    /// that reads the queue once still receives one envelope per transition.
    fn record_state_changes(&mut self, now: SystemTime, limit: usize) -> Vec<QueryStateChange> {
        let timeline = self.state_timeline(now);
        // Everything AFTER the state last published; a published state the timeline no
        // longer holds (none does — a query's states are reached once) would publish the
        // whole timeline again rather than skipping it.
        let published = self.published_state.clone();
        let taken = published
            .as_deref()
            .and_then(|published| {
                timeline
                    .iter()
                    .position(|(state, _)| *state == published)
                    .map(|position| position + 1)
            })
            .unwrap_or_default();
        let mut previous = published.unwrap_or_else(|| QUERY_STATE_QUEUED.to_string());
        let mut changes = Vec::new();
        for (state, at) in timeline.into_iter().skip(taken).take(limit) {
            self.published_events = self.published_events.saturating_add(1);
            changes.push(QueryStateChange {
                query_execution_id: self.id.clone(),
                current_state: state.to_string(),
                previous_state: previous,
                work_group: self.work_group.clone(),
                statement_type: self.statement_type.clone(),
                sequence_number: self.published_events,
                athena_error: match state {
                    QUERY_STATE_FAILED => self.athena_error_at(now),
                    _ => None,
                },
                at,
            });
            previous = state.to_string();
            self.published_state = Some(state.to_string());
        }
        changes
    }

    /// The next instant this query owes a state-change event at, or `None` when it owes
    /// none — what the clock pump waits for.
    ///
    /// A transition already observable is owed NOW; otherwise it is the instant the query
    /// leaves the state it is in, which is the next stored instant its own record holds. A
    /// query that has settled and been published owes nothing ever again.
    fn next_event_due_at(&self, now: SystemTime) -> Option<SystemTime> {
        if self.published_state.as_deref() != Some(self.state_at(now)) {
            return Some(now);
        }
        if self.cancelled_at.is_some() {
            return None;
        }
        if now < self.running_due_at() {
            return Some(self.running_due_at());
        }
        // A run that has not reported yet moves when it reports, which is the call that ran
        // it rather than the clock, so nothing is owed at an instant until then.
        (self.outcome.is_some() && now < self.completion_due_at).then_some(self.completion_due_at)
    }
}

impl ComAmazonawsAthenaAmazonathenaState {
    /// Every query-execution transition this store has not published yet, recorded as
    /// published, oldest transition first.
    ///
    /// Called with the store held, and answers the changes to publish once it is released. At
    /// most [`MAX_RECORDED_CHANGES`] are taken; the rest keep the state they were last
    /// published in, so the next call or wake-up records them.
    pub fn record_query_state_changes(&mut self, now: SystemTime) -> Vec<QueryStateChange> {
        let mut changes: Vec<QueryStateChange> = Vec::new();
        for record in self.query_executions.values_mut() {
            let room = MAX_RECORDED_CHANGES.saturating_sub(changes.len());
            if room == 0 {
                break;
            }
            changes.extend(record.record_state_changes(now, room));
        }
        // The execution map walks in no particular order, so the changes are ordered by WHEN
        // the transitions they report happened: a target receives the states of a query, and
        // of two queries, in the order they were reached.
        changes.sort_by_key(|change| change.at);
        changes
    }

    /// Give back the record of publication for `changes`: they were recorded as published
    /// and the publication did not go through, so each query is left in the state it was
    /// last published in and the next call or wake-up publishes those transitions again.
    ///
    /// The changes of one query arrive in the order they were recorded, so rolling back to
    /// the EARLIEST of them is what the guard below keeps: a later one cannot undo it.
    pub fn forget_published_query_states(&mut self, changes: &[QueryStateChange]) {
        for change in changes {
            let Some(record) = self.query_executions.get_mut(&change.query_execution_id) else {
                continue;
            };
            if record.published_events < change.sequence_number {
                continue;
            }
            record.published_state = match change.sequence_number {
                // The first event of a query was never published, so nothing has been.
                1 => None,
                _ => Some(change.previous_state.clone()),
            };
            record.published_events = change.sequence_number.saturating_sub(1);
        }
    }

    /// The earliest instant at which this store owes a query-execution event, if any — the
    /// instant the clock pump waits for.
    fn next_query_event_due_at(&self, now: SystemTime) -> Option<SystemTime> {
        self.query_executions
            .values()
            .filter_map(|record| record.next_event_due_at(now))
            .min()
    }

    /// What the clock pump waits for next, or `None` — having RELEASED its claim — when this
    /// store owes nothing more.
    ///
    /// Two things are owed at an instant: the next transition to publish, and the next
    /// query's own metrics ([`query_metrics`]) where there is a store to publish them into —
    /// `metering` says whether there is. The pump waits for the earlier of them, so a query
    /// whose transitions are all published is still kept up with until its metrics are.
    ///
    /// Observing "nothing is owed" and standing down happen in ONE critical section, which is
    /// what makes the hand-over race-free: the call that starts a query stores it (and so its
    /// due instants) under this same lock BEFORE it tries to claim the pump, so either this
    /// call sees that query and the running pump keeps it, or the starter's own claim finds
    /// the flag already cleared and starts a pump of its own.
    fn next_query_event_due_or_release_pump(
        &mut self,
        now: SystemTime,
        generation: u64,
        metering: bool,
    ) -> Option<SystemTime> {
        let metrics_due = metering.then(|| self.next_query_metric_due_at()).flatten();
        match self
            .next_query_event_due_at(now)
            .into_iter()
            .chain(metrics_due)
            .min()
        {
            Some(due) => Some(due),
            None => {
                self.release_query_clock_pump(generation);
                None
            }
        }
    }

    /// Claim the single clock pump for this store: the claim's GENERATION when the caller may
    /// start one, `None` when one is really running.
    ///
    /// The claim is TAKEN OVER from a pump whose task has already ended while still holding
    /// it — one wake-up's work panicked, or the task was cancelled. Without the take-over
    /// that claim would be held for as long as this store lives, and because the claim is
    /// what stops a second pump from starting, no later call could ever start one again.
    fn start_query_clock_pump(&mut self) -> Option<u64> {
        let ended = self
            .query_clock_pump
            .as_ref()
            .is_some_and(::tokio::task::JoinHandle::is_finished);
        if self.query_clock_pump_running && !ended {
            return None;
        }
        if ended && self.query_clock_pump_running {
            ::tracing::warn!(
                "the Athena query clock pump this store holds the claim for has ended without \
                 releasing it; the claim is taken over so the events it owes are still published"
            );
        }
        if let Some(previous) = self.query_clock_pump.take() {
            previous.abort();
        }
        self.query_clock_pump_running = true;
        self.query_clock_pump_generation = self.query_clock_pump_generation.wrapping_add(1);
        Some(self.query_clock_pump_generation)
    }

    /// Release the claim `generation` took — and ONLY that one, so a pump that stood down
    /// cannot clear a claim a pump started since is holding.
    fn release_query_clock_pump(&mut self, generation: u64) {
        if self.query_clock_pump_generation == generation {
            self.query_clock_pump_running = false;
            self.query_clock_pump = None;
        }
    }

    /// Keep the running pump's task handle, aborting the one it replaces, so a pump is
    /// cancellable rather than merely forgotten — and only while `generation` is still the
    /// CURRENT claim, so a starter whose own task has already stood down installs nothing but
    /// its own abort.
    fn set_query_clock_pump(&mut self, generation: u64, handle: ::tokio::task::JoinHandle<()>) {
        if self.query_clock_pump_generation != generation || !self.query_clock_pump_running {
            handle.abort();
            return;
        }
        if let Some(previous) = self.query_clock_pump.replace(handle) {
            previous.abort();
        }
    }
}

/// Publish every query-execution transition this call observed and the metrics every query it
/// observed COMPLETE owes, and keep this store's clock pump running for the transitions and
/// the metrics still to fall due.
///
/// The ONE call an operation makes: `engine` is the handler's own `self.engine`, and the
/// Region the publication is addressed to is the Region the call was signed for
/// ([`authorization::region_from_request`]), which is the Region the event belongs to.
///
/// Called by the operations that report a query's state, AFTER they have released this
/// service's store: a publication reaches EventBridge's own handler, which matches the event
/// against the account's rules and delivers it to their targets, so holding this store across
/// it would serialise every other Athena call behind that fan-out. The metrics go into
/// CloudWatch's own store beside it ([`query_metrics`]), which this service's store is not
/// held across either.
pub async fn announce_query_state_changes<Eng: Send + Sync + 'static>(
    engine: &::std::sync::Arc<Eng>,
    ctx: &smithy_server_runtime::OperationContext<'_>,
) {
    let Some(state) = service_state(&**engine) else {
        return;
    };
    let region = authorization::region_from_request(ctx.parts());
    // The store a query's own metrics are published into, taken once: `None` is an emulator
    // with no CloudWatch mounted, which has nowhere to publish them.
    let cloudwatch = query_metrics::cloudwatch_state(&**engine);
    // What the publications ANSWERED matters only to the pump, which paces itself by it.
    publish_due(&state, ctx.callback(), &region).await;
    query_metrics::publish_due_query_metrics(&state, cloudwatch.as_ref());
    drive_query_clock(&state, ctx.callback(), &region, cloudwatch);
}

/// Record and publish what this store owes at this instant, with no lock held across the
/// publication, and answer whether everything it recorded went through.
///
/// `false` says the publication FAILED — the transitions it recorded were given back and are
/// still owed — which is what tells the clock pump to slow down rather than to go on asking
/// at the rate a live transition is published at.
async fn publish_due(
    state: &::engine::ServiceState<ComAmazonawsAthenaAmazonathenaState>,
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) -> bool {
    // The guard is taken and dropped inside this statement.
    let changes = state.lock().record_query_state_changes(SystemTime::now());
    if changes.is_empty() {
        return true;
    }
    let unpublished = publish(callback, region, &changes).await;
    if unpublished.is_empty() {
        return true;
    }
    state.lock().forget_published_query_states(&unpublished);
    false
}

/// Publish `changes` onto the account's default EventBridge bus, and answer the transitions
/// that could NOT be published.
///
/// Publication is best effort — an emulator with no EventBridge mounted has nowhere to
/// publish — so a failure is logged and the operation's own answer stands; what it is not is
/// silent loss, which is why the changes of a request that failed are answered and their
/// record of publication given back.
async fn publish(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    changes: &[QueryStateChange],
) -> Vec<QueryStateChange> {
    let client = ::service_common::eventbridge_client::eventbridge_client(callback, region);
    let mut unpublished: Vec<QueryStateChange> = Vec::new();
    for window in changes.chunks(MAX_PUT_EVENTS_ENTRIES) {
        let mut request = client.put_events();
        for change in window {
            let entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
                .event_bus_name(DEFAULT_EVENT_BUS)
                .source(EVENT_SOURCE)
                .detail_type(QUERY_STATE_CHANGE_DETAIL_TYPE)
                // The event's `time` is WHEN the query reached the state:
                // `PutEventsRequestEntry$Time` is "the time stamp of the event, per RFC3339.
                // If no time stamp is provided, the time stamp of the PutEvents call is
                // used."
                .time(::aws_sdk_eventbridge::primitives::DateTime::from_millis(
                    epoch_millis(change.at),
                ))
                .detail(change.detail().to_string())
                .build();
            request = request.entries(entry);
        }
        if let Err(error) = request.send().await {
            ::tracing::warn!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "an Athena query state change could not be published to EventBridge"
            );
            unpublished.extend(window.iter().cloned());
        }
    }
    unpublished
}

/// Start the one in-process task that waits for the instants this store's queries owe their
/// next state and their own metrics at, and publishes what is then due — so no request ever
/// has to be the one that asks.
///
/// A client that keys off these events to react to a query completing makes no further
/// Athena call, and so does one that watches the workgroup's metrics, so what is published
/// only by the next reader would never be published at all. The pump publishes exactly what
/// [`announce_query_state_changes`] publishes and decides nothing about WHEN a query moves.
///
/// At most one pump runs per store (the store carries the claim), it is bounded by
/// [`MAX_CLOCK_WAKE_UPS`] wake-ups, and it stands down as soon as the store owes nothing.
/// With no async runtime to spawn onto there is no pump: the transitions are then published
/// by the next operation that reports a query, as they are anyway.
fn drive_query_clock(
    state: &::engine::ServiceState<ComAmazonawsAthenaAmazonathenaState>,
    callback: &smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    cloudwatch: Option<::engine::ServiceState<query_metrics::CloudWatchState>>,
) {
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(generation) = state.lock().start_query_clock_pump() else {
        return;
    };
    let pumped = ::engine::ServiceState::clone(state);
    let callback = callback.clone();
    let region = region.to_string();
    let handle = runtime.spawn(async move {
        // `true` only if the wake-up cap below is what ended the loop; a pump that ran out of
        // work has already released its claim inside the one critical section that observed
        // it (`next_query_event_due_or_release_pump`).
        let mut capped = true;
        // The shortest a wake-up waits. Widened to [`MAX_CLOCK_WAIT`] by a wake-up whose
        // publication FAILED: an emulator with no EventBridge mounted has nowhere to publish,
        // and such a pump is not to go on asking at the rate a live transition is published
        // at. The first wake-up that gets through brings it back.
        let mut floor = MIN_CLOCK_WAIT;
        for _ in 0..MAX_CLOCK_WAKE_UPS {
            let now = SystemTime::now();
            let due = pumped.lock().next_query_event_due_or_release_pump(
                now,
                generation,
                cloudwatch.is_some(),
            );
            let Some(due) = due else {
                capped = false;
                break;
            };
            // Every wake-up waits, even for an instant already past: that is what keeps the
            // cap above from ever costing a CPU-bound spin, and it hands the runtime back
            // between wake-ups.
            let wait = due
                .duration_since(now)
                .unwrap_or_default()
                .clamp(floor, MAX_CLOCK_WAIT);
            ::tokio::time::sleep(wait).await;
            // Both publications are made, whichever of them failed: they are owed
            // independently, and the floor below is what paces a pump whose work is not
            // getting through.
            let announced = publish_due(&pumped, &callback, &region).await;
            let metered =
                query_metrics::publish_due_query_metrics(&pumped, cloudwatch.as_ref());
            floor = match announced && metered {
                true => MIN_CLOCK_WAIT,
                false => MAX_CLOCK_WAIT,
            };
        }
        if capped {
            // Only THIS pump's claim, so a pump started since is left holding its own.
            pumped.lock().release_query_clock_pump(generation);
        }
    });
    state.lock().set_query_clock_pump(generation, handle);
}

/// `at` as the epoch milliseconds an event's `time` carries.
fn epoch_millis(at: SystemTime) -> i64 {
    i64::try_from(
        at.duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or_default()
}
