// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the QUEUE a build waits in for a machine of its compute fleet, and
//! for the wait it is given there, driven over the wire the router serves: `POST /` with
//! `X-Amz-Target: CodeBuild_20161006.<Operation>` on awsJson1_1, through the harness
//! [`super::project_build_lifecycle`] owns.
//!
//! The rules under test:
//!
//! * a build whose environment names a compute fleet runs only while fewer builds of that
//!   fleet are running than the fleet has machines (`Fleet$baseCapacity`, "the initial
//!   number of machines allocated to the compute fleet, which defines the number of builds
//!   that can run in parallel"), and an OVERFLOW build either waits for one — the fleet's
//!   `overflowBehavior` `QUEUE`, "your overflow builds need to wait on the existing fleet
//!   instance to become available" — or runs off the fleet, on `ON_DEMAND`, "your overflow
//!   builds run on CodeBuild on-demand";
//! * a build still waiting when the wait it was given runs out
//!   (`Build$queuedTimeoutInMinutes`, "the number of minutes a build is allowed to be
//!   queued before it times out") is ENDED there: `buildComplete` true, `buildStatus`
//!   `FAILED`, `currentPhase` `COMPLETED`, with the `QUEUED` phase itself carrying
//!   `phaseStatus` `TIMED_OUT`.
//!
//! Nothing here waits out a documented window on the wall clock: the queue window's own
//! floor is five minutes, so the rows that need it to have run out move the build's stored
//! start instant back past it — the service stores that instant and never sleeps for it —
//! and every other wait ends on the CONDITION it is waiting for.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use service_com_amazonaws_codebuild_codebuild_20161006::state::{
    self, ComAmazonawsCodebuildCodebuild20161006State as CodeBuildState,
};
use smithy_server_runtime::MultiServiceRouter;

use super::fleet_lifecycle::{batch_get_fleets, create_fleet};
use super::project_build_lifecycle::{call, create_project, emulator_on, phase_pairs, start_build};

/// How long a poll below gives the condition it waits for. Generous on purpose: the wait
/// ends on the condition, and the deadline is only there so a condition that never holds
/// fails the test instead of hanging it.
const CONDITION_DEADLINE: Duration = Duration::from_secs(90);

/// How long a poll waits before looking again.
const LOOK_AGAIN_AFTER: Duration = Duration::from_millis(25);

/// The `BuildPhaseType` a build waits for a machine in.
const QUEUED: &str = "QUEUED";

/// The `BuildPhaseType` a build runs its buildspec's `build` commands in, which is where a
/// build holding a machine sits for as long as those commands take.
const BUILD: &str = "BUILD";

/// A buildspec whose `build` phase states it takes fifteen minutes, for the build that HOLDS
/// a machine while the rows below start others behind it.
///
/// The commands are performed in process and spend none of the host's clock, so this is the
/// time the build STATES it took — the window it reports, and so the window its machine is
/// accounted as taken for. How soon its outcome may be READ is a shorter, compressed span
/// (`state::BUILD_CLOCK_SCALE`, bounded by `state::MAX_BUILD_EXECUTION_MILLIS`), which is
/// what makes the phase it is in observable.
const HOLDING_BUILDSPEC: &str = "version: 0.2\nphases:\n  build:\n    commands:\n      - sleep 900\n";

/// A buildspec whose `build` phase states it takes ten seconds, for a holder a row wants to
/// see give its machine BACK while it watches the build behind it.
const BRIEF_BUILDSPEC: &str = "version: 0.2\nphases:\n  build:\n    commands:\n      - sleep 10\n";

/// A buildspec whose `build` phase states it takes four seconds, for a row that watches a
/// QUEUE of builds go through one machine.
const CHAINED_BUILDSPEC: &str = "version: 0.2\nphases:\n  build:\n    commands:\n      - sleep 4\n";

/// A buildspec no command of which states any time, for a build a row only needs to see
/// admitted or held rather than holding anything.
const INSTANT_BUILDSPEC: &str = "version: 0.2";

/// One engine, and a router carrying only this service over it — so a row can move a
/// stored instant through the very store the handlers write.
fn emulator() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(Arc::clone(&engine));
    (engine, router)
}

