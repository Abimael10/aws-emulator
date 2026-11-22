// SPDX-License-Identifier: CC0-1.0

//! The CAPACITY an Aurora Serverless v2 DB cluster is running on: the Aurora capacity units
//! (ACUs) it holds, the AUTO-PAUSE the clock owes one that has been idle, and the CloudWatch
//! series both are reported as.
//!
//! An Aurora Serverless v2 cluster has no instance class to read its compute off: its capacity
//! is a number that moves while the cluster runs, between the `MinCapacity` and `MaxCapacity`
//! its `ServerlessV2ScalingConfiguration` names. No API operation of this service reports that
//! number, so the ONE place a client can watch it is the [`SERVERLESS_DATABASE_CAPACITY_METRIC`]
//! metric of the [`METRIC_NAMESPACE`] namespace: a dashboard, an alarm or a client that waits
//! for a cluster to scale down reads that series, and against a cluster that publishes nothing
//! it cannot tell a running cluster from a paused one — or from one that does not exist.
//!
//! The observation is published at the two dimension sets the series is reported at, which are
//! independent series because a metric's identity includes its dimension set:
//!
//! * the cluster, dimensioned by [`DB_CLUSTER_IDENTIFIER_DIMENSION`] alone; and
//! * each of its Serverless v2 MEMBER instances ([`super::SERVERLESS_INSTANCE_CLASS`]),
//!   dimensioned by [`DB_INSTANCE_IDENTIFIER_DIMENSION`] — the capacity member's own documented
//!   subject is "a DB instance in an Aurora Serverless v2 cluster", so a cluster's members each
//!   publish the capacity they hold.
//!
//! # What one observation reads
//!
//! `MinCapacity` is "the minimum number of Aurora capacity units (ACUs) for a DB instance in an
//! Aurora Serverless v2 cluster … For Aurora versions that support the Aurora Serverless v2
//! auto-pause feature, the smallest value that you can use is 0. For versions that don't
//! support Aurora Serverless v2 auto-pause, the smallest value that you can use is 0.5", and
//! `SecondsUntilAutoPause` is "the number of seconds an Aurora Serverless v2 DB instance must be
//! idle before Aurora attempts to automatically pause it … This property is only shown when the
//! minimum capacity for the cluster is set to 0 ACUs". So:
//!
//! * a cluster that is RUNNING holds its configured minimum, and never less than the smallest
//!   capacity a running Serverless v2 instance has ([`MIN_RUNNING_CAPACITY`]) — a cluster
//!   configured down to 0 ACUs still runs on half an ACU until it pauses;
//! * a cluster whose minimum is 0 and that has been IDLE for its `SecondsUntilAutoPause` is
//!   PAUSED and holds [`PAUSED_CAPACITY`] — nothing is running, which is what scale-to-zero
//!   means;
//! * a cluster with an open session is not idle, so it cannot pause however long it has been
//!   connected for, and the next session on a paused cluster resumes it.
//!
//! Idleness is read from the SESSIONS the cluster's own engine endpoints and its members' hold
//! ([`super::data_plane::pg_sessions::SessionRegistry`]) — the connections this emulator is
//! really serving — so neither the pause nor the resume is a function of who called the control
//! plane. The data a paused cluster holds is untouched by the pause: the engine's databases stay
//! exactly as they were, which is what makes a resumed cluster still hold every row committed
//! before it paused.
//!
//! # The clock the series is published on
//!
//! The metric is published once a MINUTE ([`PUBLISH_INTERVAL`]), the finest period a
//! regular-resolution metric can be read at, and a series opens with [`RUN_UP_PERIODS`]
//! datapoints already behind it so a reader finds a series that has been running. The datapoints
//! are appended to the CloudWatch emulator's OWN metric store — the same store `PutMetricData`
//! writes and `GetMetricStatistics` reads — reached through the shared engine, so one metric has
//! one home. They go into it directly rather than through `PutMetricData`, which reserves the
//! `AWS/` namespaces for the services that own them ("you should not specify a namespace that
//! begins with `AWS/`", `PutMetricData$Namespace`).
//!
//! What makes that observable without any further call is [`drive_serverless_capacity`]: the
//! writes that bring a Serverless v2 cluster into being publish whatever is already due and
//! start the one bounded in-process task that waits for the next due instant. Nothing here is on
//! a READ path, and nothing it publishes depends on who called: every datapoint it materialises
//! is one the stored due instants had already made due, and so is the pause, which is owed at
//! the instant the idle period runs out whether or not anybody asks.
//!
//! Every instant here is the EMULATOR's clock ([`::service_common::clock`]), which is the
//! process clock unless an operator stops it: a pause that is due only after an idle period has
//! passed is due when that clock says so.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use super::{ComAmazonawsRdsAmazonrdsv19State as RdsState, RegionalKey};

