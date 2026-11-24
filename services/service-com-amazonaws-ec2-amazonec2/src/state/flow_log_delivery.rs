// SPDX-License-Identifier: CC0-1.0

//! The DELIVERY of a flow log's records: which interfaces it captures, when a window closes,
//! and the write into the CloudWatch Logs group the flow log names.
//!
//! # The rule this module implements
//!
//! A flow log is monitoring, not just a record in a store: "Creates one or more flow logs to
//! capture information about IP traffic for a specific network interface, subnet, or VPC …
//! When publishing to CloudWatch Logs, flow log records are published to a log group, and
//! each network interface has a unique log stream in the log group" (`CreateFlowLogs`). So an
//! ACTIVE flow log publishes, for every aggregation window and for every network interface
//! its monitored resource CONTAINS, the records of that interface's traffic — one record per
//! flow, written in the fields its own `LogFormat` names and in that order
//! ([`super::flow_log_records`]) — into that group, in the stream that interface's records
//! travel ([`flow_log_stream_name`]). `TrafficType` is the filter over those records: it
//! selects on the `action` the VPC's filters took ([`traffic_type_captures`]), so an `ACCEPT`
//! flow log carries no record of a flow the filters denied and a `REJECT` one carries no
//! record of a flow they admitted.
//!
//! WHICH interfaces a flow log captures is what its scope means
//! ([`ComAmazonawsEc2Amazonec2State::flow_log_monitored_interfaces`]): the one interface of a
//! `NetworkInterface` flow log, every interface of the subnet of a `Subnet` one, and every
//! interface of the VPC of a `VPC` one. That is the whole of the `interface-id` field's
//! meaning — a record names the interface it is about, so a flow log's scope is observable as
//! the set of interfaces its group carries records for.
//!
//! # What the records report
//!
//! Each window reports the flows this emulator OBSERVED on that interface
//! ([`ComAmazonawsEc2Amazonec2State::observed_interface_flows`]). Nothing in this emulator
//! sends a packet between two instances: a guest's own transfer program reaches servers of
//! the machine it runs on through that machine's loopback interface and nothing else
//! ([`super::guest_http_client`]), and no other component here opens a socket, so no flow is
//! ever accounted to an elastic network interface. An interval with no traffic is not an
//! interval with no RECORD, though: AWS publishes one for it — "NODATA — There was no network
//! traffic to or from the network interface during the aggregation interval" (Amazon VPC User
//! Guide, "Flow log records", the `log-status` field), whose own documented example writes
//! the fields of the flow as `-`. That is what each window of each captured interface
//! therefore carries, rather than a flow nothing observed: the record AWS publishes for the
//! traffic there was.
//!
//! # The clock a window closes on
//!
//! Publishing is owed by the CLOCK, not by any call: [`drive_flow_log_delivery`] is taken by
//! the writes that create and delete a flow log, and it publishes what is already due and
//! starts the one bounded in-process task that waits for the next window
//! ([`FLOW_LOG_CAPTURE_WINDOW`]). Nothing a pass publishes depends on who called.
//!
//! # The delivery role
//!
//! The records are written UNDER the flow log's own delivery role — "The ARN of the IAM role
//! that allows Amazon EC2 to publish flow logs to the log destination"
//! (`CreateFlowLogsRequest$DeliverLogsPermissionArn`) — so a role whose permissions do not
//! allow the write that puts them in the group cannot deliver. IAM decides that, not this
//! service ([`role_withholds_the_delivery_write`]), and a flow log whose role withholds it
//! delivers NOTHING and reports the failure where a client reads it:
//! `DeliverLogsStatus` `FAILED` with the `DeliverLogsErrorMessage` the model gives that
//! cause — "`Access error` indicates that the IAM role associated with the flow log does not
//! have sufficient permissions to publish to CloudWatch Logs"
//! (`FlowLog$DeliverLogsErrorMessage`).
//!
//! The write itself goes through the workspace's ONE vended-logs write
//! ([`::service_common::cloudwatch_logs::delivered_stamped_over`]), so a delivered record and
//! CloudWatch Logs' own `FilterLogEvents` cannot disagree about it, and the group is brought
//! into existence by the delivery — "The name of a NEW or existing CloudWatch Logs log group"
//! (`CreateFlowLogsRequest$LogGroupName`).
//!
//! Only the CloudWatch Logs destination type is published here. The Amazon S3 and Firehose
//! destinations deliver OBJECTS and RECORDS to resources of other services, which is a
//! different write; this module is the one path for the group destination.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use ::std::collections::{BTreeMap, BTreeSet};
use ::std::sync::Arc;
use ::std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::{ComAmazonawsEc2Amazonec2State as Ec2State, *};

/// The action the delivery writes a flow log's records with — the one a role that may not
/// make it cannot publish under.
///
/// "When publishing to CloudWatch Logs, flow log records are published to a log group"
/// (`CreateFlowLogs`), and putting a log event in a group is `logs:PutLogEvents`: that is the
/// write the delivery performs on the caller's behalf, so it is the permission the delivery
/// role is asked for.
pub const FLOW_LOG_DELIVERY_WRITE_ACTION: &str = "logs:PutLogEvents";

/// The delivery outcome of a flow log whose records could not be delivered: the `FAILED`
/// member of "The status of the logs delivery (`SUCCESS` | `FAILED`)"
/// (`FlowLog$DeliverLogsStatus`).
pub const DELIVER_LOGS_STATUS_FAILED: &str = "FAILED";