/// Create a fleet of `capacity` machines with `overflow` as its overflow behaviour (none
/// when `None`), wait until its capacity is provisioned, and hand back its ARN.
async fn active_fleet(
    router: &MultiServiceRouter,
    name: &str,
    capacity: i64,
    overflow: Option<&str>,
) -> String {
    let mut extra = json!({"baseCapacity": capacity});
    if let (Value::Object(target), Some(behavior)) = (&mut extra, overflow) {
        target.insert("overflowBehavior".to_string(), json!(behavior));
    }
    let fleet = create_fleet(router, name, extra).await;
    let arn = fleet["arn"].as_str().expect("a fleet ARN").to_string();
    let deadline = tokio::time::Instant::now() + CONDITION_DEADLINE;
    loop {
        let read = batch_get_fleets(router, &[name]).await;
        if read["fleets"][0]["status"]["statusCode"] == json!("ACTIVE") {
            return arn;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "fleet {name:?}: expected it to become ACTIVE, last read {read}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Create a project whose builds run `buildspec`, pinned to `fleet_arn` when one is given,
/// with `extra`'s members merged into the request.
async fn fleet_project(
    router: &MultiServiceRouter,
    name: &str,
    fleet_arn: Option<&str>,
    buildspec: &str,
    extra: Value,
) {
    let mut environment = json!({
        "type": "LINUX_CONTAINER",
        "image": "aws/codebuild/standard:7.0",
        "computeType": "BUILD_GENERAL1_SMALL",
    });
    if let (Value::Object(target), Some(arn)) = (&mut environment, fleet_arn) {
        target.insert("fleet".to_string(), json!({"fleetArn": arn}));
    }
    let mut body = json!({
        "source": {"type": "NO_SOURCE", "buildspec": buildspec},
        "environment": environment,
    });
    if let (Value::Object(target), Value::Object(members)) = (&mut body, extra) {
        target.extend(members);
    }
    create_project(router, name, body).await;
}

/// Start a build of `project`, with `extra`'s members merged into the `StartBuild` request,
/// and hand back its id.
async fn started_build_id(router: &MultiServiceRouter, project: &str, extra: Value) -> String {
    let mut body = json!({"projectName": project});
    if let (Value::Object(target), Value::Object(members)) = (&mut body, extra) {
        target.extend(members);
    }
    let build = start_build(router, body).await;
    build["id"].as_str().expect("a build id").to_string()
}

/// Read one build back through `BatchGetBuilds`, asserting it is reported.
async fn read_build(router: &MultiServiceRouter, id: &str) -> Value {
    let (status, body) = call(router, "BatchGetBuilds", json!({"ids": [id]})).await;
    assert_eq!(
        status, 200,
        "BatchGetBuilds {id:?}: expected 200, got {status} with {body}"
    );
    assert!(
        body["builds"][0].is_object(),
        "BatchGetBuilds {id:?}: expected the build back, got {body}"
    );
    body["builds"][0].clone()
}

/// The phase a build reports it is in.
fn current_phase(build: &Value) -> String {
    build["currentPhase"].as_str().unwrap_or_default().to_string()
}

/// Read `id` back until `holds` accepts the record, and hand that record back. `case` and
/// `want` only name the condition in the failure message.
async fn poll_build_until(
    router: &MultiServiceRouter,
    id: &str,
    case: &str,
    want: &str,
    holds: impl Fn(&Value) -> bool,
) -> Value {
    let deadline = tokio::time::Instant::now() + CONDITION_DEADLINE;
    loop {
        let record = read_build(router, id).await;
        if holds(&record) {
            return record;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "case {case:?}: expected {want}, last read {record}"
        );
        tokio::time::sleep(LOOK_AGAIN_AFTER).await;
    }
}

/// Move `id`'s stored start instant back by `millis`, so the wait it was given in the queue
/// has run out without the test waiting for it.
///
/// The service stores the instant the build was submitted at and never sleeps for it, so
/// moving it back is the same thing as that much of the wait having elapsed. The record's own
/// `startTime` is moved with it, because that is the instant it reports.
fn age_queue_wait(engine: &Arc<engine::InMemoryEngine>, id: &str, millis: i64) {
    let handle = engine.state::<CodeBuildState>();
    let mut guard = handle.lock();
    let stored = guard
        .builds
        .get_mut(id)
        .unwrap_or_else(|| panic!("the store must hold build {id}"));
    stored.start_millis -= millis;
    // The instant the build may first be placed on a machine moves with it: it is the span
    // after the start that the service stores, not an instant of its own.
    stored.settle_due_millis -= millis;
    stored.build.start_time = Some(state::datetime(stored.start_millis));
}

/// How far back a row moves a build's stored start instant: `fraction` of the wait a build
/// given `minutes` in the queue is held for, which is those documented minutes themselves
/// (`state::queue_window_millis`).
fn waited_millis(minutes: i32, fraction: f64) -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "a fraction of a bounded span of milliseconds is far inside i64"
    )]
    let waited = (state::queue_window_millis(minutes) as f64 * fraction) as i64;
    waited
}

