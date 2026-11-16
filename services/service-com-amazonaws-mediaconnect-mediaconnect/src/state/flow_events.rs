// SPDX-License-Identifier: CC0-1.0

//! The EventBridge events this service publishes as a flow moves between statuses.
//!
//! # The rule this module implements
//!
//! MediaConnect is an event SOURCE, not only a control plane a client polls: every status a
//! flow MOVES INTO is published onto the account's default event bus
//! ([`DEFAULT_EVENT_BUS`]) under the `source` this service owns ([`EVENT_SOURCE`]) and the
//! `detail-type` of a flow's status change ([`FLOW_STATUS_CHANGE_DETAIL_TYPE`]), naming the
//! flow by its own ARN in the envelope's `resources` and carrying the status it moved into
//! in the event's `detail`. A rule on that bus whose `EventPattern` matches then delivers
//! it to the rule's targets, so a client that reacts to a flow coming up or going down
//! through EventBridge observes it without calling MediaConnect again.
//!
//! The statuses a flow moves between are the modelled `Status` values the lifecycle
//! reaches ([`STATUS_STANDBY`], [`STATUS_STARTING`], [`STATUS_ACTIVE`], [`STATUS_STOPPING`],
//! [`STATUS_DELETING`]). The status a CREATE stores is not a move — a flow that has only
//! just been minted came from nowhere — so nothing is published for it; every later change
//! of that status is a move.
//!
//! # How a transition becomes an event
//!
//! Nothing here decides WHEN a flow moves: the stored due instants do
//! ([`TRANSITION_SPAN`]). A transition is recorded in exactly two places, which is what
//! makes one move produce one event however it was materialised:
//!
//! * the transient status a lifecycle call enters
//!   ([`ComAmazonawsMediaconnectMediaconnectState::begin_flow_transition`], the one door a
//!   handler begins a flow transition through), and the terminal status a delete reports;
//! * the settled status the CLOCK owes, materialised by
//!   ([`ComAmazonawsMediaconnectMediaconnectState::settle_due_transitions`]) — whichever
//!   call, or the clock pump below, happens to run it.
//!
//! The queue of recorded changes is bounded ([`MAX_PENDING_FLOW_EVENTS`], oldest dropped
//! first) because every transition appends to it, and each entry carries the Region of the
//! bus it belongs on, read off the flow's own ARN.
//!
//! A queued change is published with NO store guard held ([`publish_pending_flow_events`]):
//! EventBridge is reached the way every emulated service reaches another, through the
//! framework's in-process callback, so this crate names no other service's crate, and a
//! target the event sets off may call back into this service. Publication is best effort —
//! an emulator with no EventBridge mounted has nowhere to publish — so a failure is logged
//! and the operation's own answer stands.
//!
//! WHICH call publishes: the door that began the transition publishes its own change before
//! it answers, and the status the clock owes is published by this store's CLOCK PUMP
//! ([`drive_flow_clock`]), which waits for the due instant, materialises it and publishes
//! what it moved. The pump is why a client that waits on the event alone — making no
//! further MediaConnect call — still sees a flow reach `ACTIVE`.

use ::std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::*;

/// The `source` of every event MediaConnect publishes.
///
/// EventBridge names the publishing service in an event's `source` as `aws.<service>`, and
/// this service's namespace is the one its ARNs carry ([`SERVICE_NAMESPACE`]).
pub const EVENT_SOURCE: &str = "aws.mediaconnect";

/// The `detail-type` of the event published when a FLOW changes status.
pub const FLOW_STATUS_CHANGE_DETAIL_TYPE: &str = "MediaConnect Flow Status Change";

/// The event bus this service publishes onto.
///
/// The events an AWS service generates go to the account's default event bus, which is the
/// bus a rule carrying no `EventBusName` is created on.
pub const DEFAULT_EVENT_BUS: &str = "default";

