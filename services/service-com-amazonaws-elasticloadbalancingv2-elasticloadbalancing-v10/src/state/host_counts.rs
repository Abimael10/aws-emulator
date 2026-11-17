// SPDX-License-Identifier: CC0-1.0

//! The HOST-COUNT series a load balancer publishes for the target groups it routes to, and
//! the periodic health check those counts are read off.
//!
//! A load balancer does not only hold its target groups' registrations; it CHECKS them, over
//! and over, and reports how many of each group's targets it currently considers healthy and
//! unhealthy as the CloudWatch metrics [`HEALTHY_HOST_COUNT`] and [`UNHEALTHY_HOST_COUNT`] in
//! the namespace its own family publishes ([`namespace`]), dimensioned by the pair
//! [`TARGET_GROUP_DIMENSION`] + [`LOAD_BALANCER_DIMENSION`] — the two resources a count
//! belongs to. That series is how a client watches a target group: an alarm on it is what
//! drives unhealthy-host remediation, so a load balancer that publishes nothing leaves every
//! such alarm without data.
//!
//! A series belongs to the PAIR, so it exists exactly while a listener or a rule of that load
//! balancer forwards to that target group ([`super::ComAmazonawsElasticloadbalancingv2ElasticloadbalancingV10State::refresh_target_group_associations`]):
//! a target group nothing routes to is checked by no load balancer and publishes no series at
//! all, which is what a reader of such a group's series finds.
//!
//! The datapoints go into the CloudWatch emulator's OWN metric store — the same store
//! `PutMetricData` writes and `GetMetricStatistics` / `GetMetricData` / `ListMetrics` read —
//! reached through the shared engine, so one metric has one home and the publishers cannot
//! drift. They are appended to that store directly rather than sent through `PutMetricData`,
//! which reserves the `AWS/` namespaces for the services that own them ("you should not
//! specify a namespace that begins with `AWS/`", `PutMetricData$Namespace`).
//!
//! # What one check observes
//!
//! A health check reaches the registered target over the network, and this emulator makes no
//! outbound connection — so what one check can observe is whether the emulator itself holds
//! something that stands behind the registration, ON THE PORT the check connects to
//! ([`observe`]). That port is the group's effective health-check port — `HealthCheckPort`
//! when it names one, and the port the target receives traffic on otherwise:
//!
//! * a registration whose workload declares the ports it LISTENS on
//!   ([`super::ServedPorts`], what another emulated service's scheduler registered it with)
//!   fails every check against a port that is not one of them: a workload that is running
//!   answers nothing on a port it does not serve, which is what a health check pointed at
//!   the wrong port finds;
//! * a registration the security groups governing it admit no connection to on that port from
//!   the load balancer doing the checking fails every check too: a check that cannot arrive
//!   is answered by nothing. Those groups are the ones the scheduler running the workload
//!   declared ([`super::GuardingGroups`]), else the ones the account's inventory reports for
//!   the resource the registration NAMES — the interfaces attached to an instance it names,
//!   and the interface holding an address it names ([`super::health_check_firewall`]);
//! * otherwise, a registration another emulated service's SCHEDULER made for a workload it
//!   runs stands for that workload, which is running here, so the check passes;
//! * an `instance` target passes while EC2 — which owns instances — reports that instance
//!   running, and fails for one it reports stopped or holds no longer;
//! * an `ip` target passes while some network interface of the account holds that address,
//!   which is what makes the address one something in this VPC answers on, and fails for an
//!   address no interface holds;
//! * anything else observes NOTHING, and a registration nothing was observed about keeps the
//!   health it has rather than being failed on silence: a `lambda` target, whose function is
//!   another service's resource, and every target of a group whose VPC the account's EC2
//!   inventory does not hold — an inventory that knows nothing about the network the target
//!   sits in can report nothing about the target either ([`super::network`] reads every
//!   other fact this service does not own the same way).
//!
//! The verdict only MOVES once the target group's own thresholds are reached — "the number of
//! consecutive health check failures required before considering a target unhealthy"
//! (`CreateTargetGroup$UnhealthyThresholdCount`) and its healthy counterpart — which
//! [`super::target_health::TargetProbe`] folds the observations into, and a registration whose
//! checks have failed without reaching that threshold is still `initial`. Those checks come
//! one `HealthCheckIntervalSeconds` apart, the cadence the caller configured
//! ([`super::target_health::health_check_interval`]), so a verdict is never reached sooner
//! than the threshold times that interval; a pass that looks at a registration between two of
//! its checks keeps what it saw and counts toward no verdict.
//!
//! # The clock the series is published on
//!
//! An Application Load Balancer publishes these counts once a MINUTE
//! ([`PUBLISH_INTERVAL`]), so the run-up an alarm over several evaluation periods needs is
//! minutes long — which a client is not going to sit through against an emulator. So the
//! series is published on an ACCELERATED schedule: its datapoints come due
//! [`ACCELERATED_SPACING`] apart until the series reaches the present, and one publish
//! interval apart from then on, while every datapoint keeps the one-minute SPACING the real
//! series has. A series therefore opens with [`RUN_UP_PERIODS`] datapoints stamped BEFORE the
//! instant routing began, and an alarm whose evaluation periods reach back over them sees a
//! series that has been running. What each datapoint COUNTS is the health the checks have
//! reached by then, on their own schedule above.
//!
//! So a datapoint is published only once there is a count to report: while the checks have
//! determined the health of NO registration of the group, the load balancer has found out
//! nothing to count ([`super::TargetGroupRecord::reported_host_counts`]), and the datapoint
//! stays owed rather than being published as a zero — a count of no unhealthy targets among
//! registrations nothing has yet been observed about is a verdict no check reached, and an
//! alarm over the series would be graded from it. The series therefore begins at the first
//! verdict the checks reached, which is also the earliest instant AWS's own series could
//! report one; a group some of whose registrations are decided keeps reporting the counts it
//! has, so one registration coming up never silences a series.
//!
//! What makes that observable without any further call is [`drive_host_count_metrics`]: the
//! write that changes what a load balancer routes to, or which targets are registered,
//! publishes what is due at that instant and starts the one bounded in-process task that
//! waits for the next due check. Nothing here is on a READ path, and nothing it publishes
//! depends on who called: the checks it materialises are the ones the stored due instants
//! have already made due. [`check_registrations`] is the one entry point another emulated
//! service's scheduler reaches, for the first check of a registration it has just created:
//! it observes those registrations and nothing else, and publishes no datapoint, so it moves
//! no other registration's probe and no series' schedule.

use ::std::collections::{BTreeMap, HashMap, HashSet};
use ::std::sync::Arc;
use ::std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;
use ::service_com_amazonaws_ec2_amazonec2::ops::types as ec2_types;
use ::service_com_amazonaws_ec2_amazonec2::state as ec2;

use super::target_health::{HEALTHY_STATE, INITIAL_STATE, UNHEALTHY_STATE};
use super::{
    load_balancer_type_code,
    ComAmazonawsElasticloadbalancingv2ElasticloadbalancingV10State as ElbState, GuardingGroups,
    LoadBalancerRecord, RegisteredTarget, TargetGroupRecord, TargetKey,
};

/// "The number of targets that are considered healthy" — the metric an Elastic Load
/// Balancing namespace reports per target group.
pub const HEALTHY_HOST_COUNT: &str = "HealthyHostCount";

/// "The number of targets that are considered unhealthy", the companion of
/// [`HEALTHY_HOST_COUNT`].
pub const UNHEALTHY_HOST_COUNT: &str = "UnHealthyHostCount";

/// The dimension naming the target group a count is about: its ARN from the resource type
/// onwards (`targetgroup/<name>/<id>`), which is how an Elastic Load Balancing dimension
/// names a target group.
pub const TARGET_GROUP_DIMENSION: &str = "TargetGroup";

/// The dimension naming the load balancer that did the checking: its ARN from the family
/// code onwards (`app/<name>/<id>`), which is how an Elastic Load Balancing dimension names
/// a load balancer.
pub const LOAD_BALANCER_DIMENSION: &str = "LoadBalancer";

/// The `StandardUnit` a host count carries: it is a number of targets.
pub const COUNT_UNIT: &str = "Count";

/// The namespace an Application Load Balancer's metrics live in.
pub const APPLICATION_NAMESPACE: &str = "AWS/ApplicationELB";