/// One row of the capacity table: the fleet a project's builds run on, and whether a build
/// started while the fleet's machines are all taken WAITS for one.
struct CapacityRow {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The fleet's machine count and overflow behaviour — `None` for a project pinned to no
    /// fleet at all, which runs on on-demand capacity.
    fleet: Option<(i64, Option<&'static str>)>,
    /// Whether the build started behind the holder has to stay in its `QUEUED` phase.
    waits: bool,
}

/// RULE: a build of a project pinned to a compute fleet runs only while that fleet has a
/// machine free; one that finds them all taken waits in its `QUEUED` phase when the fleet
/// asked for its overflow builds to wait, and runs off the fleet when it did not.
///
/// The rows span what the rule generalises over: how many machines the fleet has, what it
/// says about its overflow builds (`FleetOverflowBehavior`, from
/// `smithy_query(query="shape",shape="FleetOverflowBehavior")`: QUEUE, ON_DEMAND — and the
/// member left unstated), and a project pinned to no fleet at all. Every row is read in the
/// SAME window, and the rows that do not wait are read FIRST, so a build of this service
/// leaving the queue promptly is established before the row that must still be in it is read
/// — the waiting build is held by the fleet, not by the span every build spends in the queue.
#[tokio::test]
async fn a_build_waits_for_a_machine_of_its_fleet_only_when_the_fleet_holds_its_overflow() {
    let rows = [
        CapacityRow {
            case: "two machines, one still free",
            fleet: Some((2, Some("QUEUE"))),
            waits: false,
        },
        CapacityRow {
            case: "one machine, overflow builds run on demand",
            fleet: Some((1, Some("ON_DEMAND"))),
            waits: false,
        },
        CapacityRow {
            case: "one machine, no overflow behaviour stated",
            fleet: Some((1, None)),
            waits: false,
        },
        CapacityRow {
            case: "no fleet at all",
            fleet: None,
            waits: false,
        },
        CapacityRow {
            case: "one machine, overflow builds wait",
            fleet: Some((1, Some("QUEUE"))),
            waits: true,
        },
    ];

    let (engine, router) = emulator();

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let project = format!("capacity-{index}");
        let fleet_arn = match row.fleet {
            Some((capacity, overflow)) => {
                Some(active_fleet(&router, &format!("capacity-fleet-{index}"), capacity, overflow).await)
            }
            None => None,
        };
        fleet_project(
            &router,
            &project,
            fleet_arn.as_deref(),
            HOLDING_BUILDSPEC,
            json!({"artifacts": {"type": "NO_ARTIFACTS"}}),
        )
        .await;

        // The build that HOLDS a machine of this row's fleet: it occupies it for as long as
        // its own commands take, which is what a reader observes as the phase it is in.
        let holder = started_build_id(&router, &project, json!({})).await;
        let held = poll_build_until(
            &router,
            &holder,
            case,
            "the holding build to reach its BUILD phase",
            |record| current_phase(record) == BUILD,
        )
        .await;
        assert_eq!(
            held["buildStatus"], "IN_PROGRESS",
            "case {case:?}: a build still running its commands is IN_PROGRESS, got {held}"
        );
        assert_eq!(
            held["buildComplete"], false,
            "case {case:?}: a build still running its commands has not completed, got {held}"
        );

        // And the build started BEHIND it.
        let behind = started_build_id(&router, &project, json!({})).await;
        if !row.waits {
            let record = poll_build_until(
                &router,
                &behind,
                case,
                "the build behind the holder to leave the QUEUED phase",
                |record| current_phase(record) != QUEUED,
            )
            .await;
            assert_ne!(
                record["buildStatus"], "FAILED",
                "case {case:?}: a build a machine was available for must not fail in the \
                 queue, got {record}"
            );
            continue;
        }
        // Past the span every build spends in the queue before anything may place it, so
        // what holds this one is the fleet rather than that span: the service stores the
        // instant the build was submitted at and never sleeps for it, so moving it back is
        // the same thing as the span having elapsed.
        age_queue_wait(&engine, &behind, state::BUILD_SETTLE_MILLIS * 2);
        let record = read_build(&router, &behind).await;
        assert_eq!(
            current_phase(&record),
            QUEUED,
            "case {case:?}: expected the overflow build to be held in QUEUED, got {record}"
        );
        assert_eq!(
            record["buildStatus"], "IN_PROGRESS",
            "case {case:?}: a build waiting for a machine is IN_PROGRESS, got {record}"
        );
        assert_eq!(
            record["buildComplete"], false,
            "case {case:?}: a build waiting for a machine has not completed, got {record}"
        );
        let holder_record = read_build(&router, &holder).await;
        let holder_window_closed = holder_record["buildComplete"] == json!(true)
            && holder_record["endTime"].as_f64().unwrap_or_default()
                <= record["startTime"].as_f64().unwrap_or_default();
        assert!(
            !holder_window_closed,
            "case {case:?}: the fleet's machine must still be taken by the holder's own \
             window when the waiting build joined, got {holder_record}"
        );
    }
}

