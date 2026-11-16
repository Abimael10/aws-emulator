// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series a NAT GATEWAY publishes about the traffic it carries.
//!
//! # Why a gateway publishes at all
//!
//! A NAT gateway is watched rather than read: the way a client learns what went through one
//! is the `AWS/NATGateway` namespace Amazon VPC publishes for it — "Amazon VPC publishes data
//! points to Amazon CloudWatch for your NAT gateways … The metrics are published to
//! CloudWatch every minute" (Amazon VPC User Guide, "Monitor NAT gateways with Amazon
//! CloudWatch") — dimensioned on the gateway the data point is about
//! ([`NAT_GATEWAY_DIMENSION`]). A gateway that publishes NOTHING leaves that client with no
//! answer at all: a read of its series is served, it just never has a data point, so a caller
//! polling for one cannot tell "no traffic yet" from "this emulator publishes none", an alarm
//! over the series never has data to grade, and a quiet gateway is indistinguishable from an
//! absent one.
//!
//! So a gateway publishes for as long as it is available, and each data point reads what the
//! gateway carried in the minute it stamps — which is the 0 of a gateway nothing sends through
//! ([`CarriedTraffic::carried_by`]). A quiet gateway's series EXISTS and reads 0, the same
//! contract the other services of this workspace publish their own counters under.
//!
//! # Where the data points go
//!
//! Into the CloudWatch emulator's OWN metric store — the one `PutMetricData` writes and
//! `ListMetrics` / `GetMetricStatistics` / `GetMetricData` read — reached through the shared
//! engine, so one metric has one home. They are appended to that store directly rather than
//! sent through `PutMetricData`, which reserves the `AWS/` namespaces for the services that
//! own them ("you should not specify a namespace that begins with `AWS/`",
//! `PutMetricData$Namespace`).
//!
//! # The clock the series runs on
//!
//! Publishing is owed by the CLOCK, not by any call: [`drive_nat_gateway_metrics`] is taken by
//! the writes that create and delete a gateway, and it publishes what is already due and
//! starts the one bounded in-process task that waits for the next window. Nothing here is on a
//! read path, and nothing a pass publishes depends on who called: a window is published when
//! its minute has passed, whether or not anybody asked.

use ::std::collections::{BTreeMap, BTreeSet};
use ::std::sync::Arc;
use ::std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use super::{
    ComAmazonawsEc2Amazonec2State as Ec2State, NAT_GATEWAY_STATE_AVAILABLE,
    NAT_GATEWAY_STATE_PENDING,
};

/// The namespace Amazon VPC publishes a NAT gateway's metrics in.
pub const METRIC_NAMESPACE: &str = "AWS/NATGateway";

/// The one dimension those metrics are reported per: the gateway they are about.
pub const NAT_GATEWAY_DIMENSION: &str = "NatGatewayId";

/// The `StandardUnit` a byte count carries (`smithy_query(query="shape",shape="StandardUnit")`
/// holds `Bytes`).
pub const BYTES_UNIT: &str = "Bytes";

/// The `StandardUnit` a packet, connection or error count carries: it is a number of things.
pub const COUNT_UNIT: &str = "Count";

/// How far apart the data points of one series are stamped: the minute Amazon VPC publishes a
/// NAT gateway's metrics per.
pub const PUBLISH_INTERVAL: Duration = Duration::from_secs(60);

/// The `StorageResolution` these data points are stored at, in seconds: the regular resolution
/// a metric has unless its publisher asks for the high-resolution one ("Setting this to 60
/// specifies this metric as regular resolution, which CloudWatch stores at 1-minute
/// resolution", `MetricDatum$StorageResolution`).
const STORAGE_RESOLUTION_SECS: i32 = 60;

/// The shortest a publisher-clock wake-up waits, so a window already past its minute still
/// costs a yield rather than a CPU-bound spin.
const MIN_PUBLISH_WAIT: Duration = Duration::from_millis(50);

/// The longest one waits, so the clock re-reads the store — which a write may have changed
/// since — at least this often.
const MAX_PUBLISH_WAIT: Duration = Duration::from_secs(5);

/// How many times one publisher clock wakes up before it stands down.
///
/// A backstop, not a schedule: it bounds the work one request can start, and the next write to
/// a gateway starts a clock again if none is running.
const MAX_CLOCK_WAKE_UPS: usize = 2_000;