/// The namespace a Network Load Balancer's metrics live in.
pub const NETWORK_NAMESPACE: &str = "AWS/NetworkELB";

/// The namespace a Gateway Load Balancer's metrics live in.
pub const GATEWAY_NAMESPACE: &str = "AWS/GatewayELB";

/// How far apart the datapoints of one series are STAMPED: Elastic Load Balancing reports
/// these counts per minute, which is also the finest period an alarm over a regular-resolution
/// metric can be evaluated at.
pub const PUBLISH_INTERVAL: Duration = Duration::from_secs(60);

/// The `StorageResolution` the datapoints are stored at, in seconds: the regular resolution a
/// metric has unless its publisher asks for the high-resolution one ("This field is optional,
/// if you do not specify it the default of 60 is used", `MetricDatum$StorageResolution`).
const STORAGE_RESOLUTION_SECS: i32 = 60;

/// How many datapoints a series OPENS with, stamped one [`PUBLISH_INTERVAL`] apart ahead of
/// the instant the load balancer began routing to the target group.
///
/// The emulator's own figure, not an AWS one: the real series has been running for as long as
/// the routing has, so an alarm created straight after it sees as many periods of data as its
/// `EvaluationPeriods` asks for. Five periods is the run-up of the longest evaluation an
/// `EvaluationPeriods` of one to five asks for, and nothing is instant either way — the
/// datapoints carry the verdicts the checks really reached, in the order they reached them.
const RUN_UP_PERIODS: u32 = 5;

/// How far apart the DATAPOINTS of one series come due while that series is still catching up
/// with the present.
///
/// The emulator's own span, and the publish schedule's alone: how often a registration is
/// CHECKED is the target group's own `HealthCheckIntervalSeconds`
/// ([`super::target_health::health_check_interval`]), which this never shortens — a series
/// that catches up faster than the checks run reports the same verdict in several of its
/// datapoints, which is what a series of a metric published more often than its subject
/// changes reports.
const ACCELERATED_SPACING: Duration = Duration::from_millis(250);

/// The shortest a publisher-clock wake-up waits, so a check already past its due instant
/// still costs a yield rather than a CPU-bound spin.
const MIN_PUBLISH_WAIT: Duration = Duration::from_millis(50);

/// The longest a publisher-clock wake-up waits, so the clock re-reads the store — which a
/// write may have changed since — at least this often.
const MAX_PUBLISH_WAIT: Duration = Duration::from_secs(5);

/// How many times one publisher clock wakes up before it stands down.
///
/// A backstop, not a schedule: it bounds the work one request can start, and the next write
/// to a load balancer's routing or registrations starts a clock again if none is running.
const MAX_CLOCK_WAKE_UPS: usize = 2_000;

/// How many registrations ONE pass checks, over every target group the store holds.
///
/// A bound on the work one wake-up does, not a quota: the pass walks the registrations of the
/// routed groups, and a store holding more than this many hands the rest to the next wake-up
/// rather than doing unbounded work while a clock tick is being served.
const MAX_CHECKS_PER_PASS: usize = 2_000;

/// The namespace the host counts of a load balancer of `lb_type` are published into.
///
/// One namespace per FAMILY, as each family's own metric reference names it: the counts of an
/// Application Load Balancer are not the counts of a Network Load Balancer, and a reader
/// windows exactly one of them.
#[must_use]
pub fn namespace(lb_type: &str) -> &'static str {
    match load_balancer_type_code(lb_type) {
        "net" => NETWORK_NAMESPACE,
        "gwy" => GATEWAY_NAMESPACE,
        _ => APPLICATION_NAMESPACE,
    }
}

/// How an Elastic Load Balancing dimension names the load balancer `record`:
/// `<family>/<name>/<id>`, which is its ARN from the family code onwards.
#[must_use]
pub fn load_balancer_dimension_value(record: &LoadBalancerRecord) -> String {
    format!(
        "{}/{}/{}",
        load_balancer_type_code(&record.lb_type),
        record.name,
        record.id
    )
}

/// How an Elastic Load Balancing dimension names the target group `arn` identifies:
/// `targetgroup/<name>/<id>`, which is that ARN from the resource type onwards.
///
/// `None` for a value carrying no target-group resource field, which names no target group
/// for a dimension to be about.
#[must_use]
pub fn target_group_dimension_value(arn: &str) -> Option<String> {
    let marker = ":targetgroup/";
    let at = arn.find(marker)?;
    Some(arn[at + 1..].to_string())
}

/// One series' place in its schedule: the stamp its next datapoint carries, and when that
/// datapoint comes due.
#[derive(Debug)]
struct SeriesClock {
    /// The time stamp the next datapoint carries. Advances by [`PUBLISH_INTERVAL`] per
    /// datapoint, so the series keeps the spacing the real one has.
    next_stamp: SystemTime,
    /// The instant the next datapoint comes due; see [`ACCELERATED_SPACING`].
    next_datapoint_at: Instant,
    /// Whether a datapoint of this series has been PUBLISHED yet.
    ///
    /// What keeps the run-up a series opens with to the periods before its FIRST datapoint: a
    /// series that has already published stands where its schedule has reached, so a period it
    /// has reported is never re-stamped ([`hold`]).
    published: bool,
}

/// Where every host-count series of this store has got to, and the one in-process clock that
/// advances them.
///
/// Held on the store so the schedule is the store's own: a series exists while the pair it
/// belongs to is routed, and the clock is claimed once however many writes drive it.
#[derive(Debug, Default)]
pub struct HostCountPublisher {
    /// The schedule of each `(load balancer ARN, target group ARN)` pair being checked.
    series: HashMap<(String, String), SeriesClock>,
    /// Whether a clock is running for this store.
    clock_running: bool,
    /// Which claim the running clock holds, so a clock that has stood down cannot release a
    /// claim a later one took.
    clock_generation: u64,
    /// The running clock's task, held so the task a request started stays stoppable.
    clock: Option<::tokio::task::JoinHandle<()>>,
}

/// One registration a pass is about to check: where it is stored, and what it stands for.
#[derive(Debug, Clone)]
struct Checked {
    /// The target group holding the registration.
    target_group_arn: String,
    /// The registration's key in that group.
    key: TargetKey,
    /// The group's `TargetType`, which says what the id names.
    target_type: String,
    /// The VPC the group's targets live in, which is the inventory a check reads.
    vpc_id: Option<String>,
    /// The registered target's id: an instance id, an address, or a function ARN.
    id: String,
    /// Whether another emulated service's scheduler owns this registration.
    scheduled: bool,
    /// Whether the workload behind the registration listens on the port this check
    /// CONNECTS to — the group's effective health-check port, which is `HealthCheckPort`
    /// when it names one and the port the target receives traffic on otherwise
    /// (`TargetGroupRecord::health_check_port_of`).
    ///
    /// `None` when that is not known: a registration whose workload declares no ports
    /// ([`super::ServedPorts`]), or a health check whose port no number names.
    serves_health_check_port: Option<bool>,
    /// The port this check CONNECTS to, as a number — the same effective health-check port,
    /// which is what the target's own inbound rules are read against
    /// ([`super::health_check_firewall`]). `None` for a health check whose port no number
    /// names.
    health_check_port: Option<i32>,
    /// The `GroupId` of every security group GOVERNING this registration's address, as the
    /// scheduler that runs the workload behind it declares them
    /// ([`super::GuardingGroups`]). Empty for a registration nothing declares them for,
    /// whose groups are read off the network interface holding its address instead.
    ///
    /// The scheduler's own SHARED list, never copied: a pass checks up to
    /// [`MAX_CHECKS_PER_PASS`] registrations, so one allocation per registration would make
    /// the pass cost the list's length times that cap.
    guarding_group_ids: GuardingGroups,
    /// The `GroupId` of every security group the load balancers forwarding to this group are
    /// associated with: where the check arrives FROM, as an inbound rule of the target can
    /// name it.
    source_group_ids: Vec<String>,
    /// The `NetworkInterfaceId` of every own interface those load balancers hold, which is
    /// what the addresses their checks come from are read off.
    source_interface_ids: Vec<String>,
    /// Whether the load balancers forwarding to this group were READ out of this store, so
    /// [`Checked::source_group_ids`] is what they are associated with rather than what this
    /// pass failed to resolve — the difference between a load balancer in no security group
    /// and one whose association is unknown ([`super::health_check_firewall::CheckSource`]).
    source_groups_resolved: bool,
}

