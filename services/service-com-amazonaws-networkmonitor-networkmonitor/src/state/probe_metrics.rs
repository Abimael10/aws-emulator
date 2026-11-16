// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch series a PROBE publishes about the network path it measures.
//!
//! # Why a probe publishes at all
//!
//! A probe is not read, it is WATCHED: what a monitor exists for is the metrics its
//! probes send to Amazon CloudWatch. The model says so twice — `CreateMonitor` and
//! `CreateProbe` document that "Within a monitor you'll create one or more probes
//! that monitor network traffic between your source Amazon Web Services VPC subnets
//! and your destination IP addresses. Each probe then aggregates and sends metrics
//! to Amazon CloudWatch", and `CreateMonitorInput$aggregationPeriod` is "The time,
//! in seconds, that metrics are aggregated and sent to Amazon CloudWatch". So a
//! probe that is live and publishes NOTHING leaves the client with no answer at
//! all: the namespace it polls stays empty, which on AWS is what a monitor that
//! was never activated looks like, and an alarm over the series never has data to
//! grade. That is also the one observable effect `aggregationPeriod` has: it is the
//! spacing of the windows ([`aggregation_interval`]).
//!
//! # What one window reports
//!
//! A probe measures a path by sending packets down it and waiting for them to come
//! back. Nothing in this emulator sends such a packet or answers one: no probe
//! opens a socket, and no destination an address can name here replies to an ICMP
//! echo or a TCP handshake. So every packet of every window is lost, which is the
//! [`TOTAL_PACKET_LOSS_PERCENT`] every window of [`PACKET_LOSS_METRIC`] reports —
//! the loss a destination that does not answer really produces, rather than a
//! number nothing measured.
//!
//! That is also why this is the ONE metric published. A round-trip time exists only
//! for a packet that came back and a health indicator is derived from those
//! measurements, so for a path nothing answers on there is no sample of either to
//! publish: a series of zero-millisecond round trips would report measurements that
//! never happened.
//!
//! # Where the datapoints go
//!
//! Into the CloudWatch emulator's OWN metric store — the one `PutMetricData` writes
//! and `ListMetrics` / `GetMetricStatistics` / `GetMetricData` read — reached
//! through the shared engine, so one metric has one home and the publisher and the
//! reader cannot drift. They are appended to that store directly rather than sent
//! through `PutMetricData`, which reserves the `AWS/` namespaces for the services
//! that own them ("you should not specify a namespace that begins with `AWS/`",
//! `PutMetricData$Namespace`).
//!
//! A series is identified by its namespace, its metric name and its dimension set
//! together, and what one of these datapoints is ABOUT is one probe of one monitor
//! ([`series_of`]): without both dimensions the probes of an account would share a
//! single series and no reader could tell which path a datapoint describes. What a
//! series is NOT keyed by is the Region — the destination store holds one series per
//! namespace, metric name and dimension set for the whole engine — but a `probeId`
//! is unique across this store, so two Regions never address one probe's series.
//!
//! # The clock the series runs on
//!
//! Publishing is owed by the CLOCK, not by any call: [`drive_probe_metrics`] is
//! taken by the writes that can change which probes are live or how far apart their
//! windows are, and it publishes what is already due and starts the one bounded
//! in-process task that waits for the next window. Nothing here is on a read path,
//! and nothing a pass publishes depends on who called: a window is published once
//! its aggregation period has passed, whether or not anybody asked.

use ::std::collections::{BTreeMap, BTreeSet};
use ::std::sync::Arc;
use ::std::time::{Duration, SystemTime, UNIX_EPOCH};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use super::{
    ComAmazonawsNetworkmonitorNetworkmonitorState as NetworkMonitorState, PROBE_STATE_ACTIVE,
    PROBE_STATE_PENDING,
};

/// The namespace CloudWatch Network Monitor publishes a probe's measurements in.
pub const METRIC_NAMESPACE: &str = "AWS/NetworkMonitor";

