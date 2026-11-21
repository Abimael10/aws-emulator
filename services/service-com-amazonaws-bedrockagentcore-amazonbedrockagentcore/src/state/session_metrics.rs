// SPDX-License-Identifier: CC0-1.0

//! The `AWS/Bedrock-AgentCore` series the built-in tools publish about the sessions a
//! caller currently holds open.
//!
//! # What the series is
//!
//! A built-in tool session is not only the `sessionId` a start is answered with: Amazon
//! Bedrock AgentCore reports how many of them are open as the
//! [`ACTIVE_SESSION_COUNT`] metric of the [`NAMESPACE`] namespace, dimensioned by the
//! built-in tool the sessions belong to ([`SERVICE_DIMENSION`], whose values name the tool
//! — [`BuiltInTool::service_dimension`]). That series is what a session-capacity dashboard
//! plots and what every concurrency alarm is evaluated against, so a tool whose live
//! sessions are never published has no concurrency to graph and no breach to fire on.
//!
//! # What one sample says
//!
//! One sample of a tool is one reading of the SAME status the reads of a session report
//! ([`crate::state::CodeInterpreterSessionRecord::status_as_of`] and
//! [`crate::state::BrowserSessionRecord::status_as_of`]): a session counts while it is
//! still usable, and stops counting the instant a stop terminates it or its time-to-live
//! runs out. So the value is the number of sessions of that tool the caller holds open at
//! the instant it was sampled, published as a gauge — one observation, in the unit a count
//! of things is measured in ([`COUNT_UNIT`]) — which is what makes an `Average`, a
//! `Minimum` and a `Maximum` over a period agree about the concurrency that period stood
//! at.
//!
//! A tool this store has never held a session for has no series at all; a tool whose
//! sessions have all ended reports the ZERO it fell back to, which is what tells an idle
//! tool from one that was never used.
//!
//! # When a sample happens
//!
//! Two things drive the samples, and [`sample_due`] alone decides what is published:
//!
//! * [`publish_active_session_counts`], called from each operation that opens or closes a
//!   session, so a caller that has just started one sees the gauge move with it rather
//!   than up to a minute later;
//! * [`drive_session_metric_clock`], the one bounded in-process task per store, which keeps
//!   sampling while the store holds an open session. A time-to-live running out is a
//!   transition the CLOCK makes due with nobody calling, and a client watching concurrency
//!   makes no further AgentCore call before it looks for it, so without the task that
//!   movement would never reach the series. It does not change WHEN a session ends: it
//!   publishes what the stored instants have already made due, exactly as the next call
//!   would have.
//!
//! [`report_active_sessions`] is the one seam that does both, so no call site can publish
//! without also keeping the clock alive.
//!
//! # Where they go
//!
//! Into the CloudWatch emulator's OWN metric store — the very series `ListMetrics`,
//! `GetMetricStatistics`, `GetMetricData` and the alarm evaluation read — so one metric has
//! one home and a publisher cannot drift from its readers. They never travel over
//! `PutMetricData`, which reserves the namespaces the services own ("you should not specify
//! a namespace that begins with `AWS/`", `PutMetricData$Namespace`).
//!
//! Nothing here fails a request: the datapoints are published off the answer, so an engine
//! carrying no CloudWatch store, or one at capacity, drops the sample and the request is
//! answered exactly as it was.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch;

use super::{service_state, ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState};

/// The namespace Amazon Bedrock AgentCore publishes its built-in tools' own metrics under.
pub const NAMESPACE: &str = "AWS/Bedrock-AgentCore";

/// The dimension that names the built-in tool a series belongs to.
pub const SERVICE_DIMENSION: &str = "Service";

/// How many sessions of that tool the caller currently holds open.
pub const ACTIVE_SESSION_COUNT: &str = "ActiveSessionCount";

/// The unit a count of things is published in: `Count`, the `StandardUnit` value for a
/// number of things.
pub const COUNT_UNIT: &str = "Count";

/// The `StorageResolution` these datapoints are published at, in seconds: the regular
/// resolution, "which CloudWatch stores at 1-minute resolution"
/// (`MetricDatum$StorageResolution`).
const STANDARD_STORAGE_RESOLUTION_SECONDS: i32 = 60;