/// What one check observed about a registration: `Some(true)` for one something in this
/// emulator stands behind, `Some(false)` for one nothing does, and `None` when nothing was
/// observed at all.
type Observation = Option<bool>;

/// Run ONE check of exactly the registrations `registrations` names — each a
/// `(target group ARN, registration key)` of this store — and fold what each check observed
/// into that registration's own probe.
///
/// What [`drive_host_count_metrics`] does on the writes this service's OWN API makes, for a
/// registration ANOTHER emulated service's scheduler has just created through the state seam
/// (`super::TargetGroupRecord::register_for`): the first check of a registration is owed by
/// the write that created it, so the verdict its checks reach does not wait for a clock
/// wake-up and a registration whose checks fail is never reported healthy on the way to one.
/// The periodic checking is the clock's, which the create of the listener that routes to the
/// group started.
///
/// NOTHING else is touched: no other registration is observed, no series is opened or closed
/// and no datapoint is published, so a call made while materialising one new registration
/// cannot advance any other registration's probe or any series' schedule. A registration of a
/// target group no load balancer forwards to is checked by nothing and is skipped here too.
///
/// Called with NO guard of this store held: it takes the lock itself.
pub fn check_registrations<Eng: Send + Sync + 'static>(
    engine: &Eng,
    registrations: &[(String, TargetKey)],
) {
    if registrations.is_empty() {
        return;
    }
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let checked = {
        let store = state.lock();
        named_registrations(&store, registrations)
    };
    let observations = observe_all(engine, &checked);
    fold_observations(&mut state.lock(), &observations, SystemTime::now());
}

/// Publish every host-count datapoint the clock has already made due, and start the one
/// bounded in-process task that waits for the next one.
///
/// Called by the writes that change WHAT a load balancer routes to or WHICH targets are
/// registered with a group it routes to — never by a read: the checks this materialises are
/// the ones the stored due instants have already made due, so what a caller sees does not
/// depend on whether anybody called.
///
/// With no async runtime to spawn onto there is no clock, and the checks due at the moment of
/// the write are then the whole series until the next write — the router always runs on one.
pub fn drive_host_count_metrics<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) {
    publish_due_checks(&**engine);
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
        // `true` only if the wake-up cap is what ended the loop; a clock that ran out of
        // work has already released its claim inside the one critical section that observed
        // it.
        let mut capped = true;
        for _ in 0..MAX_CLOCK_WAKE_UPS {
            let Some(wait) = next_wait_or_release(&mut state.lock(), generation) else {
                capped = false;
                break;
            };
            // Every wake-up waits, even for an instant already past: that is what keeps a
            // due check from costing a CPU-bound spin, and it hands the runtime back
            // between wake-ups.
            ::tokio::time::sleep(wait).await;
            publish_due_checks(&*engine);
        }
        if capped {
            // Only THIS clock's claim, so a clock started since keeps its own.
            release_clock(&mut state.lock(), generation);
        }
    });
    set_clock(&mut claimed.lock(), generation, handle);
}

/// Keep the running clock's task handle, aborting the one it replaces — and only while
/// `generation` is still the CURRENT claim, so a starter whose own task has already stood
/// down installs nothing and aborts nothing.
fn set_clock(store: &mut ElbState, generation: u64, handle: ::tokio::task::JoinHandle<()>) {
    if store.host_counts.clock_generation != generation {
        handle.abort();
        return;
    }
    if let Some(previous) = store.host_counts.clock.replace(handle) {
        previous.abort();
    }
}

/// Claim the single publisher clock for this store: the claim's GENERATION when the caller
/// may start one, `None` when one already runs.
fn claim_clock(store: &mut ElbState) -> Option<u64> {
    if store.host_counts.clock_running {
        return None;
    }
    store.host_counts.clock_running = true;
    store.host_counts.clock_generation = store.host_counts.clock_generation.wrapping_add(1);
    Some(store.host_counts.clock_generation)
}

/// Release the claim `generation` took — and ONLY that one, so a clock that ended on the
/// wake-up cap cannot clear a claim a clock started since is holding.
fn release_clock(store: &mut ElbState, generation: u64) {
    if store.host_counts.clock_generation == generation {
        store.host_counts.clock_running = false;
    }
}

/// How long the clock waits before its next pass, or `None` — having RELEASED its claim —
/// when the store holds no series to publish.
///
/// The earliest thing the stored due instants owe: the next datapoint of a series, and the
/// next CHECK of a registration a load balancer of this store health-checks
/// ([`earliest_check_due`]) — which is the one that decides how long a registration keeps its
/// current verdict, so the clock wakes for it rather than for the publish schedule alone.
///
/// Observing "nothing is owed" and standing down happen in ONE critical section, which is
/// what makes the hand-over race-free: a write holds this same lock to store the routing
/// BEFORE it tries to claim the clock, so either this call sees that series and the running
/// clock keeps it, or the write's claim finds the flag cleared and starts a clock of its own.
fn next_wait_or_release(store: &mut ElbState, generation: u64) -> Option<Duration> {
    let now = Instant::now();
    let earliest = store
        .host_counts
        .series
        .values()
        .map(|series| series.next_datapoint_at)
        .min();
    // The next CHECK of a registration, which is a different schedule from the next datapoint
    // of the series it is counted into: a verdict is a number of checks away, each one
    // `HealthCheckIntervalSeconds` after the last, so the clock wakes for a check that is due
    // however far off that datapoint is — otherwise a verdict the cadence has already reached
    // would wait for the publish schedule.
    let next_check = earliest_check_due(store, SystemTime::now());
    match earliest {
        Some(due) => {
            let wait = due.saturating_duration_since(now);
            let wait = next_check.map_or(wait, |next_check| wait.min(next_check));
            Some(wait.clamp(MIN_PUBLISH_WAIT, MAX_PUBLISH_WAIT))
        }
        None => {
            release_clock(store, generation);
            None
        }
    }
}

/// When the next health check any load balancer of this store owes comes due, as the span
/// from `now` ([`super::TargetGroupRecord::next_check_in`]).
///
/// Only the groups a load balancer FORWARDS to: nothing checks the registrations of a group
/// nothing routes to, so none of them is a check this clock owes.
fn earliest_check_due(store: &ElbState, now: SystemTime) -> Option<Duration> {
    store
        .target_groups
        .values()
        .filter(|group| !group.load_balancer_arns.is_empty())
        .filter_map(|group| group.next_check_in(now))
        .min()
}

/// Check every registration of a routed target group whose own next check is due, fold what
/// those checks observed into the store, and publish the datapoint each due series is
/// reported as.
///
/// Four steps, so no store guard is held over another service's lock and the stores are never
/// locked at once:
///
/// 1. under THIS service's guard, open a series for every pair that is routed and close the
///    ones that are not, and snapshot the registrations to check;
/// 2. with NO guard held, read what the account's EC2 inventory holds and compute every
///    observation;
/// 3. under this service's guard again, fold the observations into the registrations and take
///    the datapoints the due series are published as;
/// 4. with the metric store's guard, append them and make the alarms over them due.
fn publish_due_checks<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let now = SystemTime::now();
    let instant = Instant::now();
    let checked = {
        let mut store = state.lock();
        sync_series(&mut store, now, instant);
        registrations_to_check(&store)
    };
    let observations = observe_all(engine, &checked);
    let published = {
        let mut store = state.lock();
        fold_observations(&mut store, &observations, now);
        due_datapoints(&mut store, now, instant)
    };
    append_datapoints(engine, published, now);
}

/// Open a series for every `(load balancer, target group)` pair the store now routes, and
/// close the series of every pair it no longer does.
///
/// A series opens with its run-up already behind it ([`RUN_UP_PERIODS`]) and its first
/// datapoint due at once, so the write that started the routing publishes the series' first
/// datapoints itself.
fn sync_series(store: &mut ElbState, now: SystemTime, instant: Instant) {
    let mut routed: HashSet<(String, String)> = HashSet::new();
    for (target_group_arn, group) in &store.target_groups {
        for load_balancer_arn in &group.load_balancer_arns {
            if store.load_balancers.contains_key(load_balancer_arn) {
                routed.insert((load_balancer_arn.clone(), target_group_arn.clone()));
            }
        }
    }
    store
        .host_counts
        .series
        .retain(|pair, _| routed.contains(pair));
    let run_up = run_up();
    for pair in routed {
        store
            .host_counts
            .series
            .entry(pair)
            .or_insert_with(|| SeriesClock {
                next_stamp: now.checked_sub(run_up).unwrap_or(now),
                next_datapoint_at: instant,
                published: false,
            });
    }
}