/// How many windows of ONE gateway a single pass publishes.
///
/// A bound on the work one wake-up does: the clock runs while a gateway publishes, so it is
/// never more than one window behind — but a store whose clock stood down and was started
/// again later hands the rest of the gap to the next pass rather than publishing an unbounded
/// backlog while a wake-up is being served.
const MAX_WINDOWS_PER_PASS: u32 = 8;

/// How far back a pass REACHES for the windows it still owes.
///
/// A series the clock was not running for has a gap in it, and the gap is this emulator's
/// (nothing drove the clock), not the gateway's. A pass therefore resumes at most this far
/// back and leaves the older windows unpublished rather than back-filling an arbitrary span of
/// a series nobody was watching.
const MAX_BACKFILL_WINDOWS: u32 = 16;

/// Everything ONE minute of a NAT gateway's traffic is counted as, in the units the gateway's
/// own metrics are published in.
///
/// One struct rather than a value per metric, because they are one observation of one window:
/// the catalog below reads each metric's value off it ([`NatGatewayMetric::of`]), so a metric
/// and the traffic it reports cannot drift apart.
#[derive(Debug, Clone, Copy, Default)]
pub struct CarriedTraffic {
    /// Bytes the gateway received from clients in the VPC.
    pub bytes_in_from_source: f64,
    /// Bytes it received from the destinations those clients reached.
    pub bytes_in_from_destination: f64,
    /// Bytes it sent on to those destinations.
    pub bytes_out_to_destination: f64,
    /// Bytes it sent back to the clients.
    pub bytes_out_to_source: f64,
    /// Packets in each of those four directions.
    pub packets_in_from_source: f64,
    /// See [`Self::packets_in_from_source`].
    pub packets_in_from_destination: f64,
    /// See [`Self::packets_in_from_source`].
    pub packets_out_to_destination: f64,
    /// See [`Self::packets_in_from_source`].
    pub packets_out_to_source: f64,
    /// Concurrent connections through the gateway.
    pub active_connections: f64,
    /// Connections attempted through it.
    pub connection_attempts: f64,
    /// Connections established through it.
    pub connections_established: f64,
    /// Packets it dropped.
    pub packets_dropped: f64,
    /// Connections it closed for idling.
    pub idle_timeouts: f64,
    /// Times it could not allocate a source port.
    pub port_allocation_errors: f64,
}

impl CarriedTraffic {
    /// What the gateway carried in one window: the traffic this emulator really moved through
    /// it.
    ///
    /// A packet only crosses a NAT gateway when something in a subnet routed to it SENDS one,
    /// and nothing in this emulator does: a guest's only network program answers out of the
    /// instance metadata this process serves it ([`super::guest_metadata`], whose address is
    /// link-local and crosses no gateway), and nothing else here opens a socket at all. So no
    /// byte, packet or connection is ever accounted to a gateway, and every count of every
    /// window is the 0 a gateway nothing sends through reports — which is what this answers,
    /// rather than a number nothing produced.
    #[must_use]
    pub fn carried_by() -> Self {
        Self::default()
    }
}

/// One metric a NAT gateway publishes: its name, the unit it carries, and which count of a
/// window it reports.
#[derive(Debug, Clone, Copy)]
pub struct NatGatewayMetric {
    /// The `MetricName` the series is published under.
    pub name: &'static str,
    /// The `StandardUnit` its data points carry.
    pub unit: &'static str,
    /// The count of the window this metric is.
    pub of: fn(&CarriedTraffic) -> f64,
}