/// The `DeliverLogsErrorMessage` of a flow log whose delivery role may not write to the
/// group: "`Access error` indicates that the IAM role associated with the flow log does not
/// have sufficient permissions to publish to CloudWatch Logs"
/// (`FlowLog$DeliverLogsErrorMessage`) — the model's own wording for exactly this cause.
pub const DELIVER_LOGS_ACCESS_ERROR: &str = "Access error";

/// How long one aggregation window of this emulator covers.
///
/// A flow log states the LONGEST a window may be — "The maximum interval of time, in seconds,
/// during which a flow of packets is captured and aggregated into a flow log record … When a
/// network interface is attached to a Nitro-based instance, the aggregation interval is
/// always 60 seconds (1 minute) or less, regardless of the specified value"
/// (`FlowLog$MaxAggregationInterval`) — so a window shorter than the flow log's own interval
/// is one that flow log admits, and the member's value is never exceeded
/// ([`capture_window_of`]).
///
/// The span itself is CHOSEN, for the reason [`super::TRANSITION_DELAY`] is chosen: it is
/// deliberately non-zero, so the window a record reports is a real span of time, and short
/// enough that a client polling its log group for the first records of a flow log it has just
/// created is not left waiting minutes for them.
pub const FLOW_LOG_CAPTURE_WINDOW: Duration = Duration::from_secs(5);

/// The shortest a delivery-clock wake-up waits, so a window already past its span still costs
/// a yield rather than a CPU-bound spin.
const MIN_DELIVERY_WAIT: Duration = Duration::from_millis(50);

/// The longest one waits, so the clock re-reads the store — which a write may have changed
/// since — at least this often.
const MAX_DELIVERY_WAIT: Duration = Duration::from_secs(5);

/// How many times one delivery clock wakes up before it stands down.
///
/// A backstop, not a schedule: it bounds the work one request can start, and the next write to
/// a flow log starts a clock again if none is running.
const MAX_CLOCK_WAKE_UPS: usize = 2_000;

/// How many windows of ONE flow log a single pass publishes — a bound on the work one wake-up
/// does, the way [`super::nat_gateway_metrics`] bounds its own pass.
const MAX_WINDOWS_PER_PASS: u32 = 4;

/// How far back a pass REACHES for the windows it still owes.
///
/// A span no clock was running for is a gap in this emulator's own capture, not in the flow
/// log's: a pass resumes at most this far back and leaves the older windows unpublished rather
/// than back-filling an arbitrary span nobody was capturing.
const MAX_BACKFILL_WINDOWS: u32 = 4;

/// Where the delivery of every CloudWatch Logs flow log of this store has got to, and the one
/// in-process clock that advances them.
///
/// Held on the store, so the schedule is the store's own: a flow log publishes while it is
/// ACTIVE, and the clock is claimed once however many writes drive it.
#[derive(Debug, Default)]
pub struct FlowLogDeliveryPublisher {
    /// The end of the last window published for each flow log, by `FlowLogId` — which is also
    /// where its capture starts: a flow log seen for the first time captures from that
    /// instant. A flow log this store no longer holds has no entry.
    published_through: BTreeMap<String, SystemTime>,
    /// Whether a clock is running for this store.
    clock_running: bool,
    /// Which claim the running clock holds, so a clock that has stood down cannot release a
    /// claim a later one took.
    clock_generation: u64,
    /// The running clock's task, held so the task a request started stays stoppable.
    clock: Option<::tokio::task::JoinHandle<()>>,
}

/// ONE network interface a flow log captures, as the fields of its records report it.
///
/// Read off the interface the store holds, so a record's `interface-id`, `vpc-id`,
/// `subnet-id`, `instance-id` and `az-id` are the ones `DescribeNetworkInterfaces` reports
/// for it rather than a second copy of them.
#[derive(Debug, Clone)]
pub struct MonitoredInterface {
    /// "The ID of the network interface" (the `interface-id` field).
    pub network_interface_id: String,
    /// "The ID of the VPC that contains the network interface" (the `vpc-id` field).
    pub vpc_id: Option<String>,
    /// "The ID of the subnet that contains the network interface" (the `subnet-id` field).
    pub subnet_id: Option<String>,
    /// "The ID of the instance that's associated with network interface" (the `instance-id`
    /// field), absent for an interface attached to no instance.
    pub instance_id: Option<String>,
    /// "The ID of the Availability Zone that contains the network interface" (the `az-id`
    /// field).
    pub availability_zone_id: Option<String>,
    /// The tags that interface carries, which is what a tag field of a custom format reports.
    pub tags: Vec<Tag>,
}

impl MonitoredInterface {
    /// The value this interface carries for the tag `key`, or `None` when it carries no such
    /// tag.
    #[must_use]
    pub fn tag_value(&self, key: &str) -> Option<String> {
        self.tags
            .iter()
            .find(|tag| tag.key.as_deref() == Some(key))
            .and_then(|tag| tag.value.clone())
    }
}