/// How many flow status changes the queue holds at once.
///
/// This module's OWN ceiling, not an AWS quota: the queue lives in memory and every
/// transition appends to it, so without a bound a client starting and stopping without end
/// could grow it without end. The oldest change is dropped first, so the transitions the
/// live flows just made are the ones that survive. It is far above what a realistic burst
/// produces (a start contributes two changes).
pub const MAX_PENDING_FLOW_EVENTS: usize = 2_000;

/// How many entries one `PutEvents` request carries.
///
/// The bound the service this publishes to declares: its `PutEventsRequestEntryList` is
/// `@length(min: 1, max: 10)`, so a drain holding more changes than that is published in
/// several requests rather than one the service would refuse.
const MAX_PUT_EVENTS_ENTRIES: usize = 10;

/// How many changes ONE drain publishes.
///
/// The queue itself is bounded ([`MAX_PENDING_FLOW_EVENTS`]), but one call draining all of
/// it would fan out to a rule's targets that many times: this caps one drain, and
/// [`publish_pending_flow_events`] repeats a drain until the queue is empty, so nothing
/// waits for a later call.
const MAX_PUBLISHED_PER_DRAIN: usize = 50;

/// The shortest one wake-up of the clock pump waits.
///
/// A due instant already in the past is materialised on the NEXT wake-up, not at this one's
/// instant, so the pump always hands the runtime back and a wake-up that materialises
/// nothing cannot turn the loop into a spin. It is well below [`TRANSITION_SPAN`], so it
/// never delays a transition noticeably.
const MIN_CLOCK_WAIT: Duration = Duration::from_millis(5);

/// The longest one wake-up of the clock pump waits before it looks at the store again.
///
/// Every status a flow owes falls due one [`TRANSITION_SPAN`] after the call that began it,
/// so a wait is never near this value; it is a backstop, so a due instant far in the future
/// cannot park the pump indefinitely.
const MAX_CLOCK_WAIT: Duration = Duration::from_secs(5);

/// How many wake-ups one clock pump makes before it stands down.
///
/// A backstop, not a budget: the transitions the store owes are already finite (a flow owes
/// at most one at a time, and the flow map is bounded), and every wake-up either
/// materialises a transition or clears the due instant that woke it. The cap guarantees
/// termination even if a later change left a due instant that materialising does not clear,
/// and the next lifecycle call starts a fresh pump.
const MAX_CLOCK_WAKE_UPS: usize = 10_000;

/// One flow status change waiting to be published.
///
/// Self-contained — WHICH flow moved, FROM which status, INTO which status, WHEN, and the
/// Region of the bus it belongs on — because the status a change reports is the one the
/// flow held at that instant: a flow that has since moved again, or that a delete has taken
/// out of the store, still moved through this status and the event still reports it.
#[derive(Debug, Clone)]
pub struct FlowStatusChange {
    /// The ARN of the flow that moved, which the envelope carries in `resources`: the event
    /// names the resource it is about, which is what a rule filtering on `resources` reads.
    pub flow_arn: String,
    /// The Region whose default bus this change belongs on, read off the flow's own ARN.
    pub region: String,
    /// The status the flow held before the move.
    pub previous_status: String,
    /// The status it moved INTO.
    pub current_status: String,
    /// WHEN it moved — the instant the move was due, not the instant some later call
    /// materialised or published it.
    pub at: SystemTime,
}

impl FlowStatusChange {
    /// The change the flow `flow_arn` made at `at`, for the bus of the Region its ARN names.
    #[must_use]
    pub fn new(flow_arn: &str, previous_status: &str, current_status: &str, at: SystemTime) -> Self {
        Self {
            flow_arn: flow_arn.to_string(),
            region: flow_arn_region(flow_arn),
            previous_status: previous_status.to_string(),
            current_status: current_status.to_string(),
            at,
        }
    }