/// Every metric a NAT gateway publishes, from the catalog Amazon VPC documents for the
/// `AWS/NATGateway` namespace (Amazon VPC User Guide, "Monitor NAT gateways with Amazon
/// CloudWatch" — the "NAT gateway metrics" table), each with the unit that table gives it.
///
/// The whole catalog is published for every window, so a reader finds the series it windows
/// whichever of them it asks for.
pub const NAT_GATEWAY_METRICS: &[NatGatewayMetric] = &[
    NatGatewayMetric {
        name: "ActiveConnectionCount",
        unit: COUNT_UNIT,
        of: |carried| carried.active_connections,
    },
    NatGatewayMetric {
        name: "BytesInFromDestination",
        unit: BYTES_UNIT,
        of: |carried| carried.bytes_in_from_destination,
    },
    NatGatewayMetric {
        name: "BytesInFromSource",
        unit: BYTES_UNIT,
        of: |carried| carried.bytes_in_from_source,
    },
    NatGatewayMetric {
        name: "BytesOutToDestination",
        unit: BYTES_UNIT,
        of: |carried| carried.bytes_out_to_destination,
    },
    NatGatewayMetric {
        name: "BytesOutToSource",
        unit: BYTES_UNIT,
        of: |carried| carried.bytes_out_to_source,
    },
    NatGatewayMetric {
        name: "ConnectionAttemptCount",
        unit: COUNT_UNIT,
        of: |carried| carried.connection_attempts,
    },
    NatGatewayMetric {
        name: "ConnectionEstablishedCount",
        unit: COUNT_UNIT,
        of: |carried| carried.connections_established,
    },
    NatGatewayMetric {
        name: "ErrorPortAllocation",
        unit: COUNT_UNIT,
        of: |carried| carried.port_allocation_errors,
    },
    NatGatewayMetric {
        name: "IdleTimeoutCount",
        unit: COUNT_UNIT,
        of: |carried| carried.idle_timeouts,
    },
    NatGatewayMetric {
        name: "PacketsDropCount",
        unit: COUNT_UNIT,
        of: |carried| carried.packets_dropped,
    },
    NatGatewayMetric {
        name: "PacketsInFromDestination",
        unit: COUNT_UNIT,
        of: |carried| carried.packets_in_from_destination,
    },
    NatGatewayMetric {
        name: "PacketsInFromSource",
        unit: COUNT_UNIT,
        of: |carried| carried.packets_in_from_source,
    },
    NatGatewayMetric {
        name: "PacketsOutToDestination",
        unit: COUNT_UNIT,
        of: |carried| carried.packets_out_to_destination,
    },
    NatGatewayMetric {
        name: "PacketsOutToSource",
        unit: COUNT_UNIT,
        of: |carried| carried.packets_out_to_source,
    },
];

/// Where every NAT gateway series of this store has got to, and the one in-process clock that
/// advances them.
///
/// Held on the store, so the schedule is the store's own: a series runs while its gateway is
/// available, and the clock is claimed once however many writes drive it.
#[derive(Debug, Default)]
pub struct NatGatewayMetricPublisher {
    /// The stamp of the last window published for each gateway, by `NatGatewayId` — which is
    /// also where the series starts: a gateway seen available for the first time publishes the
    /// window of that instant. A gateway that is no longer available has no entry.
    published_through: BTreeMap<String, SystemTime>,
    /// Whether a clock is running for this store.
    clock_running: bool,
    /// Which claim the running clock holds, so a clock that has stood down cannot release a
    /// claim a later one took.
    clock_generation: u64,
    /// The running clock's task, held so the task a request started stays stoppable.
    clock: Option<::tokio::task::JoinHandle<()>>,
}

/// Publish every NAT gateway window the clock has already made due, and start the one bounded
/// in-process task that waits for the next one.
///
/// Called by the writes that create and delete a gateway — never by a read: what this
/// materialises is what the stored stamps have already made due, so what a caller sees does
/// not depend on whether anybody called.
///
/// With no async runtime to spawn onto there is no clock, and the windows due at the moment of
/// the write are then the whole series until the next write — the router always runs on one.
pub fn drive_nat_gateway_metrics<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) {
    publish_due_windows(&**engine);
    let Some(state) = super::service_state(&**engine) else {
        return;
    };
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(generation) = claim_clock(&mut state.lock()) else {
        return;
    };
    let engine = Arc::clone(engine);
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
            publish_due_windows(&*engine);
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
    if store.nat_gateway_metrics.clock_generation != generation {
        handle.abort();
        return;
    }
    if let Some(previous) = store.nat_gateway_metrics.clock.replace(handle) {
        previous.abort();
    }
}