/// How far back the stamp of a series' FIRST datapoint is: the run-up it opens with
/// ([`RUN_UP_PERIODS`] periods of [`PUBLISH_INTERVAL`] each), which is the same span a series
/// that has reported nothing yet keeps ([`hold`]).
fn run_up() -> Duration {
    PUBLISH_INTERVAL.saturating_mul(RUN_UP_PERIODS)
}

/// Every registration of a target group some load balancer of this store routes to, up to
/// [`MAX_CHECKS_PER_PASS`].
///
/// Every one of them is LOOKED at; whether that look counts as one of the group's own
/// scheduled checks is decided per registration, against the cadence it is checked at
/// ([`super::TargetGroupRecord::observed_check`]).
fn registrations_to_check(store: &ElbState) -> Vec<Checked> {
    let mut checked: Vec<Checked> = Vec::new();
    for (target_group_arn, group) in &store.target_groups {
        if group.load_balancer_arns.is_empty() {
            continue;
        }
        for (key, target) in &group.targets {
            if checked.len() >= MAX_CHECKS_PER_PASS {
                return checked;
            }
            checked.push(checked_registration(
                store,
                target_group_arn,
                group,
                key,
                target,
            ));
        }
    }
    checked
}

/// The registrations `registrations` names, as the looks at them this store would take: one
/// per `(target group ARN, key)` that is still held by a target group a load balancer
/// forwards to, and up to [`MAX_CHECKS_PER_PASS`] of them.
///
/// A pair naming a group or a registration this store no longer holds contributes nothing: a
/// deregistration may have taken it out since the caller read it.
fn named_registrations(store: &ElbState, registrations: &[(String, TargetKey)]) -> Vec<Checked> {
    registrations
        .iter()
        .take(MAX_CHECKS_PER_PASS)
        .filter_map(|(target_group_arn, key)| {
            let group = store.target_groups.get(target_group_arn)?;
            if group.load_balancer_arns.is_empty() {
                return None;
            }
            let target = group.targets.get(key)?;
            Some(checked_registration(
                store,
                target_group_arn,
                group,
                key,
                target,
            ))
        })
        .collect()
}

/// What one check of the registration `key` of `group` reads: where it is stored, what it
/// stands for, the port the check connects to, and the load balancer it arrives from.
fn checked_registration(
    store: &ElbState,
    target_group_arn: &str,
    group: &TargetGroupRecord,
    key: &TargetKey,
    target: &RegisteredTarget,
) -> Checked {
    let health_check_port = group
        .health_check_port_of(target)
        .and_then(|port| port.parse::<i32>().ok());
    // The load balancers that FORWARD to this group are the ones checking its registrations,
    // so they are where a check of this target arrives from.
    let mut source_group_ids: Vec<String> = Vec::new();
    let mut source_interface_ids: Vec<String> = Vec::new();
    let mut source_groups_resolved = false;
    for load_balancer in group
        .load_balancer_arns
        .iter()
        .filter_map(|arn| store.load_balancers.get(arn))
    {
        // The record answered which groups this load balancer is associated with — none of
        // them, where it carries none.
        source_groups_resolved = true;
        source_group_ids.extend(load_balancer.security_groups.iter().cloned());
        source_interface_ids.extend(load_balancer.network_interface_ids.iter().cloned());
    }
    Checked {
        target_group_arn: target_group_arn.to_string(),
        key: key.clone(),
        target_type: group.target_type.clone(),
        vpc_id: group.vpc_id.clone(),
        id: target.id.clone(),
        scheduled: target.registered_for.is_some(),
        // The port this group's check connects to for THIS registration, read against the
        // ports the workload behind it serves.
        serves_health_check_port: health_check_port.and_then(|port| target.serves(port)),
        health_check_port,
        guarding_group_ids: GuardingGroups::clone(&target.guarded_by),
        source_group_ids,
        source_interface_ids,
        source_groups_resolved,
    }
}

/// What one check of each registration in `checked` observed, one entry per registration the
/// pass CHECKED — including the ones nothing could be observed about, because the check still
/// ran and the registration's next one is therefore an interval away.
///
/// The account's EC2 inventory is read ONCE for the whole pass — the VPCs it holds, the
/// addresses its network interfaces hold and the instances it reports running — so the cost
/// of a pass does not grow with the number of registrations times the size of the inventory.
fn observe_all<Eng: Send + Sync + 'static>(
    engine: &Eng,
    checked: &[Checked],
) -> Vec<(String, TargetKey, Observation)> {
    if checked.is_empty() {
        return Vec::new();
    }
    let inventory = ec2::service_state(engine);
    let held: Option<Inventory> = inventory.as_ref().map(|state| {
        let store = state.lock();
        let mut interface_groups: HashMap<String, Vec<String>> = HashMap::new();
        let mut interface_addresses: HashMap<String, Vec<String>> = HashMap::new();
        let mut instance_groups: HashMap<String, Vec<String>> = HashMap::new();
        for interface in store.network_interfaces.values() {
            let addresses = interface_addresses_of(interface);
            let groups = interface_group_ids(interface);
            if let Some(id) = interface.network_interface_id.clone() {
                interface_addresses.insert(id, addresses.clone());
            }
            // An instance's traffic arrives on the interfaces ATTACHED to it, so the groups
            // of every one of them govern traffic to that instance — one interface of it
            // admitting a connection is enough, exactly as one group of an interface is.
            if let Some(instance_id) = interface
                .attachment
                .as_ref()
                .and_then(|attachment| attachment.instance_id.clone())
            {
                instance_groups
                    .entry(instance_id)
                    .or_default()
                    .extend(groups.iter().cloned());
            }
            for address in addresses {
                // An address is held by one interface, so the groups of the interface holding
                // it are the groups governing traffic to it.
                interface_groups.entry(address).or_insert_with(|| groups.clone());
            }
        }
        Inventory {
            vpcs: store.vpcs.keys().cloned().collect(),
            addresses: store
                .network_interfaces
                .values()
                .flat_map(interface_addresses_of)
                .collect(),
            running: store
                .instances
                .values()
                .filter(|instance| instance_is_running(instance))
                .map(|instance| instance.instance_id.clone())
                .collect(),
            interface_groups,
            interface_addresses,
            instance_groups,
            ingress: store
                .security_groups
                .iter()
                .map(|(group_id, group)| {
                    (
                        group_id.clone(),
                        group.ip_permissions.clone().unwrap_or_default(),
                    )
                })
                .collect(),
            group_names: store
                .security_groups
                .iter()
                .filter_map(|(group_id, group)| {
                    Some((group_id.clone(), group.group_name.clone()?))
                })
                .collect(),
        }
    });
    checked
        .iter()
        .map(|target| {
            (
                target.target_group_arn.clone(),
                target.key.clone(),
                observe(target, held.as_ref()),
            )
        })
        .collect()
}

/// What the account's EC2 inventory holds that a health check reads: the VPCs of the
/// account, the addresses its network interfaces hold, the instances it reports running, and
/// the security groups governing what may reach those interfaces.
#[derive(Debug, Default)]
struct Inventory {
    /// Every `VpcId` the inventory holds.
    vpcs: HashSet<String>,
    /// Every private IPv4 and IPv6 address a network interface of the account holds.
    addresses: HashSet<String>,
    /// Every instance the inventory reports running.
    running: HashSet<String>,
    /// The security groups of the interface holding each address, keyed by that address:
    /// the rules that decide whether a check of a target at it may arrive.
    interface_groups: HashMap<String, Vec<String>>,
    /// The addresses each network interface holds, keyed by `NetworkInterfaceId` — read for
    /// the load balancer's OWN interfaces, which is where its checks come from.
    interface_addresses: HashMap<String, Vec<String>>,
    /// The security groups of the network interfaces ATTACHED TO each instance, keyed by
    /// `InstanceId`: the rules that decide whether a check of a target registered as that
    /// instance may arrive. An instance absent from this is one whose groups are not known.
    instance_groups: HashMap<String, Vec<String>>,
    /// The INBOUND rules of each security group of the account, keyed by `GroupId`. A group
    /// absent from this is one whose rules are not known.
    ingress: HashMap<String, Vec<ec2_types::IpPermission>>,
    /// The `GroupName` of each of those groups, for an inbound rule that names its source
    /// group by name rather than by id.
    group_names: HashMap<String, String>,
}