/// The namespace Amazon RDS publishes the metrics of its DB resources into.
pub const METRIC_NAMESPACE: &str = "AWS/RDS";

/// The metric reporting the current capacity of an Aurora Serverless v2 DB cluster, in Aurora
/// capacity units.
pub const SERVERLESS_DATABASE_CAPACITY_METRIC: &str = "ServerlessDatabaseCapacity";

/// The dimension naming the DB cluster an observation is about: its `DBClusterIdentifier`.
pub const DB_CLUSTER_IDENTIFIER_DIMENSION: &str = "DBClusterIdentifier";

/// The dimension naming the DB INSTANCE an observation is about: its `DBInstanceIdentifier`.
pub const DB_INSTANCE_IDENTIFIER_DIMENSION: &str = "DBInstanceIdentifier";

/// What the capacity reads while the cluster is paused: no capacity at all, which is what
/// scaling to zero leaves running.
pub const PAUSED_CAPACITY: f64 = 0.0;

/// The smallest capacity a RUNNING Aurora Serverless v2 instance holds, in ACUs: half an ACU,
/// the smallest value `MinCapacity` takes for a version without the auto-pause feature ("For
/// versions that don't support Aurora Serverless v2 auto-pause, the smallest value that you can
/// use is 0.5"). A cluster configured down to 0 ACUs runs on this until it pauses — 0 ACUs is
/// the PAUSED state, not a running capacity.
pub const MIN_RUNNING_CAPACITY: f64 = 0.5;

/// How far apart the datapoints of one series are STAMPED: the minute a regular-resolution
/// metric is read and alarmed at.
pub const PUBLISH_INTERVAL: Duration = Duration::from_secs(60);

/// The `StorageResolution` these datapoints are stored at, in seconds: the regular resolution a
/// metric has unless its publisher asks for the high-resolution one ("This field is optional, if
/// you do not specify it the default of 60 is used", `MetricDatum$StorageResolution`).
const STORAGE_RESOLUTION_SECS: i32 = 60;

/// How many datapoints a series OPENS with, stamped one [`PUBLISH_INTERVAL`] apart ahead of the
/// instant the cluster became available.
///
/// The emulator's own figure, not an AWS one: the real series has been running for as long as
/// the cluster has, so a reader — or an alarm created straight after — sees as many periods of
/// data as it asks for. Five periods is the run-up of the longest evaluation an
/// `EvaluationPeriods` of one to five asks for.
const RUN_UP_PERIODS: u32 = 5;

/// How far apart the datapoints of one series come DUE while the series is still catching up
/// with the present.
///
/// The emulator's own span: the run-up is published in a little over a second, so a caller
/// reading the series the instant the cluster became available still finds the empty series a
/// cluster that has published nothing yet has.
const ACCELERATED_SPACING: Duration = Duration::from_millis(250);

/// The shortest a clock wake-up waits, so a datapoint already past its due instant still costs a
/// yield rather than a CPU-bound spin.
const MIN_PUBLISH_WAIT: Duration = Duration::from_millis(50);

/// The longest a clock wake-up waits, so the clock re-reads the store — which a write, a session
/// or an advance of the emulator's clock may have changed since — at least this often.
const MAX_PUBLISH_WAIT: Duration = Duration::from_secs(5);