    /// The `detail` of a flow status change: the status the flow moved out of and the status
    /// it is in now, which is what a consumer keyed on a flow coming up or going down reads.
    fn detail(&self) -> ::serde_json::Value {
        ::serde_json::json!({
            "previousStatus": self.previous_status,
            "currentStatus": self.current_status,
        })
    }
}

/// The Region segment of the flow ARN `flow_arn`, or [`DEFAULT_REGION`] when it carries no
/// Region token.
///
/// Every ARN this service mints carries the Region it was minted in
/// ([`is_mintable_flow_arn`]), so the flow's own identity is where the bus it publishes onto
/// is read from — no part of a caller's later request decides it.
#[must_use]
pub fn flow_arn_region(flow_arn: &str) -> String {
    flow_arn
        .split(':')
        .nth(3)
        .filter(|region| is_region_token(region))
        .unwrap_or(DEFAULT_REGION)
        .to_string()
}

impl ComAmazonawsMediaconnectMediaconnectState {
    /// Enter `transient` on the flow `flow_arn` now, owe `settled` one [`TRANSITION_SPAN`]
    /// later, and record the status change for publication.
    ///
    /// The ONE door a handler begins a flow transition through, so a lifecycle call cannot
    /// move a flow without the move being published.
    pub fn begin_flow_transition(
        &mut self,
        flow_arn: &str,
        transient: &str,
        settled: &str,
        now: SystemTime,
    ) {
        let Some(flow) = self.flows.get_mut(flow_arn) else {
            return;
        };
        let previous_status = flow.status.clone();
        flow.begin_transition(transient, settled, now);
        self.record_flow_status_change(FlowStatusChange::new(
            flow_arn,
            &previous_status,
            transient,
            now,
        ));
    }

    /// Queue `change` for publication to EventBridge.
    ///
    /// Called from the places that WRITE a flow's status — the transition door above, the
    /// delete that reports the terminal status, and the clock's own settle — so every move a
    /// flow makes is recorded exactly once and none can be published twice.
    pub fn record_flow_status_change(&mut self, change: FlowStatusChange) {
        while self.pending_flow_events.len() >= MAX_PENDING_FLOW_EVENTS {
            self.pending_flow_events.pop_front();
            ::tracing::warn!(
                "the queue of MediaConnect flow status changes waiting to be published to \
                 EventBridge is full at {MAX_PENDING_FLOW_EVENTS}; the oldest change was \
                 dropped"
            );
        }
        self.pending_flow_events.push_back(change);
    }

    /// Take at most `limit` of the changes not yet published that belong on `region`'s bus,
    /// oldest first.
    ///
    /// Taking the changes out under the lock is what stops two concurrent calls from
    /// publishing one change twice.
    fn take_flow_status_changes(&mut self, region: &str, limit: usize) -> Vec<FlowStatusChange> {
        let mut taken = Vec::new();
        self.pending_flow_events.retain(|change| {
            if taken.len() < limit && change.region == region {
                taken.push(change.clone());
                return false;
            }
            true
        });
        taken
    }

    /// Every Region a queued change belongs on, in the order the changes were recorded.
    fn pending_flow_event_regions(&self) -> Vec<String> {
        let mut regions: Vec<String> = Vec::new();
        for change in &self.pending_flow_events {
            if !regions.iter().any(|region| region == &change.region) {
                regions.push(change.region.clone());
            }
        }
        regions
    }

    /// How many changes are queued for publication — reporting only, for a reader that has
    /// to tell "nothing moved" from "nothing was published".
    #[must_use]
    pub fn pending_flow_event_count(&self) -> usize {
        self.pending_flow_events.len()
    }

    /// The earliest instant the clock owes a flow a status change at, or `None` when it owes
    /// none — what the pump waits for.
    fn next_flow_transition_due_at(&self) -> Option<SystemTime> {
        self.flows
            .values()
            .filter_map(|flow| flow.pending.as_ref().map(|pending| pending.due_at))
            .min()
    }

