// SPDX-License-Identifier: CC0-1.0

//! What a group does once the `Launch` process it suspended is given back: it fills the
//! capacity the suspension withheld, and every member it adds comes INTO SERVICE.
//!
//! RULE: resuming a scaling process re-enters exactly the reconciliation that process
//! performs — "resumes the specified suspended auto scaling processes, or all suspended
//! process, for the specified Auto Scaling group"
//! (`smithy_query(query="operation",operation="ResumeProcesses")`), and `Launch` is the
//! process that adds instances to a group — so a group that has `Launch` back launches the
//! shortfall between the capacity it holds and the capacity its members provide, and each
//! instance it launches passes through the transient state of a new member and comes to rest
//! reporting the `LifecycleState` of a member in service (`Pending` -> `InService`, the
//! states `AutoScalingGroup$Instances` projects). Until then the group is STEADY: the
//! capacity every change made while `Launch` was suspended is what it holds, no member was
//! added for it, and nothing is owed on the clock — so a client polling such a group reads
//! the same answer however often it looks, and a client that resumes the process reaches an
//! in-service member with no further call.
//!
//! The table below spans what that rule generalises over: HOW MANY units the withheld change
//! recorded (one, two, three), WHICH writer recorded them (`SetDesiredCapacity`,
//! `UpdateAutoScalingGroup` by `DesiredCapacity` and by `MinSize` alone, which carries the
//! capacity up with it), how WIDE the placement is (one subnet, two), what the group's
//! `HealthCheckGracePeriod` is (none, no seconds at all, a long one) and how the resumption
//! NAMES the process (`Launch` itself, no process at all, the whole published catalog) —
//! including a row that leaves another suspension in force, which the resumption must not
//! touch. Every row goes over the awsQuery wire through one router.
//!
//! The sibling suites pin the other parts of the same rule: `all/resume_processes.rs` pins
//! which suspensions a resumption turns off and the instance COUNTS it moves,
//! `all/suspended_launch_capacity_intent.rs` pins the capacity a withheld change records, and
//! `all/owed_capacity_convergence.rs` pins the ONE case of a launch an attachment had already
//! stopped, resumed afterwards. What this suite adds is the TABLE above over the plain
//! suspend-record-resume path — the writer that recorded the capacity, the width of the
//! placement, the group's grace period, how the resumption names the process, and a
//! suspension it must leave in force — each row asserted on the `LifecycleState` every member
//! comes to rest in and on the group owing the clock nothing while the change is withheld.
//!
//! Each row drives a store of its OWN (one engine, one router, its own subnets), so what the
//! clock owes is this row's group and nothing another row left behind.
//!
//! Every wait here ends on the CONDITION it is waiting for — the members the resumption brings
//! into service, and the clock settling on nothing that is already past — looked at again
//! under a deadline generous enough that only a condition which never holds turns into a
//! failure. A convergence falls due at the same instant as the announcements it produces, and
//! the reader retires the one while the service's own clock retires the other, so neither is
//! read at a single moment.

use std::time::{Duration, Instant, SystemTime};

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;

use crate::support::{borrowed, emulator, p, query, seed_subnets, tag_value, tag_values};

/// The launch source every group here is created from: a launch TEMPLATE, because no account
/// can create a launch configuration (`CreateLaunchConfiguration` is refused).
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The `LifecycleState` of a member that has come into service, from the model's own
/// `LifecycleState` enum (`smithy_query(query="shape",shape="LifecycleState")`).
const IN_SERVICE: &str = "InService";

/// How long a row keeps looking for the state the clock owes before it calls the wait a
/// failure, and how long it waits between looks. Far above the span one launch of this
/// service takes, because what is waited for is the CONDITION and this is only the backstop
/// that turns a condition which never holds into a failed assertion.
const DEADLINE: Duration = Duration::from_secs(60);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How the capacity one row's group is asked to hold is recorded, while `Launch` is suspended.
enum Recorded {
    /// `SetDesiredCapacity`, naming the capacity.
    DesiredCapacity(i32),
    /// `UpdateAutoScalingGroup`, naming the capacity.
    UpdatedCapacity(i32),
    /// `UpdateAutoScalingGroup` naming only a `MinSize` above the capacity the group holds,
    /// which "sets the group's DesiredCapacity to the new MinSize value"
    /// (`smithy_query(query="doc",shape="UpdateAutoScalingGroupType",member="MinSize")`).
    MinSize(i32),
}