/// Whether a flow log capturing `traffic_type` records a flow whose `action` is `action`.
///
/// "The type of traffic to monitor (accepted traffic, rejected traffic, or all traffic)"
/// (`CreateFlowLogsRequest$TrafficType`), whose members are `ACCEPT`, `REJECT` and `ALL`
/// (`smithy_query(query="shape",shape="TrafficType")`): the member selects on the `action`
/// field of the record, so `ACCEPT` captures exactly the flows the VPC's filters admitted and
/// `REJECT` exactly those they denied.
///
/// A flow log carrying NO `TrafficType` captures every flow: the member "is not supported for
/// transit gateway resource types" (same member), which are the only flow logs without one,
/// and no filter on the action is then in force.
#[must_use]
pub fn traffic_type_captures(traffic_type: Option<&str>, action: &str) -> bool {
    match traffic_type {
        Some(TRAFFIC_TYPE_ACCEPT) => action == FLOW_LOG_ACTION_ACCEPT,
        Some(TRAFFIC_TYPE_REJECT) => action == FLOW_LOG_ACTION_REJECT,
        _ => true,
    }
}

/// The `TrafficType` members, spelled as the model spells them
/// (`smithy_query(query="shape",shape="TrafficType")`).
pub const TRAFFIC_TYPE_ACCEPT: &str = "ACCEPT";
pub const TRAFFIC_TYPE_REJECT: &str = "REJECT";
pub const TRAFFIC_TYPE_ALL: &str = "ALL";

/// What ONE flow log owes its destination right now: the group, the role the write is made
/// under, and the records of each of its streams.
#[derive(Debug, Clone)]
struct DueDelivery {
    /// The flow log the records belong to, so the outcome is recorded against it.
    flow_log_id: String,
    /// "The name of the flow log group" (`FlowLog$LogGroupName`).
    log_group: String,
    /// "The ARN of the IAM role allows the service to publish logs to CloudWatch Logs"
    /// (`FlowLog$DeliverLogsPermissionArn`).
    deliver_logs_permission_arn: Option<String>,
    /// One entry per interface the flow log captures: the stream its records travel, and the
    /// records themselves, each stamped with the end of the window it reports.
    streams: Vec<DueStream>,
}

/// The records ONE stream of a flow log's group is owed.
#[derive(Debug, Clone)]
struct DueStream {
    /// The stream the records travel ([`flow_log_stream_name`]).
    log_stream: String,
    /// `(instant in epoch milliseconds, record)` pairs, which is what the vended-logs write
    /// takes.
    events: Vec<(i64, String)>,
}

/// The log stream one interface's records travel in a flow log's group: the interface the
/// records are about, followed by the traffic the flow log captures.
///
/// "When publishing to CloudWatch Logs, flow log records are published to a log group, and
/// each network interface has a unique log stream in the log group" (`CreateFlowLogs`), and
/// the name AWS gives that stream is the interface id with the flow log's own `TrafficType`
/// after it, in lower case (`eni-…-all`) — so two flow logs capturing different traffic of
/// the same interface into one group keep their records apart. A flow log with no
/// `TrafficType` at all — "not supported for transit gateway resource types"
/// (`CreateFlowLogsRequest$TrafficType`) — has no such suffix to carry.
fn flow_log_stream_name(network_interface_id: &str, traffic_type: Option<&str>) -> String {
    match traffic_type {
        Some(traffic_type) => {
            format!(
                "{network_interface_id}-{captured}",
                captured = traffic_type.to_ascii_lowercase()
            )
        }
        None => network_interface_id.to_string(),
    }
}

/// Publish every flow-log window the clock has already made due, and start the one bounded
/// in-process task that waits for the next one.
///
/// Called by the writes that create and delete a flow log — never by a read: what this
/// materialises is what the stored windows have already made due, so what a caller sees does
/// not depend on whether anybody called.
///
/// With no async runtime to spawn onto there is no clock, and nothing is published until the
/// next write — the router always runs on one.
pub fn drive_flow_log_delivery<Eng: Send + Sync + 'static>(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    engine: &Arc<Eng>,
    region: &str,
) {
    spawn_flow_log_delivery_clock(ctx.callback(), engine, region);
}

/// Start the ONE delivery clock of this store, unless a clock is already driving it.
fn spawn_flow_log_delivery_clock<Eng: Send + Sync + 'static>(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    engine: &Arc<Eng>,
    region: &str,
) {
    let Some(state) = super::service_state(&**engine) else {
        return;
    };
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(generation) = claim_clock(&mut state.lock()) else {
        return;
    };
    let callback = callback.clone();
    let engine = Arc::clone(engine);
    let region = region.to_string();
    let claimed = ::engine::ServiceState::clone(&state);
    let handle = runtime.spawn(async move {
        // `true` only if the wake-up cap is what ended the loop; a clock that ran out of work
        // has already released its claim inside the one critical section that observed it.
        let mut capped = true;
        for _ in 0..MAX_CLOCK_WAKE_UPS {
            let Some(wait) = next_wait_or_release(&mut state.lock(), generation) else {
                capped = false;
                break;
            };
            // Every wake-up waits, even for a window already past: that is what keeps a due
            // window from costing a CPU-bound spin, and it hands the runtime back between
            // wake-ups.
            ::tokio::time::sleep(wait).await;
            settle_flow_log_delivery(&callback, &*engine, &region).await;
        }
        if capped {
            // Only THIS clock's claim, so a clock started since keeps its own.
            release_clock(&mut state.lock(), generation);
        }
    });
    set_clock(&mut claimed.lock(), generation, handle);
}