/// The `TargetType` whose registrations name an INSTANCE of the account.
const INSTANCE_TARGET_TYPE: &str = "instance";

/// The `TargetType` whose registrations name an ADDRESS
/// (`crate::ops::types::TARGET_TYPE_ENUM_VALUES` carries the whole catalog).
const IP_TARGET_TYPE: &str = "ip";

impl Inventory {
    /// The security groups GOVERNING traffic to what one registration names, as the account's
    /// inventory reports them — `None` when the inventory reports none, which decides nothing.
    ///
    /// A security group is associated with a network INTERFACE, so the groups governing a
    /// registration are those of the interfaces belonging to the resource it names: an
    /// `instance` registration names the instance, whose traffic arrives on the interfaces
    /// attached to it, and every other registration names an address, which one interface
    /// holds. Reading them off the resource the registration NAMES is what makes a target
    /// registered as an instance governed by the same rules as the same workload registered
    /// by its address.
    fn governing_groups(&self, target: &Checked) -> Option<&[String]> {
        match target.target_type.as_str() {
            INSTANCE_TARGET_TYPE => self.instance_groups.get(&target.id),
            _ => self.interface_groups.get(&target.id),
        }
        .map(Vec::as_slice)
    }
}

/// What one check of `target` observed, given what the account's EC2 inventory holds — see
/// the module docs.
///
/// A check reads the inventory of the VPC the target group belongs to, so an inventory
/// holding no such VPC is one nothing can be observed in: a verdict is never assumed from
/// silence, which is how this service reads every other fact it does not own
/// ([`super::network`]).
fn observe(target: &Checked, held: Option<&Inventory>) -> Observation {
    if target.serves_health_check_port == Some(false) {
        // The check connects to the group's effective health-check port, and the workload
        // behind this registration does not listen on it: nothing answers there, whatever
        // else stands behind the registration. This is the one condition a check can
        // observe about the PORT, and it is read before what stands behind the target,
        // because a workload that is running answers nothing on a port it does not serve.
        return Some(false);
    }
    if held.is_some_and(|held| health_check_is_blocked(target, held)) {
        // The check is a connection to the target's own address on that port, and the
        // security groups governing that address admit no such connection from this load
        // balancer: nothing answers a check that cannot arrive, whatever stands behind the
        // registration ([`super::health_check_firewall`]). Read before what stands behind
        // the target, for the same reason the port is.
        return Some(false);
    }
    if target.scheduled {
        // The workload another service's scheduler registered this target for runs in this
        // emulator, and that is what stands behind the registration.
        return Some(true);
    }
    let held = held?;
    if !held.vpcs.contains(target.vpc_id.as_deref()?) {
        return None;
    }
    match target.target_type.as_str() {
        IP_TARGET_TYPE => Some(held.addresses.contains(&target.id)),
        INSTANCE_TARGET_TYPE => Some(held.running.contains(&target.id)),
        // A `lambda` target is a function of another service, and a target type this
        // service does not read is one no check here observes anything about.
        _ => None,
    }
}

/// Whether the security groups governing what this registration names are known to admit NO
/// health check of it from the load balancers that check it.
///
/// `false` whenever the question is not decided — a target whose groups are neither declared
/// nor readable off the account's inventory, a check whose port no number names, an interface
/// in no security group, a group whose rules are not held, or a load balancer whose own groups
/// and addresses are unknown. Nothing is failed on silence
/// ([`super::health_check_firewall`]).
fn health_check_is_blocked(target: &Checked, held: &Inventory) -> bool {
    let Some(port) = target.health_check_port else {
        return false;
    };
    // The groups the scheduler running the workload DECLARED, else the ones the account's
    // inventory reports for what this registration names: either way they are the groups
    // governing traffic to it.
    let groups = if target.guarding_group_ids.is_empty() {
        let Some(groups) = held.governing_groups(target) else {
            return false;
        };
        groups
    } else {
        target.guarding_group_ids.as_ref()
    };
    let source = super::health_check_firewall::CheckSource {
        group_ids: target.source_group_ids.clone(),
        group_names: target
            .source_group_ids
            .iter()
            .filter_map(|group_id| held.group_names.get(group_id).cloned())
            .collect(),
        addresses: target
            .source_interface_ids
            .iter()
            .filter_map(|interface_id| held.interface_addresses.get(interface_id))
            .flatten()
            .cloned()
            .collect(),
        groups_resolved: target.source_groups_resolved,
    };
    super::health_check_firewall::interface_admits_check(groups, &held.ingress, port, &source)
        == Some(false)
}

/// The `GroupId` of every security group one network interface is associated with.
fn interface_group_ids(interface: &ec2_types::NetworkInterface) -> Vec<String> {
    interface
        .groups
        .iter()
        .flatten()
        .filter_map(|group| group.group_id.clone())
        .collect()
}

/// Every private IPv4 and IPv6 address one network interface holds.
fn interface_addresses_of(interface: &ec2_types::NetworkInterface) -> Vec<String> {
    let private = interface
        .private_ip_addresses
        .iter()
        .flatten()
        .filter_map(|address| address.private_ip_address.clone());
    let ipv6 = interface
        .ipv6_addresses
        .iter()
        .flatten()
        .filter_map(|address| address.ipv6_address.clone());
    interface
        .private_ip_address
        .clone()
        .into_iter()
        .chain(private)
        .chain(ipv6)
        .collect()
}

/// Whether EC2 reports this instance as one that is running, which is the state an instance
/// serving traffic is in.
fn instance_is_running(instance: &ec2::InstanceRecord) -> bool {
    instance.state == ec2::InstanceStateName::Running
}

/// Fold what the checks of this pass observed into the registrations they were about.
///
/// A registration the store no longer holds is skipped: the pass ran with no guard held, so a
/// deregistration may have taken it out in the meantime. Whether a look counts as one of the
/// group's own scheduled checks, and what an observation of nothing leaves behind, are decided
/// per registration against the cadence it is checked at
/// ([`super::TargetGroupRecord::observed_check`]).
fn fold_observations(
    store: &mut ElbState,
    observations: &[(String, TargetKey, Observation)],
    now: SystemTime,
) {
    for (target_group_arn, key, observed) in observations {
        let Some(group) = store.target_groups.get_mut(target_group_arn) else {
            continue;
        };
        group.observed_check(key, *observed, now);
    }
}

/// Take the datapoints every series that is due at `instant` is published as, and advance
/// each of those series to its next check.
///
/// A series whose target group has no count to report yet publishes no datapoint and keeps
/// its place in the run-up instead ([`hold`]), so the counts a client reads are the verdicts
/// the group's checks reached.
fn due_datapoints(
    store: &mut ElbState,
    now: SystemTime,
    instant: Instant,
) -> Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> {
    let due: Vec<(String, String)> = store
        .host_counts
        .series
        .iter()
        .filter(|(_, series)| instant >= series.next_datapoint_at)
        .map(|(pair, _)| pair.clone())
        .collect();
    let mut published: Vec<(cloudwatch::MetricKey, cloudwatch::StoredDatum)> =
        Vec::with_capacity(due.len() * 2);
    for pair in due {
        let (load_balancer_arn, target_group_arn) = &pair;
        let Some(load_balancer) = store.load_balancers.get(load_balancer_arn) else {
            continue;
        };
        let (metric_namespace, load_balancer_value) = (
            namespace(&load_balancer.lb_type),
            load_balancer_dimension_value(load_balancer),
        );
        let Some(target_group_value) = target_group_dimension_value(target_group_arn) else {
            continue;
        };
        let Some(group) = store.target_groups.get_mut(target_group_arn) else {
            continue;
        };
        // What the clock already owes the group: a registration whose draining span is over
        // has left it, so it is counted in neither host count.
        group.settle_draining(now);
        let reported = group.reported_host_counts(now);
        // What can make an undetermined count determined: the next check of the group.
        let next_check = group.next_check_in(now);
        let Some(series) = store.host_counts.series.get_mut(&pair) else {
            continue;
        };
        let Some((healthy, unhealthy)) = reported else {
            hold(series, now, instant, next_check);
            continue;
        };
        let stamp = advance(series);
        for (metric_name, count) in [
            (HEALTHY_HOST_COUNT, healthy),
            (UNHEALTHY_HOST_COUNT, unhealthy),
        ] {
            let dimensions: BTreeMap<String, String> = [
                (
                    TARGET_GROUP_DIMENSION.to_string(),
                    target_group_value.clone(),
                ),
                (
                    LOAD_BALANCER_DIMENSION.to_string(),
                    load_balancer_value.clone(),
                ),
            ]
            .into_iter()
            .collect();
            published.push((
                cloudwatch::MetricKey {
                    namespace: metric_namespace.to_string(),
                    metric_name: metric_name.to_string(),
                    dimensions,
                },
                cloudwatch::StoredDatum {
                    timestamp_secs: epoch_secs(stamp),
                    // One observation: this check's own count of the group's targets.
                    sample_count: 1.0,
                    sum: count,
                    minimum: count,
                    maximum: count,
                    unit: Some(COUNT_UNIT.to_string()),
                    storage_resolution: STORAGE_RESOLUTION_SECS,
                },
            ));
        }
    }
    published
}