/// The dimension naming the monitor a datapoint belongs to, after the member the
/// model identifies a monitor by (`GetMonitorOutput$monitorName`).
pub const MONITOR_DIMENSION: &str = "monitorName";

/// The dimension naming the probe a datapoint was measured by, after the member the
/// model identifies a probe by (`Probe$probeId`).
pub const PROBE_DIMENSION: &str = "probeId";

/// The metric one window of a probe's measurements is published as: the share of the
/// packets the probe sent that never came back.
pub const PACKET_LOSS_METRIC: &str = "PacketLoss";

/// The `StandardUnit` a share of packets carries
/// (`smithy_query(query="shape",shape="StandardUnit")` holds `Percent`).
pub const PACKET_LOSS_UNIT: &str = "Percent";

/// What one window of a path NOTHING answers on comes to: every packet the probe
/// sent is lost, so the loss is the whole of what it sent.
///
/// Nothing in this emulator answers a probe's packets (see the module docs), so this
/// is the measurement every window of every probe reports — the series EXISTS and
/// reads total loss, which is what a destination that never replies produces.
pub const TOTAL_PACKET_LOSS_PERCENT: f64 = 100.0;

/// The `StorageResolution` these datapoints are stored at, in seconds: the regular
/// resolution a metric has unless its publisher asks for the high-resolution one
/// ("Setting this to 60 specifies this metric as regular resolution, which
/// CloudWatch stores at 1-minute resolution", `MetricDatum$StorageResolution`).
const STORAGE_RESOLUTION_SECS: i32 = 60;

/// The shortest a publisher-clock wake-up waits, so a window already past its period
/// still costs a yield rather than a CPU-bound spin.
const MIN_PUBLISH_WAIT: Duration = Duration::from_millis(50);

/// The longest one waits, so the clock re-reads the store — which a write may have
/// changed since — at least this often.
const MAX_PUBLISH_WAIT: Duration = Duration::from_secs(5);

/// How many times one publisher clock wakes up before it stands down.
///
/// A backstop, not a schedule: it bounds the work one request can start, and the
/// next write to a monitor or a probe starts a clock again if none is running.
const MAX_CLOCK_WAKE_UPS: usize = 2_000;

/// How many windows of ONE probe a single pass publishes.
///
/// A bound on the work one wake-up does: the clock runs while a probe publishes, so
/// it is never more than one window behind — but a store whose clock stood down and
/// was started again later hands the rest of the gap to the next pass rather than
/// publishing an unbounded backlog while a wake-up is being served.
const MAX_WINDOWS_PER_PASS: u32 = 8;

/// How far back a pass REACHES for the windows it still owes.
///
/// A series the clock was not running for has a gap in it, and the gap is this
/// emulator's (nothing drove the clock), not the probe's. A pass therefore resumes
/// at most this many windows back and leaves the older ones unpublished rather than
/// back-filling an arbitrary span of a series nobody was watching.
const MAX_BACKFILL_WINDOWS: u32 = 16;

/// Which probe's series a stored stamp belongs to: the Region and monitor that hold
/// the probe, and the probe's own id — the same identity the store itself keys a
/// probe by, so a stamp can never be read for another probe.
type ProbeSeries = (String, String, String);

/// Where every probe series of this store has got to, and the one in-process clock
/// that advances them.
///
/// Held on the store, so the schedule is the store's own: a series runs while its
/// probe is live, and the clock is claimed once however many writes drive it.
#[derive(Debug, Default)]
pub struct ProbeMetricPublisher {
    /// The stamp of the last window published for each live probe — which is also
    /// where its series starts: a probe seen live for the first time publishes the
    /// window of that instant. A probe that is no longer live has no entry, so one
    /// switched back on opens a series again from the instant it returned.
    published_through: BTreeMap<ProbeSeries, SystemTime>,
    /// Whether a clock is running for this store.
    clock_running: bool,
    /// Which claim the running clock holds, so a clock that has stood down cannot
    /// release a claim a later one took.
    clock_generation: u64,
    /// The running clock's task, held so the task a request started stays stoppable.
    clock: Option<::tokio::task::JoinHandle<()>>,
}