/// How many times one clock wakes up before it stands down.
///
/// A backstop, not a schedule: it bounds the work one request can start, and the next write that
/// brings a Serverless v2 cluster into being starts a clock again if none is running.
const MAX_CLOCK_WAKE_UPS: usize = 2_000;

/// How many datapoints ONE pass publishes, over every series the store holds.
///
/// A bound on the work one wake-up does, not a quota: a cluster this pass has no room for keeps
/// the due instant it carries, so the next wake-up publishes its minute intact.
const MAX_DATAPOINTS_PER_PASS: usize = 2_000;

/// The cluster-wide series of the DB cluster `identifier`.
#[must_use]
pub fn cluster_series(identifier: &str) -> cloudwatch::MetricKey {
    series(DB_CLUSTER_IDENTIFIER_DIMENSION, identifier)
}

/// The series of ONE Serverless v2 member instance of such a cluster.
#[must_use]
pub fn instance_series(identifier: &str) -> cloudwatch::MetricKey {
    series(DB_INSTANCE_IDENTIFIER_DIMENSION, identifier)
}

/// The one place a published series is addressed, so the publish and every reader that looks for
/// what it stored name the same series.
fn series(dimension: &str, value: &str) -> cloudwatch::MetricKey {
    let mut dimensions: BTreeMap<String, String> = BTreeMap::new();
    dimensions.insert(dimension.to_string(), value.to_string());
    cloudwatch::MetricKey {
        namespace: METRIC_NAMESPACE.to_string(),
        metric_name: SERVERLESS_DATABASE_CAPACITY_METRIC.to_string(),
        dimensions,
    }
}

/// One series' place in its schedule: the stamp its next datapoint carries, and when that
/// datapoint comes due.
#[derive(Debug)]
struct SeriesClock {
    /// The time stamp the next datapoint carries. Advances by [`PUBLISH_INTERVAL`] per publish,
    /// so the series keeps the spacing the real one has.
    next_stamp: SystemTime,
    /// The instant the next datapoint comes due, on the emulator's clock.
    next_due_at: Instant,
}

/// How long one cluster has been idle, and whether that has paused it.
#[derive(Debug)]
struct IdleClock {
    /// When the cluster last had no session open at all, or `None` while one is open.
    idle_since: Option<Instant>,
    /// Whether the cluster is paused now.
    paused: bool,
}

/// Where every capacity series of this store has got to, how long each of its clusters has been
/// idle, and the one in-process clock that advances them.
///
/// Held on the store so the schedule is the store's own: a series exists while the cluster it
/// belongs to does, and the clock is claimed once however many writes drive it.
#[derive(Debug, Default)]
pub struct ServerlessCapacityPublisher {
    /// The schedule of each Serverless v2 cluster being published, by its key in the store.
    series: HashMap<RegionalKey, SeriesClock>,
    /// How long each of those clusters has been idle, by the same key.
    idle: HashMap<RegionalKey, IdleClock>,
    /// Whether a clock is running for this store.
    clock_running: bool,
    /// Which claim the running clock holds, so a clock that has stood down cannot release a
    /// claim a later one took.
    clock_generation: u64,
    /// The running clock's task, held so the task a request started stays stoppable.
    clock: Option<::tokio::task::JoinHandle<()>>,
}

impl ServerlessCapacityPublisher {
    /// The capacity the cluster `key` is reported at now, or `None` for a cluster this store
    /// publishes no capacity series for.
    ///
    /// For a reader inside this crate — a test, or an operation that reports the capacity a
    /// cluster holds — so there is one answer rather than a second reading of the same state.
    #[must_use]
    pub fn reported_capacity(&self, key: &RegionalKey, configured_minimum: f64) -> Option<f64> {
        let idle = self.idle.get(key)?;
        Some(match idle.paused {
            true => PAUSED_CAPACITY,
            false => configured_minimum.max(MIN_RUNNING_CAPACITY),
        })
    }
}