/// Leave a due datapoint UNPUBLISHED because the load balancer has determined no counts for
/// the group yet, and bring the series due again when the next check of it could determine
/// them.
///
/// A series that has published NOTHING keeps its run-up rather than its place in the schedule:
/// the periods it reports once a verdict is in are the ones before THAT instant — the same
/// run-up a series opens with ([`RUN_UP_PERIODS`]), which is what gives an alarm over it the
/// evaluation periods it asks for. One that has already published keeps the stamp its schedule
/// has reached, so no period it has already reported is stamped a second time.
///
/// `next_check` is the span to the next check of the group, and a group owing none is looked at
/// again one publish interval later.
fn hold(
    series: &mut SeriesClock,
    now: SystemTime,
    instant: Instant,
    next_check: Option<Duration>,
) {
    if !series.published {
        series.next_stamp = now.checked_sub(run_up()).unwrap_or(now);
    }
    let wait = next_check
        .unwrap_or(PUBLISH_INTERVAL)
        .clamp(MIN_PUBLISH_WAIT, PUBLISH_INTERVAL);
    series.next_datapoint_at = instant.checked_add(wait).unwrap_or(instant);
}

/// Take the time stamp of the datapoint that is due and schedule the next one.
fn advance(series: &mut SeriesClock) -> SystemTime {
    let stamp = series.next_stamp;
    series.published = true;
    series.next_stamp = stamp.checked_add(PUBLISH_INTERVAL).unwrap_or(stamp);
    // While the series is still behind the present it is published at the accelerated
    // spacing; once it has caught up, the next datapoint is due when the clock reaches its
    // stamp, which is one publish interval apart. Never longer than one interval: that is
    // the most the schedule can owe, so a clock that has stepped backwards costs one slow
    // datapoint rather than an unbounded wait.
    let wait = series
        .next_stamp
        .duration_since(SystemTime::now())
        .unwrap_or(ACCELERATED_SPACING)
        .min(PUBLISH_INTERVAL);
    series.next_datapoint_at = Instant::now()
        .checked_add(wait)
        .unwrap_or_else(Instant::now);
    stamp
}

