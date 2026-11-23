// SPDX-License-Identifier: CC0-1.0

//! The instances an Auto Scaling group's warm pool maintains, as the CLOCK prepares them —
//! and the calls addressed at a pool a group does not hold.
//!
//! RULE under test: **Amazon EC2 Auto Scaling "launches and maintains" the size of a warm
//! pool, so the instances a pool is short of are prepared AFTER the call that left it short:
//! a pool a caller has just configured — or just left short by a change of the group's own
//! capacity — holds what it held and nothing more, and the instances it then prepares are
//! reported under the pool's own `Warmed:` states. A delete that arrives at a pool which has
//! prepared nothing therefore takes that pool without the force delete, and a call addressed
//! at a warm pool the group does not hold is refused.**
//!
//! Why it matters: a client configures a warm pool and then reads it, deletes it, or tears
//! the group down. A pool that reported instances in the very call that configured it would
//! show every such client capacity that cannot exist yet — and the delete of a pool nobody
//! has warmed would come back refused as "in use", which is the one refusal that tells a
//! client its warmed instances are at stake.
//!
//! The rows differ along what the rule generalises over: how many instances the pool
//! maintains, and whether the shortfall came from the put that configured the pool or from a
//! later change of the group's own capacity.

use std::sync::Arc;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2::state as ec2;

use crate::support::{borrowed, emulator, p, query, tag_value, tag_values, xml_unescape};

/// How often a poll re-reads a warm pool.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How long a poll keeps trying before it gives up: the span this service owes the
/// preparation of an instance plus the one it owes that instance's own transition
/// afterwards, with ample margin for a loaded machine — derived from the service's own span,
/// so the two cannot drift apart.
fn poll_budget() -> Duration {
    autoscaling::state::LIFECYCLE_TRANSITION * 3 + Duration::from_secs(10)
}

/// The zone the groups of this suite place their instances in, in the Region the harness
/// signs for.
const ZONE: &str = "us-west-2a";

/// The launch template the harness seeds for this crate's groups.
const LAUNCH_TEMPLATE: &str = "lt-1";

/// The `PoolState` every pool of this suite is put in, one of the model's own
/// `WarmPoolState` values (`smithy_query(query="shape",shape="WarmPoolState")`).
const POOL_STATE: &str = "Stopped";

/// The `LifecycleState` an instance of a pool in [`POOL_STATE`] settles in, under the prefix
/// the model's `LifecycleState` enum spells warm-pool states with.
fn warmed() -> String {
    format!("Warmed:{POOL_STATE}")
}

/// The whole sentence a call addressed at the warm pool of a group that holds none is
/// refused with: Amazon EC2 Auto Scaling's own text, fixed apart from the group the call
/// addressed the pool on.
///
/// Written out here rather than taken from the service's own builder — a test that re-used
/// the implementation's literal would assert nothing.
fn absent_warm_pool_message(group: &str) -> String {
    format!("No warm pool found for Auto Scaling group '{group}'.")
}

/// The Amazon EC2 states an instance a warm pool in [`POOL_STATE`] holds is in: a pool that
/// is not `Running` holds its instances STOPPED, so one it has prepared is on its way to
/// that state or already there — and a group's own instance, which runs, is in none of them.
///
/// Read off the states Amazon EC2 publishes for an instance (`InstanceState$Name`:
/// `pending | running | shutting-down | terminated | stopping | stopped`).
const WARMED_EC2_STATES: &[&str] = &["pending", "stopping", "stopped"];

/// How many instances of the account's own Amazon EC2 store are in one of `states`, read
/// straight out of that store — without a request to any service, so a poll over it drives
/// no clock of this service's own.
fn ec2_instances_in(engine: &Arc<engine::InMemoryEngine>, states: &[&str]) -> usize {
    let state = ec2::service_state(&**engine).expect("the EC2 store is mounted on this engine");
    let store = state.lock();
    store
        .instances
        .values()
        .filter(|record| states.contains(&record.state.as_str()))
        .count()
}