/// Publish every capacity datapoint the clock has already made due, and start the one bounded
/// in-process task that waits for the next one.
///
/// Called by the writes that bring a Serverless v2 cluster into being or change its scaling
/// configuration — never by a read: the datapoints this materialises are the ones the stored due
/// instants have already made due, so what a caller sees does not depend on whether anybody
/// called.
///
/// With no async runtime to spawn onto there is no clock, and what is due at the moment of the
/// write is then the whole series until the next write — the router always runs on one.
pub fn drive_serverless_capacity<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) {
    publish_due_capacity(&**engine);
    let Some(state) = super::service_state(&**engine) else {
        return;
    };
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(generation) = claim_clock(&mut state.lock()) else {
        return;
    };
    let engine = Arc::downgrade(engine);
    let claimed = ::engine::ServiceState::clone(&state);
    let handle = runtime.spawn(async move {
        for _ in 0..MAX_CLOCK_WAKE_UPS {
            // The emulator this clock publishes for, for as long as it is still there: the
            // handle is WEAK and is upgraded per wake-up, so a clock cannot keep an emulator
            // nobody else holds — its listeners, its engines and its stores — alive, and it
            // stands down with it.
            let Some(alive) = engine.upgrade() else {
                break;
            };
            let read_at = ::service_common::clock::now(&*alive);
            let Some(wait) = next_wait_or_release(&mut state.lock(), generation, read_at) else {
                // Nothing is going to publish, and the claim is already released.
                return;
            };
            drop(alive);
            // Every wake-up waits, even for an instant already past: that is what keeps a due
            // datapoint from costing a CPU-bound spin, and it hands the runtime back between
            // wake-ups.
            ::tokio::time::sleep(wait).await;
            let Some(alive) = engine.upgrade() else {
                break;
            };
            publish_due_capacity(&*alive);
        }
        // Only THIS clock's claim, so a clock started since keeps its own.
        release_clock(&mut state.lock(), generation);
    });
    set_clock(&mut claimed.lock(), generation, handle);
}

/// Keep the running clock's task handle, aborting the one it replaces — and only while
/// `generation` is still the CURRENT claim, so a starter whose own task has already stood down
/// installs nothing and aborts nothing.
fn set_clock(store: &mut RdsState, generation: u64, handle: ::tokio::task::JoinHandle<()>) {
    if store.serverless_capacity.clock_generation != generation {
        handle.abort();
        return;
    }
    if let Some(previous) = store.serverless_capacity.clock.replace(handle) {
        previous.abort();
    }
}

/// Claim the single publisher clock for this store: the claim's GENERATION when the caller may
/// start one, `None` when one already runs.
fn claim_clock(store: &mut RdsState) -> Option<u64> {
    if store.serverless_capacity.clock_running {
        return None;
    }
    store.serverless_capacity.clock_running = true;
    store.serverless_capacity.clock_generation =
        store.serverless_capacity.clock_generation.wrapping_add(1);
    Some(store.serverless_capacity.clock_generation)
}

/// Release the claim `generation` took — and ONLY that one, so a clock that ended on the wake-up
/// cap cannot clear a claim a clock started since is holding.
fn release_clock(store: &mut RdsState, generation: u64) {
    if store.serverless_capacity.clock_generation == generation {
        store.serverless_capacity.clock_running = false;
    }
}