/// How far apart the windows of one monitor's probes are stamped: its
/// `aggregationPeriod`, "the time, in seconds, that metrics are aggregated and sent
/// to Amazon CloudWatch" (`CreateMonitorInput$aggregationPeriod`).
///
/// The stored period is one of the two values that member documents
/// ([`super::AGGREGATION_PERIODS`]); a value no create could store still yields a
/// non-zero spacing, so a series can never be published in a tight loop.
#[must_use]
pub fn aggregation_interval(aggregation_period: i64) -> Duration {
    let seconds = u64::try_from(aggregation_period).unwrap_or(0).max(1);
    Duration::from_secs(seconds)
}

/// The series one probe's measurements live in: the [`METRIC_NAMESPACE`] namespace,
/// the metric's name, and the monitor and probe the measurements are about.
///
/// The ONE place such a series is addressed, so the publisher and anything looking
/// for what it published name the same series.
#[must_use]
pub fn series_of(monitor_name: &str, probe_id: &str, metric_name: &str) -> cloudwatch::MetricKey {
    cloudwatch::MetricKey {
        namespace: METRIC_NAMESPACE.to_string(),
        metric_name: metric_name.to_string(),
        dimensions: [
            (MONITOR_DIMENSION.to_string(), monitor_name.to_string()),
            (PROBE_DIMENSION.to_string(), probe_id.to_string()),
        ]
        .into_iter()
        .collect(),
    }
}