/// How often a tool's live sessions are sampled while nothing opens or closes one.
///
/// One minute is the resolution the datapoints are stored at
/// ([`STANDARD_STORAGE_RESOLUTION_SECONDS`]) and the granularity the finest read of them
/// can ask for, so a tool holding sessions carries a reading in every period a reader can
/// select.
pub const SESSION_SAMPLE_INTERVAL: Duration = Duration::from_secs(60);

/// How often the clock looks at the store for a sample that has come due.
///
/// Not a publishing schedule — what is published is decided by [`sample_due`] alone — only
/// how soon after a minute opens, or after a time-to-live lapses, the reading of it is
/// taken.
const SESSION_SAMPLE_TICK: Duration = Duration::from_secs(1);

/// How long a running clock's claim on the store stands before another call may start one.
///
/// The claim is a LEASE rather than a flag, renewed on every wake-up, so a clock whose task
/// ended without releasing it — one whose wake-up panicked, or that was cancelled with its
/// runtime — cannot stop the store from ever having a clock again: the lease lapses and the
/// next call starts a new one. Several ticks long, so a clock that is merely busy keeps it.
const SESSION_CLOCK_LEASE: Duration = Duration::from_secs(5);

/// How many times one clock wakes up before it stands down.
///
/// A backstop, not a schedule: it bounds the work one request can start. Whatever the clock
/// has not reached when it is spent is sampled by the next clock — every call that opens or
/// closes a session starts one if none is running — or by that call's own publish.
const MAX_SESSION_CLOCK_WAKE_UPS: usize = 3_600;

/// One built-in tool of AgentCore, as the [`SERVICE_DIMENSION`] dimension names it.
///
/// The two tools this data plane serves sessions for, each with its own session store:
/// the code interpreter ([`ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState::code_interpreters`])
/// and the browser ([`ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState::browsers`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuiltInTool {
    /// The code interpreter tool `StartCodeInterpreterSession` opens a session on.
    CodeInterpreter,
    /// The browser tool `StartBrowserSession` opens a session on.
    Browser,
}

impl BuiltInTool {
    /// Every tool whose sessions are published, so a sampler cannot forget one.
    pub const ALL: [Self; 2] = [Self::CodeInterpreter, Self::Browser];

    /// The `Service` dimension value this tool's series is dimensioned on: the name AgentCore
    /// publishes the built-in tool under.
    #[must_use]
    pub fn service_dimension(self) -> &'static str {
        match self {
            Self::CodeInterpreter => "AgentCore.CodeInterpreter",
            Self::Browser => "AgentCore.Browser",
        }
    }
}

/// The last sample of ONE tool's gauge: when it was taken and what it read.
///
/// Both halves decide whether another is due ([`sample_due`]): the minute keeps one reading
/// in every period a reader can select, and the value is what makes a change observable at
/// once rather than at the next minute.
#[derive(Debug, Clone, Copy)]
pub struct SessionSample {
    /// When this sample was taken.
    pub at: SystemTime,
    /// How many sessions of the tool were open then.
    pub open_sessions: usize,
}

/// How many sessions of each built-in tool are open at `now`, for every tool this store HOLDS
/// a session record of.
///
/// A tool no session was ever started on has no series: nothing samples a tool this account
/// has not used, and whether the CONTROL plane holds a browser or a code interpreter is NOT
/// what decides that — a resource with no session on it is no concurrency to report. A tool
/// whose sessions have all ended still holds their records, so it keeps reporting the zero its
/// gauge fell back to, which is what tells an idle tool from one that was never used.
#[must_use]
pub fn open_session_counts(
    store: &ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState,
    now: SystemTime,
) -> Vec<(BuiltInTool, usize)> {
    BuiltInTool::ALL
        .into_iter()
        .filter_map(|tool| {
            let (held, open) = match tool {
                BuiltInTool::CodeInterpreter => {
                    let sessions = store
                        .code_interpreters
                        .values()
                        .flat_map(|held| held.sessions.values());
                    count_open(sessions, |session| {
                        session.status_as_of(now) == super::CODE_INTERPRETER_SESSION_READY
                    })
                }
                BuiltInTool::Browser => {
                    let sessions = store.browsers.values().flat_map(|held| held.sessions.values());
                    count_open(sessions, |session| {
                        session.status_as_of(now) == super::BROWSER_SESSION_READY
                    })
                }
            };
            (held > 0).then_some((tool, open))
        })
        .collect()
}