/// How long the clock waits before its next pass, or `None` — having RELEASED its claim — when
/// the store holds no Serverless v2 cluster at all.
///
/// Observing "nothing is owed" and standing down happen in ONE critical section, which is what
/// makes the hand-over race-free: a write holds this same lock to store the cluster BEFORE it
/// tries to claim the clock, so either this call sees that cluster and the running clock keeps
/// it, or the write's claim finds the flag cleared and starts a clock of its own.
fn next_wait_or_release(
    store: &mut RdsState,
    generation: u64,
    now: Instant,
) -> Option<Duration> {
    let due = store
        .serverless_capacity
        .series
        .values()
        .map(|series| series.next_due_at)
        .min();
    // A cluster still being brought up publishes nothing yet, and its first datapoint is owed the
    // moment it becomes available — which the instant the store holds for it already fixes.
    let coming_up = serverless_clusters(store)
        .into_iter()
        .filter(|cluster| !cluster.publishing)
        .map(|cluster| cluster.ready_at)
        .min();
    match [due, coming_up].into_iter().flatten().min() {
        Some(at) => Some(
            at.saturating_duration_since(now)
                .clamp(MIN_PUBLISH_WAIT, MAX_PUBLISH_WAIT),
        ),
        None => {
            release_clock(store, generation);
            None
        }
    }
}

/// What one pass needs to know about one Serverless v2 cluster, read off the store before
/// anything is written: no observation depends on the order the pass walks the store in.
#[derive(Debug)]
struct ClusterCapacity {
    /// The cluster's key in the store.
    key: RegionalKey,
    /// Whether this cluster is far enough along to publish a series at all.
    publishing: bool,
    /// When the cluster becomes available, for one that is not publishing yet.
    ready_at: Instant,
    /// `MinCapacity`, the capacity the cluster holds while it runs.
    min_capacity: f64,
    /// `SecondsUntilAutoPause`, the idle period that pauses a cluster whose minimum is 0.
    seconds_until_auto_pause: Duration,
    /// Whether the cluster can pause at all: "This property is only shown when the minimum
    /// capacity for the cluster is set to 0 ACUs."
    can_pause: bool,
    /// Whether any session is open on the cluster's own endpoints or on one of its members'.
    connected: bool,
    /// The `DBInstanceIdentifier` of each Serverless v2 member, which publishes a series of its
    /// own.
    serverless_members: Vec<String>,
}

/// Every Serverless v2 DB cluster this store holds, with what one capacity observation of it
/// needs.
///
/// Walks the clusters of each region this store keeps records for, which the region's own cluster
/// quota bounds, and reads nothing but the records and the live session registries.
fn serverless_clusters(store: &RdsState) -> Vec<ClusterCapacity> {
    let regions: Vec<String> = store
        .db_clusters
        .keys()
        .map(|(region, _)| region.clone())
        .collect::<HashSet<String>>()
        .into_iter()
        .collect();
    let mut clusters: Vec<ClusterCapacity> = Vec::new();
    for region in regions {
        for cluster in store.clusters_in(&region) {
            let Some(scaling) = &cluster.serverless_v2_scaling else {
                continue;
            };
            let key: RegionalKey = (cluster.region.clone(), cluster.identifier.clone());
            let min_capacity = scaling.min_capacity.unwrap_or(MIN_RUNNING_CAPACITY);
            let members = store.cluster_members(cluster);
            // Every session this emulator is serving for the cluster: the ones on the cluster's
            // own writer and reader endpoints, and the ones on each member instance's own
            // endpoint. Any of them is a connection the cluster is serving, so it is not idle.
            let connected = store
                .cluster_engine_endpoint(&key)
                .is_some_and(|endpoint| endpoint.sessions().open_count() > 0)
                || members.iter().any(|member| {
                    store
                        .engine_endpoint(&(member.region.clone(), member.identifier.clone()))
                        .is_some_and(|endpoint| endpoint.sessions().open_count() > 0)
                });
            clusters.push(ClusterCapacity {
                publishing: cluster.status == super::DB_CLUSTER_STATUS_AVAILABLE,
                ready_at: cluster.ready_at,
                min_capacity,
                seconds_until_auto_pause: Duration::from_secs(
                    u64::try_from(scaling.seconds_until_auto_pause.unwrap_or(
                        *super::SECONDS_UNTIL_AUTO_PAUSE_RANGE.start(),
                    ))
                    .unwrap_or_default(),
                ),
                // Auto-pause is the feature a cluster asks for by naming a minimum of 0 ACUs.
                can_pause: min_capacity <= PAUSED_CAPACITY,
                connected,
                serverless_members: members
                    .iter()
                    .filter(|member| {
                        member.db_instance_class == super::SERVERLESS_INSTANCE_CLASS
                    })
                    .map(|member| member.identifier.clone())
                    .collect(),
                key,
            });
        }
    }
    clusters
}