/// Publish every probe window the clock has already made due, and start the one
/// bounded in-process task that waits for the next one.
///
/// Called by the writes that can change which probes are live, or how far apart
/// their windows are — never by a read: what this materialises is what the stored
/// stamps have already made due, so what a caller sees does not depend on whether
/// anybody called.
///
/// With no async runtime to spawn onto there is no clock, and the windows due at the
/// moment of the write are then the whole series until the next write — the router
/// always runs on one.
pub fn drive_probe_metrics<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) {
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
        // `true` only if the wake-up cap is what ended the loop; a clock that ran
        // out of work has already released its claim inside the one critical
        // section that observed it.
        let mut capped = true;
        for _ in 0..MAX_CLOCK_WAKE_UPS {
            let Some(wait) = next_wait_or_release(&mut state.lock(), generation) else {
                capped = false;
                break;
            };
            // Every wake-up waits, even for a window already past: that is what
            // keeps a due window from costing a CPU-bound spin, and it hands the
            // runtime back between wake-ups.
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

/// Keep the running clock's task handle, aborting the one it replaces — and only
/// while `generation` is still the CURRENT claim, so a starter whose own task has
/// already stood down installs nothing and aborts nothing.
fn set_clock(
    store: &mut NetworkMonitorState,
    generation: u64,
    handle: ::tokio::task::JoinHandle<()>,
) {
    if store.probe_metrics.clock_generation != generation {
        handle.abort();
        return;
    }
    if let Some(previous) = store.probe_metrics.clock.replace(handle) {
        previous.abort();
    }
}

/// Claim the single publisher clock for this store: the claim's GENERATION when the
/// caller may start one, `None` when one already runs.
fn claim_clock(store: &mut NetworkMonitorState) -> Option<u64> {
    if store.probe_metrics.clock_running {
        return None;
    }
    store.probe_metrics.clock_running = true;
    store.probe_metrics.clock_generation = store.probe_metrics.clock_generation.wrapping_add(1);
    Some(store.probe_metrics.clock_generation)
}

/// Release the claim `generation` took — and ONLY that one, so a clock that ended on
/// the wake-up cap cannot clear a claim a clock started since is holding.
fn release_clock(store: &mut NetworkMonitorState, generation: u64) {
    if store.probe_metrics.clock_generation == generation {
        store.probe_metrics.clock_running = false;
    }
}

/// How long the clock waits before its next pass, or `None` — having RELEASED its
/// claim — when this store holds no probe that is publishing or about to.
///
/// Observing "nothing is owed" and standing down happen in ONE critical section,
/// which is what makes the hand-over race-free: a write holds this same lock to
/// store the probe BEFORE it tries to claim the clock, so either this call sees that
/// probe and the running clock keeps it, or the write's claim finds the flag cleared
/// and starts a clock of its own.
fn next_wait_or_release(store: &mut NetworkMonitorState, generation: u64) -> Option<Duration> {
    let now = SystemTime::now();
    let mut soonest: Option<Duration> = None;
    for (region, monitors) in &store.monitors {
        for (monitor_name, monitor) in monitors {
            let interval = aggregation_interval(monitor.aggregation_period);
            for (probe_id, probe) in &monitor.probes {
                let wait = match probe.state.as_str() {
                    PROBE_STATE_ACTIVE => {
                        let series: ProbeSeries = (
                            region.clone(),
                            monitor_name.clone(),
                            probe_id.clone(),
                        );
                        match store.probe_metrics.published_through.get(&series) {
                            // The window after the last one published.
                            Some(last) => (*last + interval)
                                .duration_since(now)
                                .unwrap_or(Duration::ZERO),
                            // A probe with no series yet opens one on the next pass.
                            None => Duration::ZERO,
                        }
                    }
                    // A probe still being provisioned measures nothing yet, and the
                    // clock looks again when the activation it is owed falls due.
                    PROBE_STATE_PENDING => probe
                        .activates_at
                        .duration_since(now)
                        .unwrap_or(Duration::ZERO),
                    // Every other state is one in which the probe sends no packets:
                    // a caller switched it off, or its teardown is carrying it out
                    // of the monitor. Nothing is owed for it.
                    _ => continue,
                };
                soonest = Some(soonest.map_or(wait, |held: Duration| held.min(wait)));
            }
        }
    }
    match soonest {
        Some(wait) => Some(wait.clamp(MIN_PUBLISH_WAIT, MAX_PUBLISH_WAIT)),
        None => {
            release_clock(store, generation);
            None
        }
    }
}

/// Publish the window of every probe whose aggregation period has passed.
///
/// Two steps, so no store guard is held over another service's lock and the two
/// stores are never locked at once: under THIS store's guard, materialise the
/// lifecycle transitions the clock owes its records and take the datapoints the due
/// windows are published as; then, with the metric store's guard, append them and
/// make the alarms over them due.
fn publish_due_windows<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let now = SystemTime::now();
    let published = {
        let mut store = state.lock();
        // A probe whose activation has already fallen due IS live, whether or not
        // anybody has called since — materialising it is this store's own owed
        // write, and a probe publishes from the instant it reaches that state.
        settle_every_monitor(&mut store, now);
        due_datapoints(&mut store, now)
    };
    append_datapoints(engine, published, now);
}

/// Materialise the transitions the clock has already made due, for every monitor of
/// every Region this store holds.
///
/// The same walk a read of one monitor performs
/// ([`NetworkMonitorState::settle_monitor`]), over the whole store: the publisher is
/// not serving a request, so it has no Region of its own to settle.
fn settle_every_monitor(store: &mut NetworkMonitorState, now: SystemTime) {
    let regions: Vec<String> = store.monitors.keys().cloned().collect();
    for region in regions {
        store.settle_monitors_in(&region, now);
    }
}

/// Take the datapoints every probe window that is due at `now` is published as, and
/// advance each of those series past the windows it published.
fn due_datapoints(
    store: &mut NetworkMonitorState,
    now: SystemTime,
) -> Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> {
    // The probes that are LIVE, each with the monitor it publishes for and the
    // spacing of that monitor's windows. Taken first, so advancing the stamps below
    // does not read the records while they are being written.
    let mut publishing: Vec<(ProbeSeries, Duration)> = Vec::new();
    for (region, monitors) in &store.monitors {
        for (monitor_name, monitor) in monitors {
            let interval = aggregation_interval(monitor.aggregation_period);
            for (probe_id, probe) in &monitor.probes {
                if probe.state == PROBE_STATE_ACTIVE {
                    publishing.push((
                        (region.clone(), monitor_name.clone(), probe_id.clone()),
                        interval,
                    ));
                }
            }
        }
    }
    // A probe that is no longer live publishes nothing more, and one that becomes
    // live again opens a series of its own from the instant it returned.
    let live: BTreeSet<&ProbeSeries> = publishing.iter().map(|(series, _)| series).collect();
    let stale: Vec<ProbeSeries> = store
        .probe_metrics
        .published_through
        .keys()
        .filter(|series| !live.contains(series))
        .cloned()
        .collect();
    for series in stale {
        store.probe_metrics.published_through.remove(&series);
    }

    let mut datapoints: Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> = Vec::new();
    for (series, interval) in publishing {
        for stamp in due_window_stamps(&mut store.probe_metrics, &series, interval, now) {
            let (_, monitor_name, probe_id) = &series;
            datapoints.push((
                series_of(monitor_name, probe_id, PACKET_LOSS_METRIC),
                cloudwatch::StoredDatum {
                    timestamp_secs: epoch_secs(stamp),
                    // One observation: this window's own measurement.
                    sample_count: 1.0,
                    sum: TOTAL_PACKET_LOSS_PERCENT,
                    minimum: TOTAL_PACKET_LOSS_PERCENT,
                    maximum: TOTAL_PACKET_LOSS_PERCENT,
                    unit: Some(PACKET_LOSS_UNIT.to_string()),
                    storage_resolution: STORAGE_RESOLUTION_SECS,
                },
            ));
        }
    }
    datapoints
}

/// The stamps of the windows `series` still owes at `now`, advancing it past them.
///
/// A probe with no series yet opens one with the window of this instant — the first
/// period anything could have been measured over — and one that has a series owes
/// the windows whose period has passed since its last, at most
/// [`MAX_WINDOWS_PER_PASS`] of them and reaching no further back than
/// [`MAX_BACKFILL_WINDOWS`].
fn due_window_stamps(
    publisher: &mut ProbeMetricPublisher,
    series: &ProbeSeries,
    interval: Duration,
    now: SystemTime,
) -> Vec<SystemTime> {
    let backfill = interval.saturating_mul(MAX_BACKFILL_WINDOWS);
    let Some(published_through) = publisher.published_through.get_mut(series) else {
        publisher.published_through.insert(series.clone(), now);
        return vec![now];
    };
    // A gap no clock was running for is this emulator's, not the probe's: the series
    // resumes inside the backfill window rather than filling in every period of it.
    if let Some(earliest) = now.checked_sub(backfill) {
        if *published_through < earliest {
            *published_through = earliest;
        }
    }
    let mut stamps: Vec<SystemTime> = Vec::new();
    while stamps.len() < MAX_WINDOWS_PER_PASS as usize {
        let next = *published_through + interval;
        if next > now {
            break;
        }
        *published_through = next;
        stamps.push(next);
    }
    stamps
}

/// Append the datapoints this pass published to the metric store, and make every
/// alarm over the series they belong to due for evaluation.
///
/// Nothing is published on an engine that holds no metric store to reach: a
/// datapoint has one home, and this path never makes a second one. An alarm is
/// evaluated FROM these datapoints, so storing a series it watches is what makes its
/// evaluation due — an alarm cannot tell a service's own metric from one a client
/// published.
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
            // The metric store holds no more series. The probe itself is already
            // stored and still measuring, so nothing is failed for it; the dropped
            // datapoint is reported where a developer can see it.
            ::tracing::warn!(
                metric = watched.metric_name.as_str(),
                "the CloudWatch metric store is at capacity, so this probe window was dropped"
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

/// `at` as the epoch seconds a stored datapoint carries, which is the unit the
/// metric store's own readers window by. An instant before the epoch (which no clock
/// reports) reads as the epoch itself rather than failing the publish.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}