/// Keep the running clock's task handle, aborting the one it replaces — and only while
/// `generation` is still the CURRENT claim, so a starter whose own task has already stood down
/// installs nothing and aborts nothing.
fn set_clock(store: &mut Ec2State, generation: u64, handle: ::tokio::task::JoinHandle<()>) {
    if store.flow_log_delivery.clock_generation != generation {
        handle.abort();
        return;
    }
    if let Some(previous) = store.flow_log_delivery.clock.replace(handle) {
        previous.abort();
    }
}

/// Claim the single delivery clock for this store: the claim's GENERATION when the caller may
/// start one, `None` when one already runs.
fn claim_clock(store: &mut Ec2State) -> Option<u64> {
    if store.flow_log_delivery.clock_running {
        return None;
    }
    store.flow_log_delivery.clock_running = true;
    store.flow_log_delivery.clock_generation =
        store.flow_log_delivery.clock_generation.wrapping_add(1);
    Some(store.flow_log_delivery.clock_generation)
}

/// Release the claim `generation` took — and ONLY that one, so a clock that ended on the
/// wake-up cap cannot clear a claim a clock started since is holding.
fn release_clock(store: &mut Ec2State, generation: u64) {
    if store.flow_log_delivery.clock_generation == generation {
        store.flow_log_delivery.clock_running = false;
    }
}

/// How long the clock waits before its next pass, or `None` — having RELEASED its claim — when
/// this store holds no flow log that publishes into a log group.
///
/// Observing "nothing is owed" and standing down happen in ONE critical section, which is what
/// makes the hand-over race-free: a write holds this same lock to store the flow log BEFORE it
/// tries to claim the clock, so either this call sees that flow log and the running clock keeps
/// it, or the write's claim finds the flag cleared and starts a clock of its own.
fn next_wait_or_release(store: &mut Ec2State, generation: u64) -> Option<Duration> {
    let now = SystemTime::now();
    let mut soonest: Option<Duration> = None;
    for (flow_log_id, record) in &store.flow_logs {
        if !publishes_into_log_group(&record.view) {
            continue;
        }
        let window = capture_window_of(&record.view);
        let wait = match store.flow_log_delivery.published_through.get(flow_log_id) {
            // The window after the last one published.
            Some(last) => (*last + window)
                .duration_since(now)
                .unwrap_or(Duration::ZERO),
            // A flow log with nothing published yet opens its capture on the next pass.
            None => Duration::ZERO,
        };
        soonest = Some(soonest.map_or(wait, |held: Duration| held.min(wait)));
    }
    match soonest {
        Some(wait) => Some(wait.clamp(MIN_DELIVERY_WAIT, MAX_DELIVERY_WAIT)),
        None => {
            release_clock(store, generation);
            None
        }
    }
}

/// Deliver the records every flow log of this store owes at this instant — the ONE call the
/// clock makes for it, and the one a write makes to publish what is already due.
///
/// Called with NO guard of this store held: it takes that lock itself to read what is due (and
/// to stamp the windows it read), releases it, and only then asks IAM about the delivery role
/// and writes into CloudWatch Logs — the guard-free discipline every cross-service access of
/// this crate follows.
pub async fn settle_flow_log_delivery<Eng: Send + Sync + 'static>(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    engine: &Eng,
    region: &str,
) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let due = {
        let mut store = state.lock();
        // A resource whose own move has already fallen due IS in that state, whether or not
        // anybody called — materialising it is this store's own owed write, and the interfaces
        // a flow log captures are read after it.
        store.materialize_due_transitions();
        due_deliveries(&mut store, SystemTime::now(), region)
    };
    for delivery in due {
        if let Some(role_arn) = delivery.deliver_logs_permission_arn.as_deref() {
            // The write the records are put in the group with is made UNDER this role, so a
            // role whose permissions do not allow it cannot deliver them. IAM answers, not
            // this service, and only an answer ABOUT this role's permissions is evidence: a
            // role whose policy set is EMPTY says nothing about the write — the routine state
            // of a role provisioned out of band — while a role that states permissions is the
            // authority on what it may do ([`role_withholds_the_delivery_write`]).
            if role_withholds_the_delivery_write(
                callback,
                region,
                role_arn,
                &delivery.log_group,
            )
            .await
            {
                record_delivery_outcome(&state, &delivery.flow_log_id, false);
                continue;
            }
        }
        let mut delivered_everything = true;
        for stream in &delivery.streams {
            if stream.events.is_empty() {
                continue;
            }
            let delivered = ::service_common::cloudwatch_logs::delivered_stamped_over(
                callback,
                region,
                &delivery.log_group,
                &stream.log_stream,
                &stream.events,
            )
            .await;
            if !delivered {
                delivered_everything = false;
                ::tracing::warn!(
                    log_group = %delivery.log_group,
                    log_stream = %stream.log_stream,
                    "this flow log's records could not be delivered to its log group"
                );
            }
        }
        record_delivery_outcome(&state, &delivery.flow_log_id, delivered_everything);
    }
}