/// Wait until the account's own Amazon EC2 store holds `wanted` instances in one of
/// `states`, WITHOUT calling any service in between, or a generous deadline passes.
async fn ec2_holding(
    engine: &Arc<engine::InMemoryEngine>,
    states: &[&str],
    wanted: usize,
) -> usize {
    let deadline = Instant::now() + poll_budget();
    loop {
        let held = ec2_instances_in(engine, states);
        if held == wanted || Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Create one group placed by Availability Zone alone, with the bounds a case needs.
async fn create_group(router: &MultiServiceRouter, name: &str, max_size: i32, desired: i32) {
    let params = vec![
        p("AutoScalingGroupName", name),
        p("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
        p("MinSize", "0"),
        p("MaxSize", max_size.to_string()),
        p("DesiredCapacity", desired.to_string()),
        p("AvailabilityZones.member.1", ZONE),
    ];
    let (status, body) = query(router, "CreateAutoScalingGroup", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "creating {name:?}: expected 200, got {status} with {body}"
    );
}

/// Configure `group`'s warm pool: its own minimum, and the prepared capacity a case names.
async fn put_warm_pool(
    router: &MultiServiceRouter,
    group: &str,
    min_size: i32,
    prepared_capacity: Option<i32>,
) -> (u16, String) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("MinSize", min_size.to_string()),
        p("PoolState", POOL_STATE),
    ];
    if let Some(prepared) = prepared_capacity {
        params.push(p("MaxGroupPreparedCapacity", prepared.to_string()));
    }
    query(router, "PutWarmPool", &borrowed(&params)).await
}

/// The warm pool of `group` as the service reports it NOW.
#[derive(Debug, Default, PartialEq, Eq)]
struct Reported {
    /// Whether the read reports a pool at all.
    held: bool,
    instance_ids: Vec<String>,
    lifecycle_states: Vec<String>,
}

/// Read `group`'s warm pool once.
async fn describe_pool(case: &str, router: &MultiServiceRouter, group: &str) -> Reported {
    let (status, body) = query(
        router,
        "DescribeWarmPool",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeWarmPool({group}) expected 200, got {status} with {body}"
    );
    Reported {
        held: !tag_value(&body, "WarmPoolConfiguration").is_empty(),
        instance_ids: tag_values(&body, "InstanceId"),
        lifecycle_states: tag_values(&body, "LifecycleState"),
    }
}

/// Read `group`'s warm pool until it reports `wanted` instances, or a generous deadline
/// passes — the CONDITION, never a fixed wait.
async fn pool_holding(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
) -> Reported {
    let deadline = Instant::now() + poll_budget();
    loop {
        let reported = describe_pool(case, router, group).await;
        if reported.instance_ids.len() == wanted || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Read `group`'s warm pool until every instance it reports has settled in `state`, or a
/// generous deadline passes.
async fn pool_settled(
    case: &str,
    router: &MultiServiceRouter,
    group: &str,
    wanted: usize,
    state: &str,
) -> Reported {
    let deadline = Instant::now() + poll_budget();
    loop {
        let reported = describe_pool(case, router, group).await;
        let settled = reported.instance_ids.len() == wanted
            && reported
                .lifecycle_states
                .iter()
                .all(|held| held == state);
        if settled || Instant::now() >= deadline {
            return reported;
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// One row of the preparation table: the group's bounds, what the put asks the pool to
/// maintain, and how many instances the documented size rule then has it prepare.
struct Row {
    case: &'static str,
    group: &'static str,
    max_size: i32,
    min_size: i32,
    /// `None` leaves the pool's size to the group's maximum, as the model documents.
    prepared_capacity: Option<i32>,
    maintained: usize,
}

/// A stored pool carrying only what the size rule reads: its own minimum and the prepared
/// capacity it was given.
fn stored_pool(
    min_size: i32,
    prepared_capacity: Option<i32>,
) -> autoscaling::state::StoredWarmPool {
    autoscaling::state::StoredWarmPool {
        min_size,
        max_group_prepared_capacity: prepared_capacity,
        pool_state: POOL_STATE.to_string(),
        instance_reuse_policy: None,
        instances: Vec::new(),
        unplaced_reported: 0,
        prepare_due: None,
    }
}

/// RULE: how many instances a pool MAINTAINS is the documented difference — the prepared
/// capacity it was given, or the group's own maximum where it was given none, less the
/// group's desired capacity, with the pool's own minimum as the floor under it — and it is
/// held to the ceiling this process holds ONE GROUP to, because a group's maximum is stored
/// as the headroom it may later grow within and is not itself bounded.
///
/// This reads the rule where every reader of it reads it, so the ceiling cannot be missed by
/// one of them: a pool preparing more instances than a group may hold is what a stored
/// maximum would otherwise buy.
#[test]
fn the_maintained_size_is_the_documented_difference_held_to_the_group_ceiling() {
    let ceiling = autoscaling::state::MAX_GROUP_CAPACITY;
    // The rows differ along which half of the rule decides the answer, and they take the
    // published ceiling itself, one past it, and far past it.
    let rows = [
        (
            "no prepared capacity: the group's maximum less its desired capacity",
            0,
            None,
            1,
            3,
            2,
        ),
        (
            "a prepared capacity narrower than the group's maximum",
            0,
            Some(2),
            1,
            5,
            1,
        ),
        (
            "a desired capacity above the prepared capacity: a pool of none",
            0,
            Some(1),
            2,
            5,
            0,
        ),
        (
            "the pool's own minimum as the floor under the difference",
            2,
            Some(1),
            2,
            5,
            2,
        ),
        ("the ceiling itself", 0, None, 0, ceiling, ceiling),
        ("one past the ceiling", 0, None, 0, ceiling + 1, ceiling),
        ("a maximum far past the ceiling", 0, None, 0, i32::MAX, ceiling),
        (
            "a minimum far past the ceiling",
            i32::MAX,
            Some(0),
            0,
            1,
            ceiling,
        ),
    ];

    for (case, min_size, prepared_capacity, desired_capacity, max_size, expected) in rows {
        let maintained = stored_pool(min_size, prepared_capacity).target_size(desired_capacity, max_size);
        assert_eq!(
            maintained, expected,
            "case {case:?}: expected a pool maintaining {expected}, got {maintained}"
        );
    }
}

/// RULE: the instances a put leaves the pool short of are prepared after that put, not
/// within it — so the pool a caller reads straight after configuring it reports the
/// configuration and no instance at all, and the instances it maintains appear afterwards,
/// settling in the pool's own state.
#[tokio::test]
async fn a_warm_pool_prepares_what_a_put_left_it_short_of_after_that_put() {
    let (router, _engine) = emulator();
    // The rows differ along how many instances the pool maintains, and whether the request
    // bounds that with its own prepared capacity or leaves it to the group's maximum.
    let rows = [
        Row {
            // FIRST, so every later row's polling carries this pool well past the span its own
            // preparation would have come due in: the re-read after the table is what proves
            // a pool that maintains nothing never prepares anything.
            case: "a pool that maintains nothing at all",
            group: "asg-wp-clock-none",
            max_size: 2,
            min_size: 0,
            prepared_capacity: Some(0),
            maintained: 0,
        },
        Row {
            case: "a pool of one, bounded by the request's own prepared capacity",
            group: "asg-wp-clock-one",
            max_size: 1,
            min_size: 1,
            prepared_capacity: Some(1),
            maintained: 1,
        },
        Row {
            case: "a pool of two",
            group: "asg-wp-clock-two",
            max_size: 2,
            min_size: 2,
            prepared_capacity: Some(2),
            maintained: 2,
        },
        Row {
            case: "a pool left to the group's own maximum",
            group: "asg-wp-clock-maximum",
            max_size: 3,
            min_size: 0,
            prepared_capacity: None,
            maintained: 3,
        },
    ];

    for row in &rows {
        let case = row.case;
        create_group(&router, row.group, row.max_size, 0).await;
        let (status, body) =
            put_warm_pool(&router, row.group, row.min_size, row.prepared_capacity).await;
        assert_eq!(
            status, 200,
            "case {case:?}: PutWarmPool expected 200, got {status} with {body}"
        );

        // The pool is stored by that put and has prepared NOTHING in it: preparing an
        // instance is work this service does after the call, so the read that follows it
        // reports the configuration and an empty instance list.
        assert_eq!(
            describe_pool(case, &router, row.group).await,
            Reported {
                held: true,
                instance_ids: Vec::new(),
                lifecycle_states: Vec::new(),
            },
            "case {case:?}: expected the stored pool to have prepared no instance yet"
        );

        // ...and the instances it maintains appear afterwards, every one of them settling in
        // the pool's own state.
        let settled = pool_settled(case, &router, row.group, row.maintained, &warmed()).await;
        assert_eq!(
            (settled.instance_ids.len(), settled.lifecycle_states.clone()),
            (
                row.maintained,
                vec![warmed(); row.maintained]
            ),
            "case {case:?}: expected the pool to prepare {} instance(s) and settle them in \
             {:?}, got {settled:?}",
            row.maintained,
            warmed()
        );
    }

    // Read every row's pool once more, now that the table has carried each of them well past
    // the span its preparation came due in: a pool maintains exactly what the size rule says
    // and nothing drifts in afterwards — least of all into the pool that maintains none.
    for row in &rows {
        let case = row.case;
        let reported = describe_pool(case, &router, row.group).await;
        assert_eq!(
            (reported.held, reported.instance_ids.len()),
            (true, row.maintained),
            "case {case:?}: expected the pool to still hold exactly {} instance(s), got \
             {reported:?}",
            row.maintained
        );
    }
}

/// RULE: the same holds for a shortfall a CAPACITY CHANGE leaves behind — "the size of the
/// warm pool is dynamic", so a group whose desired capacity drops leaves its pool short, and
/// that difference is prepared after the call that made it rather than within it.
#[tokio::test]
async fn a_capacity_change_that_leaves_the_pool_short_prepares_the_difference_afterwards() {
    let (router, _engine) = emulator();
    let group = "asg-wp-clock-capacity";
    // A group at its own maximum: the difference the pool maintains is zero, so the pool it
    // is given prepares nothing while the group stays there.
    create_group(&router, group, 2, 2).await;
    let (status, body) = put_warm_pool(&router, group, 0, None).await;
    assert_eq!(
        status, 200,
        "PutWarmPool expected 200, got {status} with {body}"
    );
    let case = "a pool whose group is at its maximum";
    let reported = pool_holding(case, &router, group, 0).await;
    assert_eq!(
        (reported.held, reported.instance_ids.len()),
        (true, 0),
        "case {case:?}: expected a stored pool that maintains nothing, got {reported:?}"
    );

    // Taking the group's desired capacity down widens that difference to the group's whole
    // maximum, so the pool is short of two.
    let case = "the capacity change that left the pool short";
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[("AutoScalingGroupName", group), ("DesiredCapacity", "0")],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: SetDesiredCapacity expected 200, got {status} with {body}"
    );
    assert_eq!(
        describe_pool(case, &router, group).await.instance_ids,
        Vec::<String>::new(),
        "case {case:?}: expected the pool to have prepared nothing within the call that left \
         it short"
    );
    let settled = pool_settled(case, &router, group, 2, &warmed()).await;
    assert_eq!(
        (settled.instance_ids.len(), settled.lifecycle_states.clone()),
        (2, vec![warmed(); 2]),
        "case {case:?}: expected the pool to prepare the difference afterwards, got {settled:?}"
    );
}

/// RULE: the preparation is owed by the CLOCK, from the instant the change that left the pool
/// short was made — so it happens with no further call from anybody, and the instant it is
/// owed from does not depend on who read the pool in between.
///
/// Neither of the two shortfalls below is read back through this service at all: the
/// instances a warm pool prepares are the account's own Amazon EC2 instances, so this test
/// waits on the account's Amazon EC2 store, which no request of its own drives.
#[tokio::test]
async fn a_pool_left_short_is_prepared_with_no_further_call() {
    let (router, engine) = emulator();
    // The PUT's own shortfall, on a group that holds no instances of its own, so every
    // instance the Amazon EC2 store comes to hold is one the pool prepared.
    let put_short = "asg-wp-clock-unattended-put";
    create_group(&router, put_short, 1, 0).await;
    let (status, body) = put_warm_pool(&router, put_short, 1, Some(1)).await;
    assert_eq!(
        status, 200,
        "PutWarmPool expected 200, got {status} with {body}"
    );
    assert_eq!(
        ec2_instances_in(&engine, WARMED_EC2_STATES),
        0,
        "the put must have prepared nothing within its own call"
    );
    let held = ec2_holding(&engine, WARMED_EC2_STATES, 1).await;
    assert_eq!(
        held, 1,
        "expected the pool's one instance to be prepared with no further call to this \
         service, got {held} warmed instance(s) in the account's Amazon EC2 store"
    );

    // ...and a CAPACITY CHANGE's shortfall, which is owed by the call that made the change
    // rather than by a later read: the group below is at its maximum, so its pool maintains
    // nothing until the change, and the two instances it then maintains appear without
    // anybody asking this service about the pool again.
    let (router, engine) = emulator();
    let capacity_short = "asg-wp-clock-unattended-capacity";
    create_group(&router, capacity_short, 2, 2).await;
    let (status, body) = put_warm_pool(&router, capacity_short, 0, None).await;
    assert_eq!(
        status, 200,
        "PutWarmPool expected 200, got {status} with {body}"
    );
    // The group's own two instances RUN, and a pool that maintains nothing holds none: the
    // two halves of the account's Amazon EC2 store are told apart by the state each is in,
    // so neither count depends on whether a record Amazon EC2 has finished with is kept.
    let own = ec2_holding(&engine, &["running"], 2).await;
    assert_eq!(
        own, 2,
        "expected the group's own two instances to be running, got {own}"
    );
    assert_eq!(
        ec2_instances_in(&engine, WARMED_EC2_STATES),
        0,
        "expected a pool that maintains nothing to have prepared nothing"
    );
    let (status, body) = query(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", capacity_short),
            ("DesiredCapacity", "0"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "SetDesiredCapacity expected 200, got {status} with {body}"
    );
    // The two the group gives up are shut down on Amazon EC2's own clock, and the two the
    // pool now maintains are prepared on this service's — with no further call to it.
    let prepared = ec2_holding(&engine, WARMED_EC2_STATES, 2).await;
    assert_eq!(
        prepared, 2,
        "expected the capacity change's own shortfall to be prepared with no further call to \
         this service, got {prepared} warmed instance(s) in the account's Amazon EC2 store"
    );
}

/// RULE: a delete that arrives at a pool which has prepared nothing takes that pool without
/// the force delete — there are no instances in it to protect — and the group then holds no
/// pool for any later call to address, which is refused.
#[tokio::test]
async fn a_delete_before_the_pool_has_prepared_anything_takes_the_pool() {
    let (router, _engine) = emulator();
    let group = "asg-wp-clock-delete";
    // The WITNESS: a second group whose pool maintains an instance, so this suite learns that
    // the preparation has come due from the service's own report rather than from a clock.
    let witness = "asg-wp-clock-witness";
    create_group(&router, group, 1, 0).await;
    create_group(&router, witness, 1, 0).await;
    for holder in [group, witness] {
        let (status, body) = put_warm_pool(&router, holder, 1, Some(1)).await;
        assert_eq!(
            status, 200,
            "PutWarmPool for {holder:?} expected 200, got {status} with {body}"
        );
    }

    let case = "a pool that has prepared nothing, deleted without the force";
    let (status, body) = query(
        &router,
        "DeleteWarmPool",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the delete to be accepted, got {status} with {body}"
    );
    assert_eq!(
        describe_pool(case, &router, group).await,
        Reported::default(),
        "case {case:?}: expected the group to hold no pool"
    );

    // The witness has prepared its instance, so the preparation this group's pool was owed
    // has come due too — and nothing was prepared into a pool the delete took away.
    let held = pool_holding("the witness", &router, witness, 1).await;
    assert_eq!(
        held.instance_ids.len(),
        1,
        "the witness pool must have prepared its instance, got {held:?}"
    );
    assert_eq!(
        describe_pool(case, &router, group).await,
        Reported::default(),
        "case {case:?}: expected no instance to be prepared into the pool the delete took"
    );

    // A group that holds no pool holds nothing a delete can address: the one the delete just
    // took, and one that never had a pool at all, are refused with the same sentence.
    let never = "asg-wp-clock-never";
    create_group(&router, never, 1, 0).await;
    for (case, holder) in [
        ("the pool the delete just took", group),
        ("a group that never had a pool", never),
    ] {
        let (status, body) = query(
            &router,
            "DeleteWarmPool",
            &[("AutoScalingGroupName", holder)],
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected the delete to be refused, got {status} with {body}"
        );
        assert_eq!(
            (
                tag_value(&body, "Code"),
                xml_unescape(&tag_value(&body, "Message"))
            ),
            (
                "ValidationError".to_string(),
                absent_warm_pool_message(holder)
            ),
            "case {case:?}: unexpected refusal, got {body}"
        );
    }
}