/// RULE: the machine a build holds is given back when its commands are done, and the build
/// waiting for it then runs — the wait ends on capacity rather than on a fixed span.
#[tokio::test]
async fn a_held_build_runs_as_soon_as_the_fleet_machine_is_free() {
    let (_engine, router) = emulator();
    let fleet_arn = active_fleet(&router, "handover-fleet", 1, Some("QUEUE")).await;
    fleet_project(
        &router,
        "handover",
        Some(&fleet_arn),
        BRIEF_BUILDSPEC,
        json!({"artifacts": {"type": "NO_ARTIFACTS"}}),
    )
    .await;

    let holder = started_build_id(&router, "handover", json!({})).await;
    poll_build_until(
        &router,
        &holder,
        "the holder",
        "the holding build to be given the fleet's machine",
        |record| current_phase(record) != QUEUED,
    )
    .await;
    let waiting = started_build_id(&router, "handover", json!({})).await;
    let held = read_build(&router, &waiting).await;
    assert_eq!(
        current_phase(&held),
        QUEUED,
        "a build started while the fleet's one machine is taken waits in QUEUED, got {held}"
    );

    // The holder's own window closes at an instant the service stored, so the handover is
    // waited FOR rather than slept through.
    let ran = poll_build_until(
        &router,
        &waiting,
        "the build behind the holder",
        "the build to run once the machine was free",
        |record| record["buildComplete"] == json!(true),
    )
    .await;
    assert_eq!(
        ran["buildStatus"], "SUCCEEDED",
        "the build that got the freed machine ran its buildspec, got {ran}"
    );
    let pairs = phase_pairs(&ran);
    assert!(
        pairs.contains(&(QUEUED.to_string(), Some("SUCCEEDED".to_string()))),
        "a build that left the queue onto a machine reports its QUEUED phase as SUCCEEDED, \
         got {pairs:?}"
    );
    let holder_record = read_build(&router, &holder).await;
    assert_eq!(
        holder_record["buildStatus"], "SUCCEEDED",
        "the holder finished its own run, got {holder_record}"
    );
}

/// Where the capacity a queue-window row's build asks for stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Capacity {
    /// The build is pinned to a fleet whose one machine another build holds, and the fleet
    /// asked for its overflow builds to wait: the build waits.
    FullQueueFleet,
    /// The build is pinned to a fleet with a machine free the whole time.
    FreeFleet,
    /// The build is an OVERFLOW build of a fleet whose one machine another build holds and
    /// which runs its overflow on demand.
    OnDemandOverflow,
    /// The build is pinned to no fleet at all.
    NoFleet,
}

/// What a queue-window row expects of the build it starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// The wait ran out while the build was still waiting for a machine.
    TimedOut,
    /// The build is still waiting and its wait is still open.
    StillQueued,
    /// Capacity was available for the build, so it ran — whatever its wait says.
    Runs,
}