    /// What the pump waits for next, or `None` — having RELEASED its claim — when this store
    /// owes no flow a status change and has nothing queued.
    ///
    /// Observing "nothing is owed" and standing down happen in ONE critical section, which
    /// is what makes the hand-over race-free: a door records its change, and stores the due
    /// instant of the change the clock owes, under this same guard BEFORE it tries to claim
    /// the pump — so either this call sees that instant and the running pump keeps it, or
    /// the door's claim finds the flag already cleared and starts a pump of its own. A
    /// change recorded but not yet published keeps the pump awake too, so nothing queued
    /// waits for the next call.
    fn next_due_or_release_flow_pump(
        &mut self,
        now: SystemTime,
        generation: u64,
    ) -> Option<SystemTime> {
        if !self.pending_flow_events.is_empty() {
            return Some(now);
        }
        match self.next_flow_transition_due_at() {
            Some(due) => Some(due),
            None => {
                self.release_flow_clock_pump(generation);
                None
            }
        }
    }

    /// Claim the single pump for this store: the claim's GENERATION when the caller may
    /// start one, `None` when one is already running.
    ///
    /// The generation is what makes a release safe: a pump releases its own claim and never
    /// a later pump's (see [`Self::release_flow_clock_pump`]).
    fn start_flow_clock_pump(&mut self) -> Option<u64> {
        if self.flow_clock_pump_running {
            return None;
        }
        self.flow_clock_pump_running = true;
        self.flow_clock_pump_generation = self.flow_clock_pump_generation.wrapping_add(1);
        Some(self.flow_clock_pump_generation)
    }

    /// Release the claim `generation` took — and ONLY that one.
    ///
    /// A pump that ends on the wake-up cap must not clear a claim a pump started since is
    /// holding, or two pumps would run against one store.
    fn release_flow_clock_pump(&mut self, generation: u64) {
        if self.flow_clock_pump_generation == generation {
            self.flow_clock_pump_running = false;
        }
    }

    /// Keep the running pump's task handle, aborting the one it replaces.
    ///
    /// The replaced handle is a pump that has already ended — by standing down, or on its
    /// wake-up cap — so the abort is normally a no-op; it is what guarantees the invariant
    /// anyway, that no earlier pump task is still running beside the one that now holds the
    /// claim.
    fn set_flow_clock_pump(&mut self, handle: ::tokio::task::JoinHandle<()>) {
        if let Some(previous) = self.flow_clock_pump.replace(handle) {
            previous.abort();
        }
    }
}

/// Publish every queued flow status change onto the default EventBridge bus of the Region it
/// belongs to, and leave the queue empty.
///
/// Called with NO store guard held: each drain takes its changes out in one critical
/// section, so nothing is published twice, and the in-process `PutEvents` runs outside it. A
/// Region's changes are drained until it has none left — one drain is capped
/// ([`MAX_PUBLISHED_PER_DRAIN`]) so no single call is unbounded. Nothing about the answer an
/// operation gives changes either way.
pub async fn publish_pending_flow_events(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    state: &::engine::ServiceState<ComAmazonawsMediaconnectMediaconnectState>,
) {
    // The Regions are read out in ONE critical section, so the guard the loop below runs
    // under is released before any publication: a temporary guard held for the whole loop
    // would be held across the in-process call.
    let regions = state.lock().pending_flow_event_regions();
    for region in regions {
        let client = ::service_common::eventbridge_client::eventbridge_client(callback, &region);
        let mut events = state
            .lock()
            .take_flow_status_changes(&region, MAX_PUBLISHED_PER_DRAIN);
        while !events.is_empty() {
            publish_batch(&client, &events).await;
            events = state
                .lock()
                .take_flow_status_changes(&region, MAX_PUBLISHED_PER_DRAIN);
        }
    }
}