/// Whether IAM answers that the delivery role `role_arn` names may NOT make the write that
/// puts a flow log's records in the group `log_group_arn`.
///
/// Two questions, in the order the evidence has to be read:
///
/// * does that role state identity-based permissions at all
///   ([`::service_common::iam_role::role_states_permissions_through`])? A role whose policy
///   set is empty says nothing about the write — a delivery role is routinely provisioned
///   out of band, and its absence of policies here is evidence about this emulator's IAM
///   rather than about the role — so nothing is withheld on it;
/// * and if it does, do those permissions allow
///   [`FLOW_LOG_DELIVERY_WRITE_ACTION`] on that group
///   ([`::service_common::iam_role::role_withholds_action_through`])? A role that states
///   permissions is the authority on what it may do, so a set that allows the write
///   elsewhere, or does not allow it at all, is a role that cannot publish into this group.
///   The question is put for each FORM a policy over a log group names it in — the group, and
///   the group together with everything under it, which is how CloudWatch Logs reports a
///   group's own ARN ([`log_group_arns`]) — so a grant written against either form is the
///   grant it is.
///
/// Both are put to IAM over the in-process callback, so the documents in force are collected
/// and decided by the service that HOLDS them. Call it with NO guard of this store held: it
/// awaits.
async fn role_withholds_the_delivery_write(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
    role_arn: &str,
    log_group: &str,
) -> bool {
    if !::service_common::iam_role::role_states_permissions_through(callback, region, role_arn)
        .await
    {
        return false;
    }
    for log_group_arn in log_group_arns(region, log_group) {
        let withheld = ::service_common::iam_role::role_withholds_action_through(
            callback,
            region,
            role_arn,
            FLOW_LOG_DELIVERY_WRITE_ACTION,
            &log_group_arn,
        )
        .await;
        if !withheld {
            return false;
        }
    }
    true
}

/// Record what became of one flow log's delivery, where `DescribeFlowLogs` reports it: "The
/// status of the logs delivery (`SUCCESS` | `FAILED`)" (`FlowLog$DeliverLogsStatus`) and
/// "Information about the error that occurred" (`$DeliverLogsErrorMessage`), which exists only
/// for a delivery that failed.
///
/// A flow log the store no longer holds — deleted while the write was in flight — records
/// nothing.
fn record_delivery_outcome(
    state: &::engine::ServiceState<Ec2State>,
    flow_log_id: &str,
    delivered: bool,
) {
    let mut store = state.lock();
    let Some(record) = store.flow_logs.get_mut(flow_log_id) else {
        return;
    };
    if delivered {
        record.view.deliver_logs_status = Some(DELIVER_LOGS_STATUS_SUCCESS.to_string());
        record.view.deliver_logs_error_message = None;
    } else {
        record.view.deliver_logs_status = Some(DELIVER_LOGS_STATUS_FAILED.to_string());
        record.view.deliver_logs_error_message = Some(DELIVER_LOGS_ACCESS_ERROR.to_string());
    }
}

/// Whether this flow log publishes into a CloudWatch Logs log group right now: an ACTIVE flow
/// log whose destination type is that one and which names the group its records go to.
fn publishes_into_log_group(view: &FlowLog) -> bool {
    view.flow_log_status.as_deref() == Some(FLOW_LOG_STATUS_ACTIVE)
        && view.log_destination_type.as_deref() == Some(LOG_DESTINATION_TYPE_CLOUD_WATCH_LOGS)
        && view.log_group_name.is_some()
}

/// The span one window of this flow log covers: [`FLOW_LOG_CAPTURE_WINDOW`], held to the
/// MAXIMUM the flow log itself states ("The maximum interval of time, in seconds, during which
/// a flow of packets is captured and aggregated into a flow log record",
/// `FlowLog$MaxAggregationInterval`), so a flow log created with a shorter interval than this
/// emulator's own span captures over its own.
fn capture_window_of(view: &FlowLog) -> Duration {
    let stated = view
        .max_aggregation_interval
        .and_then(|seconds| u64::try_from(seconds).ok())
        .map_or(FLOW_LOG_CAPTURE_WINDOW, Duration::from_secs);
    FLOW_LOG_CAPTURE_WINDOW.min(stated)
}

/// What every flow log of this store owes at `now`, advancing each past the windows it
/// publishes.
fn due_deliveries(store: &mut Ec2State, now: SystemTime, region: &str) -> Vec<DueDelivery> {
    let publishing: BTreeSet<String> = store
        .flow_logs
        .iter()
        .filter(|(_, record)| publishes_into_log_group(&record.view))
        .map(|(flow_log_id, _)| flow_log_id.clone())
        .collect();
    // A flow log that is gone, or no longer publishing into a group, owes nothing more; a
    // later one of the same id would open a capture of its own.
    store
        .flow_log_delivery
        .published_through
        .retain(|flow_log_id, _| publishing.contains(flow_log_id));
    let account_id = ::engine::emulator_account_id();
    let mut due: Vec<DueDelivery> = Vec::new();
    for flow_log_id in publishing {
        let windows = due_windows(store, &flow_log_id, now);
        if windows.is_empty() {
            continue;
        }
        let Some(view) = store
            .flow_logs
            .get(&flow_log_id)
            .map(|record| record.view.clone())
        else {
            continue;
        };
        let Some(log_group) = view.log_group_name.clone() else {
            continue;
        };
        let log_format = view
            .log_format
            .clone()
            .unwrap_or_else(|| DEFAULT_FLOW_LOG_FORMAT.to_string());
        let version = flow_log_format_version(&log_format);
        let interfaces = store.flow_log_monitored_interfaces(view.resource_id.as_deref());
        let mut streams: Vec<DueStream> = Vec::with_capacity(interfaces.len());
        for interface in interfaces {
            let mut events: Vec<(i64, String)> = Vec::new();
            for (window_start, window_end) in &windows {
                let observed = store.observed_interface_flows(
                    &interface.network_interface_id,
                    *window_start,
                    *window_end,
                );
                let captured: Vec<&ObservedFlow> = observed
                    .iter()
                    .filter(|flow| {
                        traffic_type_captures(view.traffic_type.as_deref(), &flow.action)
                    })
                    .collect();
                let stamp = epoch_millis(*window_end);
                let window = (epoch_secs(*window_start), epoch_secs(*window_end));
                if captured.is_empty() {
                    // The record AWS publishes for a window the interface carried no traffic
                    // in ([`FLOW_LOG_STATUS_NODATA`]).
                    events.push((
                        stamp,
                        window_record_line(
                            &log_format,
                            version,
                            account_id,
                            region,
                            &interface,
                            window,
                            None,
                        ),
                    ));
                    continue;
                }
                for flow in captured {
                    events.push((
                        stamp,
                        window_record_line(
                            &log_format,
                            version,
                            account_id,
                            region,
                            &interface,
                            window,
                            Some(flow),
                        ),
                    ));
                }
            }
            streams.push(DueStream {
                log_stream: flow_log_stream_name(
                    &interface.network_interface_id,
                    view.traffic_type.as_deref(),
                ),
                events,
            });
        }
        due.push(DueDelivery {
            flow_log_id,
            log_group,
            deliver_logs_permission_arn: view.deliver_logs_permission_arn.clone(),
            streams,
        });
    }
    due
}