/// Publish the datapoint every series that is due is reported as.
///
/// Three steps, so no store guard is held over another service's lock and the two stores are
/// never locked at once:
///
/// 1. under THIS service's guard, materialise the transitions the clock owes, so the statuses the
///    pass reads are the ones that are already due;
/// 2. still under that guard, move each cluster's idle clock on and take the datapoints the due
///    series are published as;
/// 3. with the metric store's guard, append them and make the alarms over them due.
fn publish_due_capacity<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let now = SystemTime::now();
    // The pass is dated by the EMULATOR's clock, like every transition it materialises: a clock
    // that stands still owes no datapoint and no pause yet.
    let instant = ::service_common::clock::now(engine);
    let published = {
        let mut store = state.lock();
        let regions: Vec<String> = store
            .db_clusters
            .keys()
            .map(|(region, _)| region.clone())
            .collect::<HashSet<String>>()
            .into_iter()
            .collect();
        for region in regions {
            store.settle_due_transitions(&region);
        }
        let clusters = serverless_clusters(&store);
        settle_idle_clocks(&mut store, &clusters, instant);
        sync_series(&mut store, &clusters, now, instant);
        due_datapoints(&mut store, &clusters, instant)
    };
    append_datapoints(engine, published, now);
}

/// Move each cluster's idle clock on, and close the clocks of the clusters this store no longer
/// holds.
///
/// A cluster with a session open is not idle, so its idle clock is cleared and a paused cluster
/// RESUMES — which is what the next login does to a paused Serverless v2 cluster. A cluster with
/// none has been idle since the first pass that found it so, and it is PAUSED once that has been
/// longer than its `SecondsUntilAutoPause`: a change the clock made due, which nobody has to ask
/// for.
fn settle_idle_clocks(store: &mut RdsState, clusters: &[ClusterCapacity], instant: Instant) {
    let held: HashSet<RegionalKey> = clusters
        .iter()
        .map(|cluster| cluster.key.clone())
        .collect();
    store
        .serverless_capacity
        .idle
        .retain(|key, _| held.contains(key));
    for cluster in clusters {
        let clock = store
            .serverless_capacity
            .idle
            .entry(cluster.key.clone())
            .or_insert(IdleClock {
                idle_since: Some(instant),
                paused: false,
            });
        if cluster.connected {
            clock.idle_since = None;
            clock.paused = false;
            continue;
        }
        let since = *clock.idle_since.get_or_insert(instant);
        clock.paused = cluster.can_pause
            && instant.saturating_duration_since(since) >= cluster.seconds_until_auto_pause;
    }
}

/// Open a series for every Serverless v2 cluster that is publishing one, and close the series of
/// every cluster this store no longer holds or no longer publishes for.
///
/// A series opens with its run-up already behind it ([`RUN_UP_PERIODS`]) and its first datapoint
/// due at once, so the pass that finds the cluster available publishes the series' first
/// datapoints itself. A cluster whose series already exists keeps it, so one that pauses and
/// resumes keeps ONE series across both rather than restarting its run-up.
fn sync_series(
    store: &mut RdsState,
    clusters: &[ClusterCapacity],
    now: SystemTime,
    instant: Instant,
) {
    let publishing: HashSet<RegionalKey> = clusters
        .iter()
        .filter(|cluster| cluster.publishing)
        .map(|cluster| cluster.key.clone())
        .collect();
    store
        .serverless_capacity
        .series
        .retain(|key, _| publishing.contains(key));
    let run_up = PUBLISH_INTERVAL.saturating_mul(RUN_UP_PERIODS);
    for key in publishing {
        store
            .serverless_capacity
            .series
            .entry(key)
            .or_insert(SeriesClock {
                next_stamp: now.checked_sub(run_up).unwrap_or(now),
                next_due_at: instant,
            });
    }
}