/// Claim the single publisher clock for this store: the claim's GENERATION when the caller may
/// start one, `None` when one already runs.
fn claim_clock(store: &mut Ec2State) -> Option<u64> {
    if store.nat_gateway_metrics.clock_running {
        return None;
    }
    store.nat_gateway_metrics.clock_running = true;
    store.nat_gateway_metrics.clock_generation =
        store.nat_gateway_metrics.clock_generation.wrapping_add(1);
    Some(store.nat_gateway_metrics.clock_generation)
}

/// Release the claim `generation` took — and ONLY that one, so a clock that ended on the
/// wake-up cap cannot clear a claim a clock started since is holding.
fn release_clock(store: &mut Ec2State, generation: u64) {
    if store.nat_gateway_metrics.clock_generation == generation {
        store.nat_gateway_metrics.clock_running = false;
    }
}

/// How long the clock waits before its next pass, or `None` — having RELEASED its claim — when
/// this store holds no gateway that is publishing or about to.
///
/// Observing "nothing is owed" and standing down happen in ONE critical section, which is what
/// makes the hand-over race-free: a write holds this same lock to store the gateway BEFORE it
/// tries to claim the clock, so either this call sees that gateway and the running clock keeps
/// it, or the write's claim finds the flag cleared and starts a clock of its own.
fn next_wait_or_release(store: &mut Ec2State, generation: u64) -> Option<Duration> {
    let now = SystemTime::now();
    let instant = Instant::now();
    let mut soonest: Option<Duration> = None;
    for (nat_gateway_id, gateway) in &store.nat_gateways {
        let wait = match gateway.state.as_deref() {
            Some(NAT_GATEWAY_STATE_AVAILABLE) => {
                let published_through = store
                    .nat_gateway_metrics
                    .published_through
                    .get(nat_gateway_id);
                match published_through {
                    // The window after the last one published.
                    Some(last) => (*last + PUBLISH_INTERVAL)
                        .duration_since(now)
                        .unwrap_or(Duration::ZERO),
                    // A gateway with no series yet opens one on the next pass.
                    None => Duration::ZERO,
                }
            }
            // A gateway that is still coming up publishes nothing yet, and the clock looks
            // again when the state move it is owed falls due.
            Some(NAT_GATEWAY_STATE_PENDING) => store
                .nat_gateway_moves
                .get(nat_gateway_id)
                .map_or(Duration::ZERO, |pending| {
                    pending.due_at.saturating_duration_since(instant)
                }),
            // Every other state is terminal: a deleting or deleted gateway carries no
            // traffic, so nothing is owed for it.
            _ => continue,
        };
        soonest = Some(soonest.map_or(wait, |held: Duration| held.min(wait)));
    }
    match soonest {
        Some(wait) => Some(wait.clamp(MIN_PUBLISH_WAIT, MAX_PUBLISH_WAIT)),
        None => {
            release_clock(store, generation);
            None
        }
    }
}

/// Publish the window of every gateway whose minute has passed.
///
/// Three steps, so no store guard is held over another service's lock and the two stores are
/// never locked at once: under THIS store's guard, materialise the state moves the clock owes
/// a gateway and take the data points the due windows are published as; then, with the metric
/// store's guard, append them and make the alarms over them due.
fn publish_due_windows<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let now = SystemTime::now();
    let published = {
        let mut store = state.lock();
        // A gateway whose move to `available` has already fallen due IS available, whether or
        // not anybody has called since — materialising it is this store's own owed write, and
        // a gateway publishes from the instant it reaches that state.
        store.materialize_due_transitions();
        due_datapoints(&mut store, now)
    };
    append_datapoints(engine, published, now);
}