/// The LINE one window's record is published as, for `interface` in the window
/// `(start, end)` epoch seconds, reporting `flow` — or the record a window with no traffic in
/// it is published as, when there is none.
///
/// A named function rather than a closure, because the facts it assembles borrow both the
/// interface (which outlives the window) and the flow (which does not), and one lifetime
/// covers both only where it is written down.
fn window_record_line(
    log_format: &str,
    version: u32,
    account_id: &str,
    region: &str,
    interface: &MonitoredInterface,
    window: (i64, i64),
    flow: Option<&ObservedFlow>,
) -> String {
    let facts = FlowLogRecordFacts {
        version,
        account_id,
        region,
        interface,
        window_start_secs: window.0,
        window_end_secs: window.1,
        flow,
    };
    flow_log_record_line(log_format, &facts)
}

/// The ARNs a policy over the log group `log_group` names it by: the group together with
/// everything under it — `arn:aws:logs:<region>:<account>:log-group:<name>:*`, the form
/// CloudWatch Logs reports a group's own ARN in — and the group on its own, which is the other
/// form the shared reading of such an ARN accepts
/// (`service_common::cloudwatch_logs::log_group_of_arn`).
///
/// Both, because a grant of the delivery write is written against either, and a role granted
/// the write on the group it is delivering into may deliver into it however its policy spelled
/// that group.
fn log_group_arns(region: &str, log_group: &str) -> [String; 2] {
    [
        ::engine::emulator_arn("logs", region, &format!("log-group:{log_group}:*")),
        ::engine::emulator_arn("logs", region, &format!("log-group:{log_group}")),
    ]
}

/// The windows `flow_log_id` still owes at `now`, as `(start, end)` instants, advancing its
/// capture past them.
///
/// A flow log with nothing published yet opens its capture at this instant — the first window
/// anything could have been captured in — and one that has published owes the windows whose
/// span has passed since its last, at most [`MAX_WINDOWS_PER_PASS`] of them and reaching no
/// further back than [`MAX_BACKFILL_WINDOWS`].
fn due_windows(
    store: &mut Ec2State,
    flow_log_id: &str,
    now: SystemTime,
) -> Vec<(SystemTime, SystemTime)> {
    let window = store
        .flow_logs
        .get(flow_log_id)
        .map_or(FLOW_LOG_CAPTURE_WINDOW, |record| {
            capture_window_of(&record.view)
        });
    let Some(published_through) = store
        .flow_log_delivery
        .published_through
        .get_mut(flow_log_id)
    else {
        store
            .flow_log_delivery
            .published_through
            .insert(flow_log_id.to_string(), now);
        return Vec::new();
    };
    // A gap no clock was running for is this emulator's, not the flow log's: the capture
    // resumes inside the backfill span rather than publishing every window of it.
    if let Some(earliest) = now.checked_sub(window.saturating_mul(MAX_BACKFILL_WINDOWS)) {
        if *published_through < earliest {
            *published_through = earliest;
        }
    }
    let mut windows: Vec<(SystemTime, SystemTime)> = Vec::new();
    while windows.len() < MAX_WINDOWS_PER_PASS as usize {
        let end = *published_through + window;
        if end > now {
            break;
        }
        windows.push((*published_through, end));
        *published_through = end;
    }
    windows
}

/// `at` as the epoch SECONDS a record's `start` and `end` fields are written in: "The time, in
/// Unix seconds, of the start of the aggregation interval" (the `start` field). An instant
/// before the epoch, which no clock reports, reads as the epoch itself.
fn epoch_secs(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_secs()).unwrap_or(i64::MAX))
}

/// `at` in milliseconds since the epoch, which is what a log event's timestamp is expressed in
/// (`InputLogEvent$timestamp`).
fn epoch_millis(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0, |since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
}