/// What one row's `ResumeProcesses` request names in `ScalingProcesses`.
enum Resumed {
    /// The processes written here, in this order.
    Processes(Vec<&'static str>),
    /// The member is left out altogether: "if you omit this property, all processes are
    /// specified" (`ScalingProcessQuery$ScalingProcesses`).
    Omitted,
    /// Every process the service publishes, named explicitly.
    WholePublishedCatalog,
}

impl Resumed {
    /// The names one row sends, given the catalog the service publishes.
    fn names(&self, catalog: &[String]) -> Vec<String> {
        match self {
            Self::Processes(processes) => {
                processes.iter().map(|process| (*process).to_string()).collect()
            }
            Self::Omitted => Vec::new(),
            Self::WholePublishedCatalog => catalog.to_vec(),
        }
    }
}

/// One row: what the group had suspended, how the withheld capacity was recorded, where the
/// group places instances, what grace period it carries, what the resumption names, and what
/// the group then holds.
struct Row {
    case: &'static str,
    /// The processes suspended before the capacity change.
    suspend: Vec<&'static str>,
    recorded: Recorded,
    /// How many of the seeded subnets the group is placed over.
    subnets: usize,
    /// The group's `HealthCheckGracePeriod`, when it states one.
    grace_period: Option<i32>,
    resume: Resumed,
    /// The members the group comes to rest holding, every one of them in service.
    expected_members: usize,
    /// The capacity the group then holds, stated rather than derived from the member count:
    /// the two agree only while every member is worth one unit of capacity.
    expected_capacity: i32,
    /// The processes the group still reports suspended after the resumption, in the order it
    /// reports them.
    still_suspended: Vec<&'static str>,
}

/// The processes the service publishes as its own catalog, read over the wire from
/// `DescribeScalingProcessTypes` — the same set a request may name — so no row here writes
/// that membership out.
async fn published_catalog(router: &smithy_server_runtime::MultiServiceRouter) -> Vec<String> {
    let (status, body) = query(router, "DescribeScalingProcessTypes", &[]).await;
    assert_eq!(
        status, 200,
        "the published process catalog: expected 200, got {status}: {body}"
    );
    let catalog = tag_values(&body, "ProcessName");
    assert!(
        catalog.len() > 1,
        "the published process catalog must name more than one process, got {catalog:?}"
    );
    catalog
}

/// Create one group holding no capacity, placed over `placement`, with the grace period the
/// row states.
async fn create_group(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    name: &str,
    placement: &str,
    grace_period: Option<i32>,
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p(LAUNCH_SOURCE.0, LAUNCH_SOURCE.1),
        p("MinSize", "0"),
        p("MaxSize", "3"),
        p("DesiredCapacity", "0"),
        p("VPCZoneIdentifier", placement),
    ];
    if let Some(seconds) = grace_period {
        params.push(p("HealthCheckGracePeriod", seconds.to_string()));
    }
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: creating {name:?} expected 200, got {status}: {body}"
    );
}

/// One `SuspendProcesses` or `ResumeProcesses` call naming `processes` — naming none when it
/// is empty, which names every process.
async fn process_call(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    action: &str,
    group: &str,
    processes: &[String],
) {
    let mut params = vec![p("AutoScalingGroupName", group)];
    for (index, process) in processes.iter().enumerate() {
        params.push(p(
            &format!("ScalingProcesses.member.{}", index + 1),
            process.clone(),
        ));
    }
    let (status, body) = query(router, action, &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} expected 200, got {status}: {body}"
    );
}