/// Take the data points every NAT gateway window that is due at `now` is published as, and
/// advance each of those series past the windows it published.
fn due_datapoints(
    store: &mut Ec2State,
    now: SystemTime,
) -> Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> {
    let publishing: BTreeSet<String> = store
        .nat_gateways
        .iter()
        .filter(|(_, gateway)| gateway.state.as_deref() == Some(NAT_GATEWAY_STATE_AVAILABLE))
        .map(|(nat_gateway_id, _)| nat_gateway_id.clone())
        .collect();
    // A gateway that is no longer available publishes nothing more, and a later one of the
    // same id would open a series of its own.
    store
        .nat_gateway_metrics
        .published_through
        .retain(|nat_gateway_id, _| publishing.contains(nat_gateway_id));
    let mut datapoints: Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> = Vec::new();
    for nat_gateway_id in publishing {
        let stamps = due_window_stamps(store, &nat_gateway_id, now);
        for stamp in stamps {
            let carried = CarriedTraffic::carried_by();
            for metric in NAT_GATEWAY_METRICS {
                let value = (metric.of)(&carried);
                datapoints.push((
                    cloudwatch::MetricKey {
                        namespace: METRIC_NAMESPACE.to_string(),
                        metric_name: metric.name.to_string(),
                        dimensions: [(
                            NAT_GATEWAY_DIMENSION.to_string(),
                            nat_gateway_id.clone(),
                        )]
                        .into_iter()
                        .collect(),
                    },
                    cloudwatch::StoredDatum {
                        timestamp_secs: epoch_secs(stamp),
                        // One observation: this window's own count.
                        sample_count: 1.0,
                        sum: value,
                        minimum: value,
                        maximum: value,
                        unit: Some(metric.unit.to_string()),
                        storage_resolution: STORAGE_RESOLUTION_SECS,
                    },
                ));
            }
        }
    }
    datapoints
}

/// The stamps of the windows `nat_gateway_id` still owes at `now`, advancing its series past
/// them.
///
/// A gateway with no series yet opens one with the window of this instant — the first minute
/// anything could have gone through it — and one that has a series owes the windows whose
/// minute has passed since its last, at most [`MAX_WINDOWS_PER_PASS`] of them and reaching no
/// further back than [`MAX_BACKFILL_WINDOWS`].
fn due_window_stamps(
    store: &mut Ec2State,
    nat_gateway_id: &str,
    now: SystemTime,
) -> Vec<SystemTime> {
    let backfill = PUBLISH_INTERVAL.saturating_mul(MAX_BACKFILL_WINDOWS);
    let Some(published_through) = store
        .nat_gateway_metrics
        .published_through
        .get_mut(nat_gateway_id)
    else {
        store
            .nat_gateway_metrics
            .published_through
            .insert(nat_gateway_id.to_string(), now);
        return vec![now];
    };
    // A gap no clock was running for is this emulator's, not the gateway's: the series resumes
    // inside the backfill window rather than filling in every minute of it.
    if let Some(earliest) = now.checked_sub(backfill) {
        if *published_through < earliest {
            *published_through = earliest;
        }
    }
    let mut stamps: Vec<SystemTime> = Vec::new();
    while stamps.len() < MAX_WINDOWS_PER_PASS as usize {
        let next = *published_through + PUBLISH_INTERVAL;
        if next > now {
            break;
        }
        *published_through = next;
        stamps.push(next);
    }
    stamps
}

/// Append the data points this pass published to the metric store, and make every alarm over
/// the series they belong to due for evaluation.
///
/// Nothing is published on an engine that holds no metric store to reach: a data point has one
/// home, and this path never makes a second one. An alarm is evaluated FROM these data points,
/// so storing a series it watches is what makes its evaluation due — an alarm cannot tell a
/// service's own metric from one a client published.
fn append_datapoints<Eng: Send + Sync + 'static>(
    engine: &Eng,
    published: Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)>,
    now: SystemTime,
) {
    if published.is_empty() {
        return;
    }
    let Some(state) = cloudwatch::service_state(engine) else {
        return;
    };
    let mut store = state.lock();
    let mut written: Vec<cloudwatch::WrittenSeries> = Vec::with_capacity(published.len());
    for (key, datum) in published {
        let watched = key.clone();
        let unit = datum.unit().to_string();
        if store.append_datapoints(key, vec![datum]).is_err() {
            // The metric store holds no more series. The gateway itself is already stored, so
            // nothing is failed for it; the dropped data point is reported where a developer
            // can see it.
            ::tracing::warn!(
                metric = watched.metric_name.as_str(),
                "the CloudWatch metric store is at capacity, so this NAT gateway window was \
                 dropped"
            );
            continue;
        }
        written.push(cloudwatch::WrittenSeries {
            key: watched,
            units: vec![unit],
        });
    }
    store.make_alarms_due(&written, epoch_secs(now));
}

/// `at` as the epoch seconds a stored data point carries, which is the unit the metric store's
/// own readers window by. An instant before the epoch (which no clock reports) reads as the
/// epoch itself rather than failing the publish.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}