impl Ec2State {
    /// The network interfaces the flow log monitoring `resource_id` CAPTURES — the set of
    /// interfaces its records' `interface-id` field can name.
    ///
    /// "Creates one or more flow logs to capture information about IP traffic for a specific
    /// network interface, subnet, or VPC" (`CreateFlowLogs`): a network interface contains
    /// itself, a subnet contains the interfaces in it, and a VPC contains the interfaces of
    /// its subnets. A NAT gateway contains the interfaces it handles traffic through, which is
    /// where its traffic is captured.
    ///
    /// The resource is resolved against the ONE store this service keeps that kind of resource
    /// in, so a flow log never captures an interface no describe of its monitored resource
    /// would account to it. A transit gateway resource captures no elastic network interface
    /// at all: a transit gateway flow log's records are the transit gateway record fields
    /// (`tgw-…`), which describe an attachment rather than an interface.
    ///
    /// A READ: it answers from what the store holds and writes nothing.
    #[must_use]
    pub fn flow_log_monitored_interfaces(
        &self,
        resource_id: Option<&str>,
    ) -> Vec<MonitoredInterface> {
        let Some(resource_id) = resource_id else {
            return Vec::new();
        };
        let captured: Vec<&NetworkInterface> = if self
            .network_interfaces
            .contains_key(resource_id)
        {
            self.network_interfaces
                .get(resource_id)
                .into_iter()
                .collect()
        } else if self.subnets.contains_key(resource_id) {
            self.network_interfaces
                .values()
                .filter(|interface| interface.subnet_id.as_deref() == Some(resource_id))
                .collect()
        } else if self.vpcs.contains_key(resource_id) {
            self.network_interfaces
                .values()
                .filter(|interface| interface.vpc_id.as_deref() == Some(resource_id))
                .collect()
        } else if let Some(gateway) = self.nat_gateways.get(resource_id) {
            let held: BTreeSet<&str> = gateway
                .nat_gateway_addresses
                .iter()
                .flatten()
                .filter_map(|address| address.network_interface_id.as_deref())
                .collect();
            self.network_interfaces
                .iter()
                .filter(|(network_interface_id, _)| held.contains(network_interface_id.as_str()))
                .map(|(_, interface)| interface)
                .collect()
        } else {
            Vec::new()
        };
        captured
            .into_iter()
            .filter_map(|interface| {
                Some(MonitoredInterface {
                    network_interface_id: interface.network_interface_id.clone()?,
                    vpc_id: interface.vpc_id.clone(),
                    subnet_id: interface.subnet_id.clone(),
                    instance_id: interface
                        .attachment
                        .as_ref()
                        .and_then(|attachment| attachment.instance_id.clone()),
                    availability_zone_id: interface.availability_zone_id.clone(),
                    tags: interface.tag_set.clone().unwrap_or_default(),
                })
            })
            .collect()
    }