/// How many sessions one tool holds records of, and how many of them `is_open` says are still
/// open — the two numbers that decide whether the tool has a series at all and what it reads.
fn count_open<S>(sessions: impl Iterator<Item = S>, is_open: impl Fn(&S) -> bool) -> (usize, usize) {
    sessions.fold((0, 0), |(held, open), session| {
        let counts = usize::from(is_open(&session));
        (held + 1, open + counts)
    })
}

/// Whether a tool last sampled as `sampled` owes another reading of `open_sessions` at
/// `now`.
///
/// A tool owes its first reading at once — a tool this store holds a session of has a
/// concurrency to report — and another whenever the minute it is read in differs from the
/// one its last reading was taken in, so each period a reader can select carries a reading.
/// Beside that cadence a CHANGE is published as it happens: the number of sessions a caller
/// holds open moved, and a client that has just opened or closed one reads the gauge
/// immediately rather than at the top of the next minute.
#[must_use]
pub fn sample_due(sampled: Option<SessionSample>, open_sessions: usize, now: SystemTime) -> bool {
    sampled.is_none_or(|last| {
        last.open_sessions != open_sessions || minute_of(last.at) != minute_of(now)
    })
}

/// Which minute `at` falls in, counted from the epoch: the slot one cadence sample is taken
/// per.
fn minute_of(at: SystemTime) -> u64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / SESSION_SAMPLE_INTERVAL.as_secs()
}

/// Publish the reading every built-in tool of this store owes, and answer whether any
/// session is still open.
///
/// Call it with NO store lock held: it takes this service's lock to read the sessions and
/// CloudWatch's to append the datapoints, one after the other. The answer is what tells the
/// clock whether there is anything left to sample — once every tool has reported the zero it
/// fell to, nothing changes again until a call opens a session.
pub fn publish_active_session_counts<Eng: Send + Sync + 'static>(engine: &Eng) -> bool {
    let Some(state) = service_state(engine) else {
        return false;
    };
    let now = SystemTime::now();
    let (due, any_open) = {
        let mut store = state.lock();
        let counts = open_session_counts(&store, now);
        let any_open = counts.iter().any(|(_, open)| *open > 0);
        let mut due: Vec<(BuiltInTool, usize)> = Vec::new();
        for (tool, open) in counts {
            let dimension = tool.service_dimension();
            if !sample_due(store.session_samples.get(dimension).copied(), open, now) {
                continue;
            }
            store.session_samples.insert(
                dimension.to_string(),
                SessionSample {
                    at: now,
                    open_sessions: open,
                },
            );
            due.push((tool, open));
        }
        (due, any_open)
    };
    if !due.is_empty() {
        append(engine, &due, now);
    }
    any_open
}

/// Start the one in-process task that keeps sampling this store's live sessions, and stands
/// down once none is open or its wake-ups are spent.
///
/// At most one clock runs per store ([`SESSION_CLOCK_LEASE`]), and with no async runtime to
/// spawn onto there is no clock — the series are then published by the calls that open and
/// close sessions alone, as [`publish_active_session_counts`] always does.
pub fn drive_session_metric_clock<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) {
    let Some(state) = service_state(&**engine) else {
        return;
    };
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let Some(claim) = claim_session_clock(&mut state.lock(), SystemTime::now()) else {
        return;
    };
    let engine = Arc::clone(engine);
    let handle = runtime.spawn(async move {
        for _ in 0..MAX_SESSION_CLOCK_WAKE_UPS {
            // Every wake-up waits, so a store with nothing to sample costs a yield rather
            // than a spin, and the runtime is handed back in between.
            ::tokio::time::sleep(SESSION_SAMPLE_TICK).await;
            let any_open = publish_active_session_counts(&*engine);
            if !renew_session_clock(&*engine, any_open) {
                return;
            }
        }
        release_session_clock(&*engine);
    });
    // The handle is kept in the store, so this task can be stopped: the next claim aborts a
    // clock that outlived its lease ([`claim_session_clock`]). Only while THIS call's claim is
    // still the current one, though: by now the task may have stood down and a later call
    // taken a claim of its own, and writing this finished handle over that live clock's would
    // leave the running one with no handle to be aborted by. The claim number says which of
    // the two the slot belongs to.
    let mut store = state.lock();
    if store.session_clock_claims == claim {
        store.session_clock = Some(handle);
    }
}