/// Take the datapoints every series that is due at `instant` is published as, and advance each of
/// those series to its next datapoint.
///
/// One observation per due cluster, published at BOTH the dimension sets it is reported at: the
/// cluster's own and one per Serverless v2 member. The whole minute of ONE cluster is taken
/// together or not at all, so a pass with no room for all of it leaves that cluster's clock where
/// it is and the next wake-up publishes the minute whole ([`MAX_DATAPOINTS_PER_PASS`]).
fn due_datapoints(
    store: &mut RdsState,
    clusters: &[ClusterCapacity],
    instant: Instant,
) -> Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> {
    let mut published: Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> = Vec::new();
    for cluster in clusters {
        let due = store
            .serverless_capacity
            .series
            .get(&cluster.key)
            .is_some_and(|series| instant >= series.next_due_at);
        if !due {
            continue;
        }
        let capacity = store
            .serverless_capacity
            .reported_capacity(&cluster.key, cluster.min_capacity)
            .unwrap_or_else(|| cluster.min_capacity.max(MIN_RUNNING_CAPACITY));
        // Room for this cluster's WHOLE minute, taken before its clock moves.
        if published.len() + 1 + cluster.serverless_members.len() > MAX_DATAPOINTS_PER_PASS {
            return published;
        }
        let Some(series) = store.serverless_capacity.series.get_mut(&cluster.key) else {
            continue;
        };
        let stamp = advance(series, instant);
        let datum = |stamp: SystemTime| cloudwatch::StoredDatum {
            timestamp_secs: epoch_secs(stamp),
            // One observation: the capacity the cluster holds at this minute.
            sample_count: 1.0,
            sum: capacity,
            minimum: capacity,
            maximum: capacity,
            // An Aurora capacity unit is no member of the `StandardUnit` vocabulary, so the
            // datapoint declares no unit — and the metric store reports a datapoint published
            // without one under the value the enumerated type has for a measurement that has
            // none.
            unit: None,
            storage_resolution: STORAGE_RESOLUTION_SECS,
        };
        published.push((cluster_series(&cluster.key.1), datum(stamp)));
        for member in &cluster.serverless_members {
            published.push((instance_series(member), datum(stamp)));
        }
    }
    published
}

/// Take the time stamp of the datapoint that is due and schedule the next one, `instant` being
/// what the emulator's clock read when this pass started.
fn advance(series: &mut SeriesClock, instant: Instant) -> SystemTime {
    let stamp = series.next_stamp;
    series.next_stamp = stamp.checked_add(PUBLISH_INTERVAL).unwrap_or(stamp);
    // While the series is still behind the present it is published at the accelerated spacing;
    // once it has caught up, the next datapoint is due one publish interval later, which is the
    // spacing the real series has.
    let wait = series
        .next_stamp
        .duration_since(SystemTime::now())
        .unwrap_or(ACCELERATED_SPACING)
        .min(PUBLISH_INTERVAL);
    series.next_due_at = instant.checked_add(wait).unwrap_or(instant);
    stamp
}

/// Append the datapoints this pass published to the metric store, and make every alarm over the
/// series they belong to due for evaluation.
///
/// Nothing is published on an engine that holds no metric store to reach: a datapoint has one
/// home, and this path never makes a second one. An alarm is evaluated FROM these datapoints, so
/// storing a series it watches is what makes its evaluation due — an alarm cannot tell a
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
            // The metric store holds no more series. The cluster itself is up either way, so
            // nothing is failed for it; the dropped observation is reported where a developer can
            // see it.
            ::tracing::warn!(
                metric = SERVERLESS_DATABASE_CAPACITY_METRIC,
                "the CloudWatch metric store is at capacity, so this capacity observation was \
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

/// `at` as the epoch seconds a stored datapoint carries, which is the unit the metric store's own
/// readers window by. An instant before the epoch (which no clock reports) reads as the epoch
/// itself rather than failing the publish.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}