/// Record the capacity one row's group is asked to hold, through the writer it names.
async fn record_capacity(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    group: &str,
    recorded: &Recorded,
) {
    let (action, params) = match recorded {
        Recorded::DesiredCapacity(capacity) => (
            "SetDesiredCapacity",
            vec![
                p("AutoScalingGroupName", group),
                p("DesiredCapacity", capacity.to_string()),
            ],
        ),
        Recorded::UpdatedCapacity(capacity) => (
            "UpdateAutoScalingGroup",
            vec![
                p("AutoScalingGroupName", group),
                p("DesiredCapacity", capacity.to_string()),
            ],
        ),
        Recorded::MinSize(min_size) => (
            "UpdateAutoScalingGroup",
            vec![
                p("AutoScalingGroupName", group),
                p("MinSize", min_size.to_string()),
            ],
        ),
    };
    let (status, body) = query(router, action, &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} expected 200, got {status}: {body}"
    );
}

/// What one group reports: the capacity it holds, the `LifecycleState` of each member, the
/// members' ids, and the processes it has suspended.
async fn reported(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    group: &str,
) -> (String, Vec<String>, Vec<String>, Vec<String>) {
    let (status, body) = query(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading {group:?} back expected 200, got {status}: {body}"
    );
    (
        tag_value(&body, "DesiredCapacity"),
        tag_values(&body, "LifecycleState"),
        tag_values(&body, "InstanceId"),
        tag_values(&body, "ProcessName"),
    )
}

/// The instant this store is waiting on next, or `None` when it is waiting on nothing.
fn owed(engine: &std::sync::Arc<engine::InMemoryEngine>) -> Option<SystemTime> {
    autoscaling::state::service_state(&**engine)
        .expect("the Auto Scaling store is mounted on this engine")
        .lock()
        .next_clock_due_at()
}