/// One row of the queue-window table: the wait the build is given, the capacity it asks for,
/// how long it has already waited, and what it must then report.
struct WindowRow {
    /// What this row is, for the assertion messages.
    case: &'static str,
    /// The project's own `queuedTimeoutInMinutes` — `None` for a project that states none,
    /// which leaves the build with the service default.
    project_minutes: Option<i32>,
    /// `StartBuild`'s `queuedTimeoutInMinutesOverride` — `None` for a start that overrides
    /// nothing.
    override_minutes: Option<i32>,
    /// Where the capacity this row's build asks for stands.
    capacity: Capacity,
    /// How much of the wait has already elapsed when the build is read, as a fraction of the
    /// wait itself — above 1 the wait has run out, below it has not.
    waited: f64,
    /// The wait the build reports, which is what the window is counted from.
    reported_minutes: i32,
    /// What the build must report once that much of its wait has elapsed.
    expect: Expect,
}

/// RULE: a build still waiting for a machine when the wait it was given in the queue runs
/// out is ended there — `buildComplete` true, `buildStatus` `FAILED`, `currentPhase`
/// `COMPLETED`, with the `QUEUED` phase carrying `phaseStatus` `TIMED_OUT` — and the wait is
/// the one the build reports: `StartBuild`'s override, else the project's value, else the
/// service default.
///
/// The rows span what the rule generalises over: where the wait came from, and how it sits
/// against the published bounds of `TimeOut` (`@range` min 5, max 480): the floor, the
/// ceiling both inside its wait and past it, a wait the project states, and the service
/// default.
#[tokio::test]
async fn a_build_that_outwaits_its_queue_window_fails_with_the_queued_phase_timed_out() {
    let rows = [
        WindowRow {
            case: "the documented floor of the wait, run out",
            project_minutes: None,
            override_minutes: Some(5),
            capacity: Capacity::FullQueueFleet,
            waited: 1.2,
            reported_minutes: 5,
            expect: Expect::TimedOut,
        },
        WindowRow {
            case: "the documented ceiling of the wait, still open",
            project_minutes: None,
            override_minutes: Some(480),
            capacity: Capacity::FullQueueFleet,
            waited: 0.5,
            reported_minutes: 480,
            expect: Expect::StillQueued,
        },
        WindowRow {
            case: "the documented ceiling of the wait, run out",
            project_minutes: None,
            override_minutes: Some(480),
            capacity: Capacity::FullQueueFleet,
            waited: 1.2,
            reported_minutes: 480,
            expect: Expect::TimedOut,
        },
        WindowRow {
            case: "the project's own wait, run out",
            project_minutes: Some(10),
            override_minutes: None,
            capacity: Capacity::FullQueueFleet,
            waited: 1.2,
            reported_minutes: 10,
            expect: Expect::TimedOut,
        },
        WindowRow {
            case: "the service default, still open",
            project_minutes: None,
            override_minutes: None,
            capacity: Capacity::FullQueueFleet,
            waited: 0.5,
            reported_minutes: state::DEFAULT_QUEUED_TIMEOUT_MINUTES,
            expect: Expect::StillQueued,
        },
        WindowRow {
            case: "a machine of its own fleet free throughout, the wait long run out",
            project_minutes: None,
            override_minutes: Some(5),
            capacity: Capacity::FreeFleet,
            waited: 3.0,
            reported_minutes: 5,
            expect: Expect::Runs,
        },
        WindowRow {
            case: "an overflow build of a fleet that runs its overflow on demand, the wait \
                   long run out",
            project_minutes: None,
            override_minutes: Some(5),
            capacity: Capacity::OnDemandOverflow,
            waited: 3.0,
            reported_minutes: 5,
            expect: Expect::Runs,
        },
        WindowRow {
            case: "no fleet at all, the wait long run out",
            project_minutes: None,
            override_minutes: Some(5),
            capacity: Capacity::NoFleet,
            waited: 3.0,
            reported_minutes: 5,
            expect: Expect::Runs,
        },
    ];

    let (engine, router) = emulator();
    // The three fleets the rows ask capacity of: one whose single machine is taken and whose
    // overflow builds wait, one with a machine free throughout, and one whose single machine
    // is taken and whose overflow runs on demand.
    let full_queue_fleet = active_fleet(&router, "window-fleet", 1, Some("QUEUE")).await;
    let free_fleet = active_fleet(&router, "window-free-fleet", 1, Some("QUEUE")).await;
    let on_demand_fleet = active_fleet(&router, "window-on-demand-fleet", 1, Some("ON_DEMAND")).await;

    // The builds that take the machine of each fleet that is meant to be full, so the rows
    // asking those fleets for capacity are builds really waiting for one.
    for (index, fleet) in [&full_queue_fleet, &on_demand_fleet].into_iter().enumerate() {
        let project = format!("window-holder-{index}");
        fleet_project(
            &router,
            &project,
            Some(fleet),
            HOLDING_BUILDSPEC,
            json!({"artifacts": {"type": "NO_ARTIFACTS"}}),
        )
        .await;
        let holder = started_build_id(&router, &project, json!({})).await;
        poll_build_until(
            &router,
            &holder,
            "the holder",
            "the holding build to reach its BUILD phase",
            |record| current_phase(record) == BUILD,
        )
        .await;
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let project = format!("window-{index}");
        let mut extra = json!({"artifacts": {"type": "NO_ARTIFACTS"}});
        if let (Value::Object(target), Some(minutes)) = (&mut extra, row.project_minutes) {
            target.insert("queuedTimeoutInMinutes".to_string(), json!(minutes));
        }
        let fleet = match row.capacity {
            Capacity::FullQueueFleet => Some(full_queue_fleet.as_str()),
            Capacity::FreeFleet => Some(free_fleet.as_str()),
            Capacity::OnDemandOverflow => Some(on_demand_fleet.as_str()),
            Capacity::NoFleet => None,
        };
        fleet_project(&router, &project, fleet, INSTANT_BUILDSPEC, extra).await;
        let mut start = json!({});
        if let (Value::Object(target), Some(minutes)) = (&mut start, row.override_minutes) {
            target.insert(
                "queuedTimeoutInMinutesOverride".to_string(),
                json!(minutes),
            );
        }
        let id = started_build_id(&router, &project, start).await;

        let started = read_build(&router, &id).await;
        assert_eq!(
            started["queuedTimeoutInMinutes"],
            json!(row.reported_minutes),
            "case {case:?}: the build must report the wait it was given, got {started}"
        );

        // The wait is the documented minutes themselves, so the row moves the stored start
        // instant back by a fraction of THAT rather than by a span of its own.
        age_queue_wait(&engine, &id, waited_millis(row.reported_minutes, row.waited));
        if row.expect == Expect::Runs {
            let ran = poll_build_until(
                &router,
                &id,
                case,
                "the build to run rather than time out in the queue",
                |record| record["buildComplete"] == json!(true),
            )
            .await;
            assert_eq!(
                ran["buildStatus"], "SUCCEEDED",
                "case {case:?}: a build capacity was available for runs its buildspec, got \
                 {ran}"
            );
            let pairs = phase_pairs(&ran);
            assert!(
                pairs.contains(&(QUEUED.to_string(), Some("SUCCEEDED".to_string()))),
                "case {case:?}: a build that left the queue onto capacity reports its QUEUED \
                 phase as SUCCEEDED, got {pairs:?}"
            );
            continue;
        }
        let record = read_build(&router, &id).await;
        let pairs = phase_pairs(&record);
        if row.expect == Expect::StillQueued {
            assert_eq!(
                record["buildComplete"], false,
                "case {case:?}: a build whose wait is still open has not completed, got \
                 {record}"
            );
            assert_eq!(
                record["buildStatus"], "IN_PROGRESS",
                "case {case:?}: a build whose wait is still open is IN_PROGRESS, got {record}"
            );
            assert_eq!(
                current_phase(&record),
                QUEUED,
                "case {case:?}: a build whose wait is still open is in its QUEUED phase, got \
                 {record}"
            );
            continue;
        }
        assert_eq!(
            record["buildComplete"], true,
            "case {case:?}: a build whose wait ran out has completed, got {record}"
        );
        assert_eq!(
            record["buildStatus"], "FAILED",
            "case {case:?}: a build whose wait ran out is FAILED, got {record}"
        );
        assert_eq!(
            current_phase(&record),
            "COMPLETED",
            "case {case:?}: a build whose wait ran out sits in COMPLETED, got {record}"
        );
        assert_eq!(
            pairs,
            vec![
                ("SUBMITTED".to_string(), Some("SUCCEEDED".to_string())),
                (QUEUED.to_string(), Some("TIMED_OUT".to_string())),
                ("COMPLETED".to_string(), None),
            ],
            "case {case:?}: the phase that ran out of time is the QUEUED one, got {record}"
        );
        assert!(
            record["endTime"].is_number(),
            "case {case:?}: a build that ended reports when it ended, got {record}"
        );
        let message = record["phases"][1]["contexts"][0]["message"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            message.contains(&row.reported_minutes.to_string()),
            "case {case:?}: the timed-out phase explains the wait that ran out, got \
             {message:?}"
        );
    }
}