/// Publish one drain, in `PutEvents` requests of at most [`MAX_PUT_EVENTS_ENTRIES`] entries.
async fn publish_batch(client: &::aws_sdk_eventbridge::Client, events: &[FlowStatusChange]) {
    for window in events.chunks(MAX_PUT_EVENTS_ENTRIES) {
        let mut request = client.put_events();
        for event in window {
            let entry = ::aws_sdk_eventbridge::types::PutEventsRequestEntry::builder()
                .event_bus_name(DEFAULT_EVENT_BUS)
                .source(EVENT_SOURCE)
                .detail_type(FLOW_STATUS_CHANGE_DETAIL_TYPE)
                // The event's `time` is WHEN the flow moved, not when the queue was drained:
                // `PutEventsRequestEntry$Time` is "the time stamp of the event, per RFC3339.
                // If no time stamp is provided, the time stamp of the PutEvents call is
                // used."
                .time(::aws_sdk_eventbridge::primitives::DateTime::from_millis(
                    epoch_millis(event.at),
                ))
                .resources(&event.flow_arn)
                .detail(event.detail().to_string())
                .build();
            request = request.entries(entry);
        }
        if let Err(error) = request.send().await {
            ::tracing::warn!(
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "a MediaConnect flow status change could not be published to EventBridge"
            );
        }
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

/// Start this store's flow clock pump, unless one is already running.
///
/// Called by the doors that leave the store owing a flow a status change — StartFlow and
/// StopFlow — with no store guard held. It returns at once: the waiting, the settling and
/// the publication are the spawned task's, so no request waits on the clock. With no async
/// runtime to spawn onto there is no pump, and the transitions are materialised by the next
/// caller instead.
pub fn drive_flow_clock(
    callback: &smithy_server_runtime::EmbeddedEmulator,
    state: &::engine::ServiceState<ComAmazonawsMediaconnectMediaconnectState>,
) {
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(generation) = state.lock().start_flow_clock_pump() else {
        return;
    };
    let callback = callback.clone();
    let pumped = ::engine::ServiceState::clone(state);
    let state = ::engine::ServiceState::clone(state);
    let handle = runtime.spawn(async move {
        // `true` only if the wake-up cap below is what ended the loop; a pump that ran out of
        // work has already released its claim inside the one critical section that observed
        // it ([`ComAmazonawsMediaconnectMediaconnectState::next_due_or_release_flow_pump`]).
        let mut capped = true;
        for _ in 0..MAX_CLOCK_WAKE_UPS {
            let now = SystemTime::now();
            let Some(due) = state.lock().next_due_or_release_flow_pump(now, generation) else {
                capped = false;
                break;
            };
            // Every wake-up waits, even for an instant already past: that is what keeps the
            // backstop above from ever costing a CPU-bound spin, and it hands the runtime
            // back between wake-ups.
            let wait = due
                .duration_since(now)
                .unwrap_or_default()
                .clamp(MIN_CLOCK_WAIT, MAX_CLOCK_WAIT);
            ::tokio::time::sleep(wait).await;
            state.lock().settle_due_transitions(SystemTime::now());
            publish_pending_flow_events(&callback, &state).await;
        }
        if capped {
            // Only THIS pump's claim, so a pump started since is left holding its own.
            state.lock().release_flow_clock_pump(generation);
        }
    });
    // The handle is kept so the pump it replaces is aborted rather than merely forgotten:
    // that is what guarantees no earlier pump task runs beside the one holding the claim.
    pumped.lock().set_flow_clock_pump(handle);
}

/// Publish what THIS call recorded, and hand the status the clock still owes to the pump.
///
/// The one call a lifecycle door makes once it has RELEASED the store's guard, so no guard
/// is held across the in-process publication: a call that recorded no transition publishes
/// nothing, so it costs one probe of an empty queue.
pub async fn publish_flow_status_changes<Eng: Send + Sync + 'static>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    engine: &Eng,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    publish_pending_flow_events(ctx.callback(), &state).await;
    drive_flow_clock(ctx.callback(), &state);
}