/// Publish what the built-in tools owe and keep the clock that samples them running — the
/// ONE seam every operation that opens or closes a session calls, so neither half can be
/// forgotten at a call site.
pub fn report_active_sessions<Eng: Send + Sync + 'static>(engine: &Arc<Eng>) {
    publish_active_session_counts(&**engine);
    drive_session_metric_clock(engine);
}

/// Append each tool's reading to the CloudWatch metric store, and make the alarms watching
/// those series due for evaluation.
///
/// The datapoints go into the CloudWatch emulator's OWN store — the same one `ListMetrics`,
/// `GetMetricStatistics` and the alarm evaluation read — so one metric has one home and the
/// publisher and its readers cannot drift.
fn append<Eng: Send + Sync + 'static>(engine: &Eng, due: &[(BuiltInTool, usize)], at: SystemTime) {
    let Some(state) = cloudwatch::state::service_state(engine) else {
        return;
    };
    let sampled_secs = epoch_secs(at);
    let mut store = state.lock();
    let mut written: Vec<cloudwatch::state::WrittenSeries> = Vec::new();
    for (tool, open) in due {
        let key = cloudwatch::state::MetricKey {
            namespace: NAMESPACE.to_string(),
            metric_name: ACTIVE_SESSION_COUNT.to_string(),
            dimensions: [(
                SERVICE_DIMENSION.to_string(),
                tool.service_dimension().to_string(),
            )]
            .into_iter()
            .collect(),
        };
        // A gauge: ONE observation of the value the tool stood at, so every statistic over
        // the period it lands in reports that reading rather than a total of events.
        let value = count_as_f64(*open);
        let datum = cloudwatch::state::StoredDatum {
            timestamp_secs: sampled_secs,
            sample_count: 1.0,
            sum: value,
            minimum: value,
            maximum: value,
            unit: Some(COUNT_UNIT.to_string()),
            storage_resolution: STANDARD_STORAGE_RESOLUTION_SECONDS,
        };
        if store.append_datapoints(key.clone(), vec![datum]).is_err() {
            // The metric store holds no more series. The request this sample came off is
            // answered either way; the dropped reading is reported where a developer can
            // see it.
            ::tracing::warn!(
                tool = %tool.service_dimension(),
                metric = %ACTIVE_SESSION_COUNT,
                "the CloudWatch metric store is at capacity, so this tool's live session \
                 count was not published"
            );
            continue;
        }
        written.push(cloudwatch::state::WrittenSeries {
            key,
            units: vec![COUNT_UNIT.to_string()],
        });
    }
    // A concurrency alarm is an alarm on one of these series, so the arriving datapoints are
    // what makes its next evaluation due — exactly as a published datapoint does.
    store.make_alarms_due(&written, sampled_secs);
}

/// A session count as the value its datapoint carries. The counts are bounded by this
/// store's own per-tool ceilings, so the conversion is exact.
fn count_as_f64(count: usize) -> f64 {
    f64::from(u32::try_from(count).unwrap_or(u32::MAX))
}