/// RULE: the machines of a fleet are taken one build at a time — a queue of builds on a
/// one-machine fleet goes through it in turn, and no build's own window begins before the
/// window of the build that had the machine has closed.
#[tokio::test]
async fn a_one_machine_fleet_runs_the_builds_of_its_queue_one_window_at_a_time() {
    let (_engine, router) = emulator();
    let fleet_arn = active_fleet(&router, "chain-fleet", 1, Some("QUEUE")).await;
    fleet_project(
        &router,
        "chain",
        Some(&fleet_arn),
        CHAINED_BUILDSPEC,
        json!({"artifacts": {"type": "NO_ARTIFACTS"}}),
    )
    .await;

    // Three builds of one project, started in turn: one machine has to carry all three.
    let mut ids = Vec::new();
    for _ in 0..3 {
        ids.push(started_build_id(&router, "chain", json!({})).await);
    }
    let mut windows = Vec::new();
    for id in &ids {
        let record = poll_build_until(
            &router,
            id,
            "a build of the queue",
            "every build of the queue to run",
            |record| record["buildComplete"] == json!(true),
        )
        .await;
        assert_eq!(
            record["buildStatus"], "SUCCEEDED",
            "a build of the queue ran its buildspec, got {record}"
        );
        // Where the build's own window begins is where its wait in the queue ended, and
        // where it ends is what the build reports.
        let began = record["phases"]
            .as_array()
            .and_then(|phases| {
                phases
                    .iter()
                    .find(|phase| phase["phaseType"] == json!(QUEUED))
                    .and_then(|phase| phase["endTime"].as_f64())
            })
            .unwrap_or_else(|| panic!("a settled build reports its QUEUED phase, got {record}"));
        let ended = record["endTime"]
            .as_f64()
            .unwrap_or_else(|| panic!("a settled build reports an endTime, got {record}"));
        windows.push((began, ended, record));
    }
    // In the order the machine carried them, which is the order their windows begin in.
    windows.sort_by(|left, right| left.0.total_cmp(&right.0));
    for pair in windows.windows(2) {
        let (_, first_ended, first) = &pair[0];
        let (second_began, _, second) = &pair[1];
        assert!(
            second_began >= first_ended,
            "the window of a build of a one-machine fleet must not begin before the window \
             of the build that had the machine closed: {first_ended} then {second_began}, in \
             {first} and {second}"
        );
    }
}