    /// The flows this emulator OBSERVED on the interface `network_interface_id` between
    /// `from` and `to` — the flows the records of that window report.
    ///
    /// NONE, for every interface and every window, and that is a fact about this emulator
    /// rather than a value it declines to compute: a flow is a packet that crossed the
    /// interface, and nothing here sends one. A guest's own transfer program reaches servers
    /// of the machine it runs on through that machine's LOOPBACK interface and nothing else
    /// ([`super::guest_http_client`]) — loopback traffic crosses no elastic network interface
    /// — and no other component of this emulator opens a socket between two instances. So
    /// every window of every captured interface is a window with no traffic in it, which AWS
    /// publishes a record for all the same ([`FLOW_LOG_STATUS_NODATA`]).
    ///
    /// It is a method of the store, and takes the window, because that is what the answer
    /// depends on the moment this emulator DOES move a packet across an interface: the flows
    /// it observed there, in that span. Nothing else about the delivery changes then — the
    /// records, their `TrafficType` filter and their delivery already read whatever this
    /// answers.
    ///
    /// A READ: it answers from what the store holds and writes nothing.
    #[must_use]
    pub fn observed_interface_flows(
        &self,
        network_interface_id: &str,
        from: SystemTime,
        to: SystemTime,
    ) -> Vec<ObservedFlow> {
        let _ = (network_interface_id, from, to);
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `TrafficType` selects on the `action` of the record: `ACCEPT` captures exactly the
    /// flows the filters admitted, `REJECT` exactly those they denied, and `ALL` — like a flow
    /// log carrying no `TrafficType` at all — captures both.
    #[test]
    fn the_traffic_type_filters_on_the_action() {
        // (case, the TrafficType in force, the action of the flow, whether it is captured)
        let cases: [(&str, Option<&str>, &str, bool); 8] = [
            ("ALL takes an accepted flow", Some(TRAFFIC_TYPE_ALL), FLOW_LOG_ACTION_ACCEPT, true),
            ("ALL takes a rejected flow", Some(TRAFFIC_TYPE_ALL), FLOW_LOG_ACTION_REJECT, true),
            (
                "ACCEPT takes an accepted flow",
                Some(TRAFFIC_TYPE_ACCEPT),
                FLOW_LOG_ACTION_ACCEPT,
                true,
            ),
            (
                "ACCEPT excludes a rejected flow",
                Some(TRAFFIC_TYPE_ACCEPT),
                FLOW_LOG_ACTION_REJECT,
                false,
            ),
            (
                "REJECT takes a rejected flow",
                Some(TRAFFIC_TYPE_REJECT),
                FLOW_LOG_ACTION_REJECT,
                true,
            ),
            (
                "REJECT excludes an accepted flow",
                Some(TRAFFIC_TYPE_REJECT),
                FLOW_LOG_ACTION_ACCEPT,
                false,
            ),
            ("no TrafficType takes an accepted flow", None, FLOW_LOG_ACTION_ACCEPT, true),
            ("no TrafficType takes a rejected flow", None, FLOW_LOG_ACTION_REJECT, true),
        ];
        for (case, traffic_type, action, captured) in cases {
            assert_eq!(
                traffic_type_captures(traffic_type, action),
                captured,
                "case {case:?}: expected the flow to be captured={captured}, got {}",
                traffic_type_captures(traffic_type, action)
            );
        }
    }

    /// The window a flow log captures over is never longer than the MAXIMUM the flow log
    /// states, and is this emulator's own span for every value at or above it.
    #[test]
    fn the_capture_window_is_never_longer_than_the_flow_log_states() {
        // (case, the MaxAggregationInterval of the flow log, the window it captures over)
        let cases: [(&str, Option<i32>, Duration); 5] = [
            ("the documented default", Some(600), FLOW_LOG_CAPTURE_WINDOW),
            ("the shorter documented interval", Some(60), FLOW_LOG_CAPTURE_WINDOW),
            ("an interval shorter than this emulator's span", Some(1), Duration::from_secs(1)),
            ("an interval of no seconds at all", Some(0), Duration::ZERO),
            ("a flow log reporting no interval", None, FLOW_LOG_CAPTURE_WINDOW),
        ];
        for (case, stated, expected) in cases {
            let view = FlowLog {
                max_aggregation_interval: stated,
                ..FlowLog::default()
            };
            let window = capture_window_of(&view);
            assert_eq!(
                window, expected,
                "case {case:?}: expected a window of {expected:?}, got {window:?}"
            );
        }
    }

    /// One interface's records travel a stream named after that interface and the traffic the
    /// flow log captures, so two flow logs capturing different traffic of one interface into
    /// one group keep their records apart.
    #[test]
    fn the_stream_names_the_interface_and_the_traffic_captured() {
        // (case, the interface, the TrafficType in force, the stream its records travel)
        let cases: [(&str, &str, Option<&str>, &str); 4] = [
            (
                "every flow of the interface",
                "eni-0123456789abcdef0",
                Some(TRAFFIC_TYPE_ALL),
                "eni-0123456789abcdef0-all",
            ),
            (
                "the accepted flows",
                "eni-0123456789abcdef0",
                Some(TRAFFIC_TYPE_ACCEPT),
                "eni-0123456789abcdef0-accept",
            ),
            (
                "the rejected flows of another interface",
                "eni-00000000000000001",
                Some(TRAFFIC_TYPE_REJECT),
                "eni-00000000000000001-reject",
            ),
            (
                "a flow log capturing no stated traffic type",
                "eni-0123456789abcdef0",
                None,
                "eni-0123456789abcdef0",
            ),
        ];
        for (case, network_interface_id, traffic_type, expected) in cases {
            let stream = flow_log_stream_name(network_interface_id, traffic_type);
            assert_eq!(
                stream, expected,
                "case {case:?}: expected the stream {expected:?}, got {stream:?}"
            );
        }
    }

    /// One row of the publishing table: the case, the flow log status, the destination type,
    /// the group it names, and whether such a flow log publishes into a group.
    type PublishingRow<'a> = (
        &'a str,
        Option<&'a str>,
        Option<&'a str>,
        Option<&'a str>,
        bool,
    );

    /// A flow log publishes into a log group only while it is ACTIVE, its destination type is
    /// CloudWatch Logs, and it names the group its records go to.
    #[test]
    fn only_an_active_cloud_watch_logs_flow_log_publishes_into_a_group() {
        let cases: [PublishingRow<'_>; 5] = [
            (
                "an active flow log naming its group",
                Some(FLOW_LOG_STATUS_ACTIVE),
                Some(LOG_DESTINATION_TYPE_CLOUD_WATCH_LOGS),
                Some("/aws/vpc/flow-logs"),
                true,
            ),
            (
                "an Amazon S3 flow log",
                Some(FLOW_LOG_STATUS_ACTIVE),
                Some(LOG_DESTINATION_TYPE_S3),
                None,
                false,
            ),
            (
                "a Firehose flow log",
                Some(FLOW_LOG_STATUS_ACTIVE),
                Some(LOG_DESTINATION_TYPE_KINESIS_DATA_FIREHOSE),
                None,
                false,
            ),
            (
                "a flow log naming no group",
                Some(FLOW_LOG_STATUS_ACTIVE),
                Some(LOG_DESTINATION_TYPE_CLOUD_WATCH_LOGS),
                None,
                false,
            ),
            (
                "a flow log in no status at all",
                None,
                Some(LOG_DESTINATION_TYPE_CLOUD_WATCH_LOGS),
                Some("/aws/vpc/flow-logs"),
                false,
            ),
        ];
        for (case, status, destination_type, log_group, publishes) in cases {
            let view = FlowLog {
                flow_log_status: status.map(str::to_string),
                log_destination_type: destination_type.map(str::to_string),
                log_group_name: log_group.map(str::to_string),
                ..FlowLog::default()
            };
            assert_eq!(
                publishes_into_log_group(&view),
                publishes,
                "case {case:?}: expected publishes={publishes}, got {}",
                publishes_into_log_group(&view)
            );
        }
    }
}