/// Append the datapoints this pass published to the metric store, and make every alarm over
/// the series they belong to due for evaluation.
///
/// Nothing is published on an engine that holds no metric store to reach: a datapoint has one
/// home, and this path never makes a second one. An alarm is evaluated FROM these datapoints,
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
            // The metric store holds no more series. The routing itself is already stored,
            // so nothing is failed for it; the dropped count is reported where a developer
            // can see it.
            ::tracing::warn!(
                metric = watched.metric_name.as_str(),
                "the CloudWatch metric store is at capacity, so this host count was dropped"
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

/// `at` as the epoch seconds a stored datapoint carries, which is the unit the metric store's
/// own readers window by. An instant before the epoch (which no clock reports) reads as the
/// epoch itself rather than failing the publish.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

impl super::TargetGroupRecord {
    /// How many of this group's registrations the load balancer considers healthy, and how
    /// many it considers unhealthy, at `now`.
    ///
    /// Counted off the very health [`super::TargetGroupRecord::health_of`] reports, so the
    /// series and `DescribeTargetHealth` cannot disagree about one registration. A
    /// registration in neither state — one still coming up, draining, or in a group nothing
    /// checks — is counted in neither total, which is what "considered healthy" and
    /// "considered unhealthy" leave out.
    #[must_use]
    pub fn host_counts(&self, now: SystemTime) -> (f64, f64) {
        let mut healthy = 0u32;
        let mut unhealthy = 0u32;
        for target in self.targets.values() {
            match self.health_of(target, now).state.as_deref() {
                Some(HEALTHY_STATE) => healthy += 1,
                Some(UNHEALTHY_STATE) => unhealthy += 1,
                _ => {}
            }
        }
        (f64::from(healthy), f64::from(unhealthy))
    }

    /// The counts the load balancer REPORTS for this group at `now` — `None` while its checks
    /// have determined the health of NO registration it holds.
    ///
    /// A host count is a number of targets the load balancer CONSIDERS healthy or unhealthy,
    /// so a registration whose health its checks have not determined yet — the `initial` the
    /// model words as the load balancer still sending "the minimum number of health checks
    /// required to determine its health status" — belongs in neither total
    /// ([`Self::host_counts`]). While that is true of EVERY registration of the group, the two
    /// totals report nothing the checks established: a count of no unhealthy target among
    /// registrations the load balancer has found nothing about yet is a verdict no check
    /// reached, so there is nothing to report for such a group and its series begins at the
    /// first verdict the checks did reach ([`hold`]).
    ///
    /// A group some of whose registrations ARE decided reports the counts it has — the
    /// undetermined ones are simply in neither total, which is what both counts leave out —
    /// so one registration coming up never silences the series of a group the load balancer is
    /// already counting. A group holding no registration at all likewise reports the counts it
    /// has: no healthy target and no unhealthy one.
    #[must_use]
    pub fn reported_host_counts(&self, now: SystemTime) -> Option<(f64, f64)> {
        let nothing_determined = !self.targets.is_empty()
            && self.targets.values().all(|target| {
                self.health_of(target, now).state.as_deref() == Some(INITIAL_STATE)
            });
        if nothing_determined {
            return None;
        }
        Some(self.host_counts(now))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A routed target group whose health checks are ON, checked at [`PROBE_INTERVAL`] with a
    /// threshold of [`PROBE_THRESHOLD`] consecutive checks to either verdict.
    fn checked_group() -> TargetGroupRecord {
        TargetGroupRecord {
            protocol: Some("HTTP".to_string()),
            target_type: "ip".to_string(),
            port: Some(80),
            load_balancer_arns: vec!["an-attached-load-balancer".to_string()],
            health_check: super::super::HealthCheckConfig {
                enabled: true,
                interval_seconds: i32::try_from(PROBE_INTERVAL.as_secs()).unwrap_or(5),
                healthy_threshold: PROBE_THRESHOLD,
                unhealthy_threshold: PROBE_THRESHOLD,
                ..super::super::HealthCheckConfig::default()
            },
            ..TargetGroupRecord::default()
        }
    }

    /// The cadence and threshold the rows below configure: the shortest interval
    /// `HealthCheckIntervalSeconds` admits, and the `UnhealthyThresholdCount` a target group
    /// defaults to.
    const PROBE_INTERVAL: Duration = Duration::from_secs(5);
    const PROBE_THRESHOLD: i32 = 2;

    /// Register `id` with `group` as a `RegisterTargets` caller's own registration, made at
    /// `at`: its settling span and its first check run from there.
    fn registered(group: &mut TargetGroupRecord, id: &str, at: SystemTime) {
        group.targets.insert(
            (id.to_string(), group.port),
            RegisteredTarget {
                id: id.to_string(),
                port: group.port,
                availability_zone: None,
                registered_at: at,
                healthy_at: at + super::super::SETTLE_AFTER,
                draining_until: None,
                checks_due_at: at + PROBE_INTERVAL,
                probe: Default::default(),
                registered_for: None,
                serves_ports: super::super::ServedPorts::from(Vec::new()),
                guarded_by: GuardingGroups::from(Vec::new()),
            },
        );
    }

    /// RULE: a load balancer reports a host count for a target group once its checks have
    /// determined the health of at least one registration it holds — while EVERY one of them
    /// is still `initial` there is nothing to report at all — and a group holding none reports
    /// the counts it has.
    ///
    /// The rows span what the checks observed (nothing answering, an answer, nothing at all),
    /// how far through the threshold of checks the registrations are, how many registrations
    /// the group holds, and whether one of them was registered after the others were already
    /// decided. The clock each row reads at is PASSED IN, so no row waits for one.
    #[test]
    fn a_group_reports_the_counts_its_checks_determined_and_nothing_sooner() {
        let start = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        for (case, registrations, observed, checks, late, expected) in [
            (
                "a registration no check has looked at yet",
                1,
                None,
                0,
                false,
                None,
            ),
            (
                "one failing check of the two a verdict takes",
                1,
                Some(false),
                1,
                false,
                None,
            ),
            (
                "the threshold of failing checks",
                1,
                Some(false),
                2,
                false,
                Some((0.0, 1.0)),
            ),
            (
                "two registrations past that threshold",
                2,
                Some(false),
                2,
                false,
                Some((0.0, 2.0)),
            ),
            (
                "a registration whose check was answered",
                1,
                Some(true),
                1,
                false,
                Some((1.0, 0.0)),
            ),
            (
                "a registration nothing was observed about",
                1,
                None,
                2,
                false,
                Some((1.0, 0.0)),
            ),
            (
                "a group holding no registration",
                0,
                None,
                0,
                false,
                Some((0.0, 0.0)),
            ),
            (
                "a registration still coming up beside a decided one",
                1,
                Some(false),
                2,
                true,
                Some((0.0, 1.0)),
            ),
            (
                "a registration still coming up beside two decided ones",
                2,
                Some(false),
                2,
                true,
                Some((0.0, 2.0)),
            ),
        ] {
            let mut group = checked_group();
            for index in 0..registrations {
                registered(&mut group, &format!("10.0.1.{}", index + 10), start);
            }
            let keys: Vec<TargetKey> = group.targets.keys().cloned().collect();
            let mut at = start;
            for _ in 0..checks {
                at += PROBE_INTERVAL;
                for key in &keys {
                    group.observed_check(key, observed, at);
                }
            }
            if late {
                // Registered once the others are decided, so its own health is still being
                // determined at the instant the counts are read.
                registered(&mut group, "10.0.1.99", at);
            }
            let reported = group.reported_host_counts(at);
            assert_eq!(
                reported, expected,
                "case {case:?}: expected the group to report {expected:?}, got {reported:?} \
                 with the registrations {:?}",
                group
                    .targets
                    .values()
                    .map(|target| group.health_of(target, at).state)
                    .collect::<Vec<Option<String>>>()
            );
        }
    }

    /// RULE: a dimension names each resource by its ARN from the resource type onwards, and
    /// the namespace follows the load balancer's FAMILY.
    ///
    /// The rows span the three families a load balancer can be of and the ARN forms a
    /// dimension is read off, including a value that names no target group at all.
    #[test]
    fn a_series_is_addressed_by_family_and_arn_suffix() {
        for (case, lb_type, expected_namespace) in [
            ("an application load balancer", "application", APPLICATION_NAMESPACE),
            ("a network load balancer", "network", NETWORK_NAMESPACE),
            ("a gateway load balancer", "gateway", GATEWAY_NAMESPACE),
            ("a type no family names", "", APPLICATION_NAMESPACE),
        ] {
            let found = namespace(lb_type);
            assert_eq!(
                found, expected_namespace,
                "case {case:?}: expected {expected_namespace}, got {found}"
            );
            let record = LoadBalancerRecord {
                name: "lb".to_string(),
                id: "0123456789abcdef".to_string(),
                lb_type: lb_type.to_string(),
                ..probe_load_balancer()
            };
            let value = load_balancer_dimension_value(&record);
            let expected = format!("{}/lb/0123456789abcdef", load_balancer_type_code(lb_type));
            assert_eq!(
                value, expected,
                "case {case:?}: expected {expected}, got {value}"
            );
        }

        for (case, arn, expected) in [
            (
                "a target group ARN",
                "arn:aws:elasticloadbalancing:us-east-1:000000000000:targetgroup/tg/abc123",
                Some("targetgroup/tg/abc123"),
            ),
            (
                "a load balancer ARN",
                "arn:aws:elasticloadbalancing:us-east-1:000000000000:loadbalancer/app/lb/abc123",
                None,
            ),
            ("a value that is no ARN", "targetgroup", None),
        ] {
            let found = target_group_dimension_value(arn);
            assert_eq!(
                found.as_deref(),
                expected,
                "case {case:?}: expected {expected:?}, got {found:?}"
            );
        }
    }

    /// A load balancer record with the members these tests do not vary.
    fn probe_load_balancer() -> LoadBalancerRecord {
        LoadBalancerRecord {
            arn: String::new(),
            id: String::new(),
            name: String::new(),
            region: "us-east-1".to_string(),
            dns_name: String::new(),
            created_time: SystemTime::UNIX_EPOCH,
            scheme: "internal".to_string(),
            vpc_id: None,
            state_code: "active".to_string(),
            active_at: SystemTime::UNIX_EPOCH,
            lb_type: "application".to_string(),
            availability_zones: Vec::new(),
            security_groups: Vec::new(),
            ip_address_type: "ipv4".to_string(),
            customer_owned_ipv4_pool: None,
            enforce_security_group_inbound_rules_on_private_link_traffic: None,
            enable_prefix_for_ipv6_source_nat: None,
            ipv4_ipam_pool_id: None,
            capacity_reservation_modified_at: None,
            network_interface_ids: Vec::new(),
            attributes: HashMap::new(),
            tags: HashMap::new(),
            listener_arns: Vec::new(),
        }
    }

    /// RULE: what one check observes is whether the emulator holds something that stands
    /// behind the registration AND answers on the port the check connects to, and nothing
    /// at all where there is no inventory to read.
    ///
    /// The rows span the target types a check reads, a registration another service's
    /// scheduler owns, whether the workload behind it listens on the health-check port, and
    /// an account whose inventory does not hold the group's VPC.
    #[test]
    fn a_check_observes_only_what_the_emulator_holds() {
        let held = Inventory {
            vpcs: HashSet::from(["vpc-0123456789abcdef0".to_string()]),
            addresses: HashSet::from(["10.0.1.7".to_string()]),
            running: HashSet::from(["i-0123456789abcdef0".to_string()]),
            ..Inventory::default()
        };
        for (case, target_type, id, scheduled, serves, expected) in [
            ("an address an interface holds", "ip", "10.0.1.7", false, None, Some(true)),
            (
                "an address no interface holds",
                "ip",
                "10.0.1.9",
                false,
                None,
                Some(false),
            ),
            (
                "a running instance",
                "instance",
                "i-0123456789abcdef0",
                false,
                None,
                Some(true),
            ),
            (
                "an instance the account does not hold",
                "instance",
                "i-00000000000000000",
                false,
                None,
                Some(false),
            ),
            (
                "a function of another service",
                "lambda",
                "arn:aws:lambda:us-east-1:000000000000:function:f",
                false,
                None,
                None,
            ),
            (
                "a registration a scheduler owns",
                "ip",
                "10.0.9.9",
                true,
                None,
                Some(true),
            ),
            (
                "a scheduler's registration whose workload listens on the checked port",
                "ip",
                "10.0.9.9",
                true,
                Some(true),
                Some(true),
            ),
            (
                "a scheduler's registration whose workload does not listen on the \
                 checked port",
                "ip",
                "10.0.9.9",
                true,
                Some(false),
                Some(false),
            ),
            (
                "an address an interface holds, checked on a port nothing behind it serves",
                "ip",
                "10.0.1.7",
                false,
                Some(false),
                Some(false),
            ),
        ] {
            let checked = Checked {
                target_group_arn: "tg".to_string(),
                key: (id.to_string(), Some(443)),
                target_type: target_type.to_string(),
                vpc_id: Some("vpc-0123456789abcdef0".to_string()),
                id: id.to_string(),
                scheduled,
                serves_health_check_port: serves,
                health_check_port: Some(443),
                guarding_group_ids: Vec::new().into(),
                source_group_ids: Vec::new(),
                source_interface_ids: Vec::new(),
                source_groups_resolved: false,
            };
            let observed = observe(&checked, Some(&held));
            assert_eq!(
                observed, expected,
                "case {case:?}: expected {expected:?}, got {observed:?}"
            );
            // With no inventory to read, and with an inventory that holds no such VPC,
            // only a scheduler's own registration is observed — and a check against a port
            // nothing serves fails whatever the inventory holds.
            let expected_unknown = if serves == Some(false) {
                Some(false)
            } else {
                scheduled.then_some(true)
            };
            let without = observe(&checked, None);
            assert_eq!(
                without, expected_unknown,
                "case {case:?} with no EC2 inventory: expected {expected_unknown:?}, got \
                 {without:?}"
            );
            let elsewhere = Checked {
                vpc_id: Some("vpc-00000000000000000".to_string()),
                ..checked.clone()
            };
            let foreign = observe(&elsewhere, Some(&held));
            assert_eq!(
                foreign, expected_unknown,
                "case {case:?} in a VPC the inventory does not hold: expected \
                 {expected_unknown:?}, got {foreign:?}"
            );
        }
    }

    /// RULE: a check that the target's own security groups do not admit on the checked port
    /// from the load balancer doing the checking FAILS, however healthy the workload behind
    /// the registration is — and a check those groups do admit is unaffected.
    ///
    /// The rows span a group that admits the check, one that admits the port from somewhere
    /// else, one that admits the load balancer on another port, one with no inbound rule at
    /// all, and an interface in no group (nothing known, so nothing failed).
    #[test]
    fn a_check_the_targets_security_groups_do_not_admit_fails() {
        let address = "10.0.1.7".to_string();
        let target_group = "sg-0000000000000000b".to_string();
        let load_balancer_group = "sg-0000000000000000a".to_string();
        let load_balancer_interface = "eni-0123456789abcdef0".to_string();
        let admits = |protocol: &str, from: i32, to: i32, source: &str| ec2_types::IpPermission {
            ip_protocol: Some(protocol.to_string()),
            from_port: Some(from),
            to_port: Some(to),
            user_id_group_pairs: Some(vec![ec2_types::UserIdGroupPair {
                group_id: Some(source.to_string()),
                ..ec2_types::UserIdGroupPair::default()
            }]),
            ..ec2_types::IpPermission::default()
        };
        for (case, groups_of_target, rules, expected) in [
            (
                "a group admitting the load balancer on the checked port",
                vec![target_group.clone()],
                vec![admits("tcp", 443, 443, &load_balancer_group)],
                Some(true),
            ),
            (
                "a group admitting the checked port from another group",
                vec![target_group.clone()],
                vec![admits("tcp", 443, 443, "sg-0000000000000000c")],
                Some(false),
            ),
            (
                "a group admitting the load balancer on another port",
                vec![target_group.clone()],
                vec![admits("tcp", 8080, 8080, &load_balancer_group)],
                Some(false),
            ),
            (
                "a group with no inbound rule at all",
                vec![target_group.clone()],
                Vec::new(),
                Some(false),
            ),
            (
                "an interface in no security group",
                Vec::new(),
                vec![admits("tcp", 443, 443, &load_balancer_group)],
                Some(true),
            ),
        ] {
            let held = Inventory {
                vpcs: HashSet::from(["vpc-0123456789abcdef0".to_string()]),
                addresses: HashSet::from([address.clone()]),
                interface_groups: HashMap::from([(address.clone(), groups_of_target.clone())]),
                interface_addresses: HashMap::from([(
                    load_balancer_interface.clone(),
                    vec!["10.0.0.5".to_string()],
                )]),
                ingress: HashMap::from([(target_group.clone(), rules)]),
                ..Inventory::default()
            };
            // The same groups reach this decision two ways: declared by the scheduler that
            // runs the workload, and read off the interface holding the address. Both are
            // the groups governing traffic to it, so both must decide the same.
            for (provenance, declared) in [
                ("declared by the scheduler", groups_of_target.clone()),
                ("read off the interface", Vec::new()),
            ] {
                let checked = Checked {
                    target_group_arn: "tg".to_string(),
                    key: (address.clone(), Some(443)),
                    target_type: "ip".to_string(),
                    vpc_id: Some("vpc-0123456789abcdef0".to_string()),
                    id: address.clone(),
                    // A registration an ECS-like scheduler owns, which is the one a running
                    // workload stands behind: the firewall still decides whether a check of
                    // it can arrive.
                    scheduled: true,
                    serves_health_check_port: Some(true),
                    health_check_port: Some(443),
                    guarding_group_ids: declared.into(),
                    source_group_ids: vec![load_balancer_group.clone()],
                    source_interface_ids: vec![load_balancer_interface.clone()],
                    // The load balancer's own record was read, which is where its groups
                    // come from.
                    source_groups_resolved: true,
                };
                let observed = observe(&checked, Some(&held));
                assert_eq!(
                    observed, expected,
                    "case {case:?} with groups {provenance}: expected {expected:?}, got \
                     {observed:?}"
                );
            }
        }
    }

    /// RULE: the groups governing a registration are the groups of the interfaces the resource
    /// it NAMES holds, so one naming an instance is decided by the groups of the interfaces
    /// attached to that instance — the same verdict as the same workload registered by the
    /// address one of those interfaces holds.
    ///
    /// The rows span a rule that admits the load balancer on the checked port, one that admits
    /// it somewhere else, and a group with no inbound rule at all; each is read through both
    /// namings.
    #[test]
    fn an_instance_registration_is_decided_by_the_groups_of_its_own_interfaces() {
        let instance = "i-0123456789abcdef0".to_string();
        let address = "10.0.1.7".to_string();
        let target_group = "sg-0000000000000000b".to_string();
        let load_balancer_group = "sg-0000000000000000a".to_string();
        let load_balancer_interface = "eni-0123456789abcdef0".to_string();
        let admits = |port: i32| ec2_types::IpPermission {
            ip_protocol: Some("tcp".to_string()),
            from_port: Some(port),
            to_port: Some(port),
            user_id_group_pairs: Some(vec![ec2_types::UserIdGroupPair {
                group_id: Some(load_balancer_group.clone()),
                ..ec2_types::UserIdGroupPair::default()
            }]),
            ..ec2_types::IpPermission::default()
        };
        for (case, rules, expected) in [
            (
                "a group admitting the load balancer on the checked port",
                vec![admits(443)],
                Some(true),
            ),
            (
                "a group admitting the load balancer on another port",
                vec![admits(8080)],
                Some(false),
            ),
            ("a group with no inbound rule at all", Vec::new(), Some(false)),
        ] {
            // One workload: an instance of the account, holding one interface, which holds
            // one address — and the groups of that interface are what governs either naming.
            let held = Inventory {
                vpcs: HashSet::from(["vpc-0123456789abcdef0".to_string()]),
                addresses: HashSet::from([address.clone()]),
                running: HashSet::from([instance.clone()]),
                interface_groups: HashMap::from([(
                    address.clone(),
                    vec![target_group.clone()],
                )]),
                instance_groups: HashMap::from([(
                    instance.clone(),
                    vec![target_group.clone()],
                )]),
                interface_addresses: HashMap::from([(
                    load_balancer_interface.clone(),
                    vec!["10.0.0.5".to_string()],
                )]),
                ingress: HashMap::from([(target_group.clone(), rules)]),
                ..Inventory::default()
            };
            for (naming, target_type, id) in [
                ("by its instance id", INSTANCE_TARGET_TYPE, &instance),
                ("by its address", IP_TARGET_TYPE, &address),
            ] {
                let checked = Checked {
                    target_group_arn: "tg".to_string(),
                    key: (id.clone(), Some(443)),
                    target_type: target_type.to_string(),
                    vpc_id: Some("vpc-0123456789abcdef0".to_string()),
                    id: id.clone(),
                    // A registration a caller made, so what stands behind it is what the
                    // account's inventory holds — and the firewall decides whether a check of
                    // it can arrive at all.
                    scheduled: false,
                    serves_health_check_port: None,
                    health_check_port: Some(443),
                    guarding_group_ids: Vec::new().into(),
                    source_group_ids: vec![load_balancer_group.clone()],
                    source_interface_ids: vec![load_balancer_interface.clone()],
                    source_groups_resolved: true,
                };
                let observed = observe(&checked, Some(&held));
                assert_eq!(
                    observed, expected,
                    "case {case:?} with the target named {naming}: expected {expected:?}, got \
                     {observed:?}"
                );
            }
        }
    }
}