/// A group whose withheld `Launch` process is resumed fills the capacity it holds, and every
/// member it adds comes into service — while the withheld change itself leaves the group
/// steady, with nothing owed on the clock.
#[tokio::test]
async fn a_resumed_launch_process_brings_the_groups_members_into_service() {
    let rows = [
        Row {
            case: "one unit set outright, Launch itself resumed",
            suspend: vec!["Launch"],
            recorded: Recorded::DesiredCapacity(1),
            subnets: 2,
            // No seconds of grace at all, which is what a client that wants its members
            // checked from the moment they are in service asks for.
            grace_period: Some(0),
            resume: Resumed::Processes(vec!["Launch"]),
            expected_members: 1,
            expected_capacity: 1,
            still_suspended: Vec::new(),
        },
        Row {
            case: "two units updated, the resumption naming no process",
            suspend: vec!["Launch"],
            recorded: Recorded::UpdatedCapacity(2),
            subnets: 1,
            grace_period: None,
            resume: Resumed::Omitted,
            expected_members: 2,
            expected_capacity: 2,
            still_suspended: Vec::new(),
        },
        Row {
            case: "three units carried up by MinSize, the whole published catalog resumed",
            suspend: vec!["Launch", "AZRebalance"],
            recorded: Recorded::MinSize(3),
            subnets: 2,
            grace_period: Some(300),
            resume: Resumed::WholePublishedCatalog,
            expected_members: 3,
            expected_capacity: 3,
            still_suspended: Vec::new(),
        },
        Row {
            case: "Launch alone resumed beside a suspension the request does not name",
            suspend: vec!["Launch", "Terminate"],
            recorded: Recorded::DesiredCapacity(1),
            subnets: 1,
            grace_period: Some(0),
            resume: Resumed::Processes(vec!["Launch"]),
            expected_members: 1,
            expected_capacity: 1,
            still_suspended: vec!["Terminate"],
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        // One store per row: the clock this row reads owes what this row's own group owes.
        let (router, engine) = emulator();
        let subnets = seed_subnets(&engine, 2);
        let placement = subnets[..row.subnets]
            .iter()
            .map(|subnet| subnet.id.clone())
            .collect::<Vec<String>>()
            .join(",");
        let catalog = published_catalog(&router).await;
        let group = format!("resumed-in-service-{index}");
        create_group(&router, row.case, &group, &placement, row.grace_period).await;
        let suspended: Vec<String> = row
            .suspend
            .iter()
            .map(|process| (*process).to_string())
            .collect();
        process_call(&router, row.case, "SuspendProcesses", &group, &suspended).await;
        record_capacity(&router, row.case, &group, &row.recorded).await;

        // While `Launch` is suspended the change is RECORDED and no member is added — and the
        // group is steady: it owes the clock nothing, so no member is going to appear later
        // either, and a second read answers exactly as the first did.
        let expected_capacity = row.expected_capacity.to_string();
        let withheld = reported(&router, row.case, &group).await;
        assert_eq!(
            withheld.0, expected_capacity,
            "case {:?}: expected the group to hold the capacity the withheld change recorded, \
             got {:?} with the members {:?}",
            row.case, withheld.0, withheld.2
        );
        assert!(
            withheld.1.is_empty(),
            "case {:?}: a group whose Launch process is suspended adds no member, got the \
             states {:?} for {:?}",
            row.case,
            withheld.1,
            withheld.2
        );
        let waiting = owed(&engine);
        assert_eq!(
            waiting, None,
            "case {:?}: a withheld change leaves nothing owed on the clock, got {waiting:?}",
            row.case
        );
        let again = reported(&router, row.case, &group).await;
        assert_eq!(
            again, withheld,
            "case {:?}: a second read reports the same group, got {again:?}",
            row.case
        );

        process_call(
            &router,
            row.case,
            "ResumeProcesses",
            &group,
            &row.resume.names(&catalog),
        )
        .await;

        // And the resumed group converges: it launches the shortfall and every member it added
        // comes to rest in service. Looked at AGAIN until it has, rather than read once and
        // assumed: a new member passes through its transient state first.
        let deadline = Instant::now() + DEADLINE;
        let settled = loop {
            let (capacity, states, ids, suspended) = reported(&router, row.case, &group).await;
            if states.len() == row.expected_members
                && states.iter().all(|state| state == IN_SERVICE)
            {
                break (capacity, states, ids, suspended);
            }
            assert!(
                Instant::now() < deadline,
                "case {:?}: expected the resumed group to hold {} member(s), every one of them \
                 {IN_SERVICE}, got the states {states:?} for {ids:?} at capacity {capacity:?}",
                row.case,
                row.expected_members
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        };
        let (capacity, states, ids, suspended) = settled;

        assert_eq!(
            capacity, expected_capacity,
            "case {:?}: the resumption leaves the recorded capacity where it was, got \
             {capacity:?} with the states {states:?}",
            row.case
        );
        assert_eq!(
            ids.len(),
            row.expected_members,
            "case {:?}: expected {} member id(s), got {ids:?}",
            row.case,
            row.expected_members
        );
        let mut distinct = ids.clone();
        distinct.sort();
        distinct.dedup();
        assert_eq!(
            distinct.len(),
            ids.len(),
            "case {:?}: every member of the group is its own instance, got {ids:?}",
            row.case
        );
        assert_eq!(
            suspended,
            row.still_suspended
                .iter()
                .map(|process| (*process).to_string())
                .collect::<Vec<String>>(),
            "case {:?}: the resumption turns off only the suspensions it names, got \
             {suspended:?}",
            row.case
        );

        // The group has come to REST: what it reached is a steady state, not a moment in a
        // transition, so nothing of this group is owed at an instant already past. Looked at
        // AGAIN until that holds, rather than read once: the convergence this row just
        // observed also queued the announcements of the steps it made, and those are
        // published — and so taken off what the clock is waiting for — by the service's own
        // clock with no further call, so the instant read at the moment convergence became
        // visible is one the service is still retiring. The deadline is the same backstop
        // the wait above uses: it only turns a debt nothing ever retires into a failure.
        let settling_deadline = Instant::now() + DEADLINE;
        loop {
            // Read the group again first, the way a client watching it does: the read
            // materialises whatever the clock has since made due, and what the store is then
            // still waiting on is read after it.
            let (_, held_states, held_ids, _) = reported(&router, row.case, &group).await;
            let waiting = owed(&engine);
            if waiting.is_none_or(|due| due > SystemTime::now()) {
                break;
            }
            assert!(
                Instant::now() < settling_deadline,
                "case {:?}: a converged group owes nothing at an instant already past, got \
                 {waiting:?} with the states {held_states:?} for {held_ids:?}",
                row.case
            );
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }
}