/// RULE: the wait in the queue applies to a build that is WAITING for a machine — a build a
/// machine was free for runs, however long ago it was started.
#[tokio::test]
async fn a_build_a_machine_was_free_for_is_never_timed_out_in_the_queue() {
    let (engine, router) = emulator();
    fleet_project(
        &router,
        "on-demand-window",
        None,
        "version: 0.2",
        json!({"artifacts": {"type": "NO_ARTIFACTS"}}),
    )
    .await;
    let id = started_build_id(
        &router,
        "on-demand-window",
        json!({"queuedTimeoutInMinutesOverride": 5}),
    )
    .await;
    // Older than several times over the whole wait it was given, and still a build nothing
    // held back.
    age_queue_wait(&engine, &id, waited_millis(5, 3.0));

    let record = poll_build_until(
        &router,
        &id,
        "a build on on-demand capacity",
        "the build to run rather than time out in the queue",
        |record| record["buildComplete"] == json!(true),
    )
    .await;
    assert_eq!(
        record["buildStatus"], "SUCCEEDED",
        "a build nothing held back runs its buildspec, got {record}"
    );
    let pairs = phase_pairs(&record);
    assert!(
        pairs.contains(&(QUEUED.to_string(), Some("SUCCEEDED".to_string()))),
        "a build nothing held back leaves the queue onto a machine, got {pairs:?}"
    );
}