/// `at` as the epoch seconds a stored datapoint carries, which is the unit CloudWatch's own
/// readers window by.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// Take the clock claim for this store when no clock holds a live lease, answering the NUMBER
/// of the claim taken — which is what tells the caller's own clock from a later one — or `None`
/// when a live lease stands and the caller may start none.
///
/// The task of a clock whose lease has lapsed without being released is ABORTED as the claim
/// is taken: such a clock has either ended already (where the abort does nothing) or is
/// stuck, and leaving a stuck one running beside the new one would be two clocks over one
/// store. It is the handle this store holds that makes that possible, which is why the clock
/// is kept there rather than dropped on the floor.
fn claim_session_clock(
    store: &mut ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState,
    now: SystemTime,
) -> Option<u64> {
    if store.session_clock_until.is_some_and(|until| now < until) {
        return None;
    }
    if let Some(previous) = store.session_clock.take() {
        previous.abort();
    }
    store.session_clock_claims = store.session_clock_claims.wrapping_add(1);
    store.session_clock_until = now.checked_add(SESSION_CLOCK_LEASE);
    Some(store.session_clock_claims)
}

/// Renew the running clock's lease, reporting whether it is to keep sampling.
///
/// A store holding no open session has nothing left to say: the gauges have already reported
/// the zero they fell to, so the clock releases its claim and stands down; the next call that
/// opens one starts a clock again.
fn renew_session_clock<Eng: Send + Sync + 'static>(engine: &Eng, any_open: bool) -> bool {
    let Some(state) = service_state(engine) else {
        return false;
    };
    let mut store = state.lock();
    if !any_open {
        release(&mut store);
        return false;
    }
    store.session_clock_until = SystemTime::now().checked_add(SESSION_CLOCK_LEASE);
    true
}

/// Release the claim of a clock that has spent its wake-ups.
fn release_session_clock<Eng: Send + Sync + 'static>(engine: &Eng) {
    if let Some(state) = service_state(engine) {
        release(&mut state.lock());
    }
}

/// Give up the clock claim: no clock holds the lease, and none holds a task either.
fn release(store: &mut ComAmazonawsBedrockagentcoreAmazonbedrockagentcoreState) {
    store.session_clock_until = None;
    store.session_clock = None;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An instant inside the minute that starts at `minute * 60`, `offset` seconds into it.
    fn inside_minute(minute: u64, offset: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + Duration::from_secs(minute * SESSION_SAMPLE_INTERVAL.as_secs() + offset)
    }

    /// The cadence rule: a tool owes its first reading at once, another whenever the count
    /// it reads CHANGES, and another in each new minute — and owes none while neither the
    /// minute nor the count has moved.
    #[test]
    fn a_reading_is_owed_on_the_first_sample_on_a_change_and_once_a_minute() {
        let last = SessionSample {
            at: inside_minute(100, 5),
            open_sessions: 2,
        };
        let rows: [(&str, Option<SessionSample>, usize, SystemTime, bool); 5] = [
            ("nothing sampled yet", None, 0, inside_minute(100, 5), true),
            (
                "the same count, the same minute",
                Some(last),
                2,
                inside_minute(100, 59),
                false,
            ),
            (
                "one more session open, the same minute",
                Some(last),
                3,
                inside_minute(100, 6),
                true,
            ),
            (
                "every session closed, the same minute",
                Some(last),
                0,
                inside_minute(100, 6),
                true,
            ),
            (
                "the same count, the next minute",
                Some(last),
                2,
                inside_minute(101, 0),
                true,
            ),
        ];
        for (case, sampled, open_sessions, now, expected) in rows {
            let due = sample_due(sampled, open_sessions, now);
            assert_eq!(
                due, expected,
                "case {case:?}: expected sample_due to be {expected}, got {due}"
            );
        }
    }

    /// Each built-in tool publishes under its own `Service` dimension value, and no two
    /// share one — otherwise one tool's concurrency would be folded into another's series.
    #[test]
    fn every_built_in_tool_has_its_own_service_dimension_value() {
        let named: Vec<&str> = BuiltInTool::ALL
            .into_iter()
            .map(BuiltInTool::service_dimension)
            .collect();
        let distinct: ::std::collections::BTreeSet<&str> = named.iter().copied().collect();
        assert_eq!(
            distinct.len(),
            named.len(),
            "expected one dimension value per built-in tool, got {named:?}"
        );
        for value in named {
            assert!(
                !value.is_empty(),
                "expected every built-in tool to name a dimension value, got {value:?}"
            );
        }
    }
}
