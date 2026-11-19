// SPDX-License-Identifier: CC0-1.0

//! What a refused `TerminateJobFlows` leaves behind, end to end over the wire and across
//! the service that OWNS a cluster's capacity.
//!
//! THE RULE this suite asserts: a shutdown request naming a cluster whose termination
//! protection holds is refused in whole — HTTP 400 on the `ValidationException` channel
//! with Amazon EMR's one constant sentence — and it RELEASES NOTHING of any cluster the
//! request named: the lock "prevent[s] the Amazon EC2 instances from being terminated by
//! an API call or user intervention" (`Cluster$TerminationProtected` doc), so each named
//! cluster keeps its status, its step queue, and the Amazon EC2 instances it was
//! provisioned from, in Amazon EC2 itself. Only "a subsequent call to
//! SetTerminationProtection in which you set the value to false"
//! (`SetTerminationProtection` doc) lets the shutdown through, and THEN those same
//! instances are released — which is what makes the assertions above non-vacuous.
//!
//! The sibling suites assert the other halves of the lock: `tests/all/termination_protection.rs`
//! the WRITE of it, `tests/all/terminate_job_flows_protection.rs` the batch the refusal is
//! decided over, and `tests/all/multi_primary_termination_protection.rs` the lock a
//! highly available launch forces on. This one spans what the refusal must NOT have
//! destroyed, which only the owning service can answer for.
//!
//! Every case drives the real HTTP path — `POST /` with
//! `X-Amz-Target: ElasticMapReduce.<Op>` and an `awsJson1_1` body — through the router's
//! `dispatch`, with Amazon EC2 mounted BESIDE Amazon EMR on one engine, through the
//! harness `tests/all/cluster_instances.rs` owns. Nothing reaches into either service's
//! state struct: a cluster is read back through `DescribeCluster`, `ListSteps` and
//! `ListInstances`, and its instances are resolved in Amazon EC2 through
//! `DescribeInstances`.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use serde_json::{json, Value};

use crate::cluster_instances::{
    described_in_ec2, ec2_ids, emr, emr_call, emulator, instances_in_state, launch, list_instances,
    subnet, Emulator, POLL_INTERVAL, WAIT_DEADLINE,
};
use crate::job_flow_lifecycle::assert_refused;
use crate::refusal_messages::message;
use crate::terminate_job_flows_protection::{assert_not_shut_down, PROTECTED_SENTENCE};

/// The step states a SERVED shutdown of the cluster brings — "any step not yet completed
/// is canceled" (`TerminateJobFlows` doc): a step that had not started is `CANCELLED`, one
/// that was running is `INTERRUPTED`, and one on its way to either is `CANCEL_PENDING`
/// (`com.amazonaws.emr#StepState`). A REFUSED shutdown must bring none of them, whatever
/// the clock has done with the queue meanwhile.
const CANCELLED_BY_A_SHUTDOWN: [&str; 3] = ["CANCELLED", "INTERRUPTED", "CANCEL_PENDING"];

/// The Amazon EC2 instance states that mean an instance is going away or gone
/// (`ec2#InstanceStateName`), which is what a cluster's shutdown does to its capacity.
const RELEASED_IN_EC2: [&str; 2] = ["shutting-down", "terminated"];

/// One row: how the cluster came to hold the lock, the capacity its launch asks for, and
/// how many Amazon EC2 instances that comes to.
struct Case {
    label: &'static str,
    /// The `Instances` member of the launch, without `Ec2SubnetId` or
    /// `KeepJobFlowAliveWhenNoSteps`, which the shared harness fills in.
    instances: Value,
    /// Whether the lock is written by `SetTerminationProtection` once the cluster is up,
    /// rather than carried by the launch or forced by its primary count.
    locked_by_the_api: bool,
    /// How many instances the launch provisions.
    instances_launched: usize,
}

/// The rows span what the rule generalises over: how the lock arose (the launch asked for
/// it, the API wrote it on a live cluster, or more than one primary node forced it), the
/// form the launch states its capacity in (the uniform `InstanceCount` form, instance
/// groups, an instance fleet) and how many instances it provisions.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "the launch asked for the lock",
            instances: json!({
                "InstanceCount": 1,
                "MasterInstanceType": "m5.xlarge",
                "TerminationProtected": true,
            }),
            locked_by_the_api: false,
            instances_launched: 1,
        },
        Case {
            label: "the lock written on a cluster that is already up",
            instances: json!({
                "InstanceCount": 2,
                "MasterInstanceType": "m5.xlarge",
                "SlaveInstanceType": "m5.xlarge",
                "TerminationProtected": false,
            }),
            locked_by_the_api: true,
            instances_launched: 2,
        },
        Case {
            label: "three primaries in instance groups, protection declined",
            instances: json!({
                "TerminationProtected": false,
                "InstanceGroups": [
                    { "InstanceRole": "MASTER", "InstanceType": "m5.xlarge", "InstanceCount": 3 },
                    { "InstanceRole": "CORE", "InstanceType": "m5.xlarge", "InstanceCount": 1 },
                ],
            }),
            locked_by_the_api: false,
            instances_launched: 4,
        },
        Case {
            label: "two primaries in an instance fleet, protection not mentioned",
            instances: json!({
                "InstanceFleets": [{
                    "InstanceFleetType": "MASTER",
                    "TargetOnDemandCapacity": 2,
                    "InstanceTypeConfigs": [{ "InstanceType": "m5.xlarge" }],
                }],
            }),
            locked_by_the_api: false,
            instances_launched: 2,
        },
    ]
}

/// The cluster `DescribeCluster` answers for `cluster_id`.
async fn describe_cluster(emulator: &Emulator, cluster_id: &str) -> Value {
    emr(
        &emulator.router,
        "DescribeCluster",
        json!({ "ClusterId": cluster_id }),
    )
    .await["Cluster"]
        .clone()
}

/// Look for `cluster_id` to report `state`, until a deadline far above every span this
/// service owes a cluster passes: the condition is waited FOR rather than waited out, so a
/// loaded host delays a case rather than failing it.
async fn await_cluster_state(emulator: &Emulator, case: &str, cluster_id: &str, state: &str) {
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        let cluster = describe_cluster(emulator, cluster_id).await;
        if cluster["Status"]["State"].as_str() == Some(state) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {cluster_id} never reached {state}, last {cluster}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// What Amazon EC2 reports each of `ec2_instance_ids` as being in, by instance id, read
/// through the shared cross-service read — an id Amazon EC2 does not hold is absent, so a
/// missing instance fails an assertion rather than panicking here.
async fn ec2_states(emulator: &Emulator, ec2_instance_ids: &[String]) -> BTreeMap<String, String> {
    described_in_ec2(emulator, ec2_instance_ids)
        .await
        .into_iter()
        .map(|(id, instance)| (id, instance.state))
        .collect()
}

/// That Amazon EC2 still holds every one of `ec2_instance_ids` as an instance that is
/// neither going away nor gone — the capacity a refused shutdown may not have released.
async fn assert_still_running_in_ec2(emulator: &Emulator, case: &str, ec2_instance_ids: &[String]) {
    assert!(
        !ec2_instance_ids.is_empty(),
        "case {case:?}: the cluster must have been provisioned from some Amazon EC2 instances"
    );
    let held = ec2_states(emulator, ec2_instance_ids).await;
    for id in ec2_instance_ids {
        let state = held
            .get(id)
            .unwrap_or_else(|| panic!("case {case:?}: Amazon EC2 holds no instance {id:?}"));
        assert!(
            !RELEASED_IN_EC2.contains(&state.as_str()),
            "case {case:?}: {id} must still be running in Amazon EC2, got state {state:?}"
        );
    }
}

/// That nothing of `cluster_id` was released: nothing shut the cluster down
/// ([`assert_not_shut_down`]), no step of its queue carries a state only a served shutdown
/// brings, and `ListInstances` reports exactly the instances it reported before — none of
/// them ended, and every one of them still held by Amazon EC2.
async fn assert_nothing_released(
    emulator: &Emulator,
    case: &str,
    cluster_id: &str,
    ec2_instance_ids: &[String],
) {
    let cluster = describe_cluster(emulator, cluster_id).await;
    assert_not_shut_down(case, cluster_id, &cluster);

    let steps = emr(
        &emulator.router,
        "ListSteps",
        json!({ "ClusterId": cluster_id }),
    )
    .await["Steps"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for step in &steps {
        let step_state = step["Status"]["State"].as_str().unwrap_or_default();
        assert!(
            !CANCELLED_BY_A_SHUTDOWN.contains(&step_state),
            "case {case:?}: no step of {cluster_id} may be stopped by a refused shutdown, \
             got state {step_state:?} in {step}"
        );
    }

    let listed = list_instances(emulator, case, cluster_id, json!({})).await;
    let still_there: BTreeSet<String> = ec2_ids(&listed).into_iter().collect();
    let expected: BTreeSet<String> = ec2_instance_ids.iter().cloned().collect();
    assert_eq!(
        still_there, expected,
        "case {case:?}: {cluster_id} must still report the instances it was provisioned from"
    );
    for instance in &listed {
        let instance_state = instance["Status"]["State"].as_str().unwrap_or_default();
        assert_ne!(
            instance_state, "TERMINATED",
            "case {case:?}: no instance of {cluster_id} may be terminated by a refused \
             shutdown, got {instance}"
        );
        assert!(
            instance["Status"]["Timeline"]["EndDateTime"].is_null(),
            "case {case:?}: no instance of {cluster_id} has ended, got {instance}"
        );
    }
    assert_still_running_in_ec2(emulator, case, ec2_instance_ids).await;
}

/// Look for every one of `ec2_instance_ids` to be going away or gone in Amazon EC2, until
/// a deadline far above the span the clock owes a shutdown passes — the condition is waited
/// FOR rather than waited out, so a loaded host delays this rather than failing it.
async fn await_released_in_ec2(emulator: &Emulator, case: &str, ec2_instance_ids: &[String]) {
    let deadline = Instant::now() + WAIT_DEADLINE;
    loop {
        let held = ec2_states(emulator, ec2_instance_ids).await;
        let released = ec2_instance_ids.iter().all(|id| {
            held.get(id)
                .is_some_and(|state| RELEASED_IN_EC2.contains(&state.as_str()))
        });
        if released {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: every instance of the cluster must be released in Amazon EC2 once \
             the shutdown is served, last {held:?}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// THE RULE: the refusal releases nothing — not the cluster, not its queue, and not the
/// Amazon EC2 instances it runs on — for any cluster the batch names; clearing the lock is
/// what releases them.
#[tokio::test]
async fn a_refused_terminate_releases_no_capacity_of_any_cluster_it_names() {
    let emulator = emulator();
    let subnet_id = subnet(&emulator).await;
    let cases = cases();

    // One unprotected cluster, named in every batch below BESIDE the locked one: the whole
    // batch is judged before anything is shut down, so this cluster's capacity must survive
    // each refusal too.
    let unlocked = launch(
        &emulator,
        "unlocked companion",
        &subnet_id,
        json!({ "InstanceCount": 1, "MasterInstanceType": "m5.xlarge" }),
    )
    .await;

    // Every cluster is launched first, so the spans the clock owes them all to come up pass
    // together rather than one after another.
    let mut launched: Vec<String> = Vec::new();
    for case in &cases {
        launched.push(launch(&emulator, case.label, &subnet_id, case.instances.clone()).await);
    }

    let unlocked_instances = ec2_ids(
        &instances_in_state(&emulator, "unlocked companion", &unlocked, 1, "RUNNING").await,
    );

    for (index, case) in cases.iter().enumerate() {
        let label = case.label;
        let cluster_id = &launched[index];

        // The lock is asserted on a cluster that has come all the way up, with its capacity
        // running — the state a long-running cluster is locked in.
        await_cluster_state(&emulator, label, cluster_id, "WAITING").await;
        let running = instances_in_state(
            &emulator,
            label,
            cluster_id,
            case.instances_launched,
            "RUNNING",
        )
        .await;
        let instance_ids = ec2_ids(&running);
        if case.locked_by_the_api {
            emr(
                &emulator.router,
                "SetTerminationProtection",
                json!({ "JobFlowIds": [cluster_id], "TerminationProtected": true }),
            )
            .await;
        }
        let cluster = describe_cluster(&emulator, cluster_id).await;
        assert_eq!(
            cluster["TerminationProtected"].as_bool(),
            Some(true),
            "case {label:?}: the cluster must hold the lock before the shutdown, got {cluster}"
        );

        // A step on the queue, which a SERVED shutdown would cancel.
        let submitted = emr(
            &emulator.router,
            "AddJobFlowSteps",
            json!({
                "JobFlowId": cluster_id,
                "Steps": [{
                    "Name": "work the refusal must not cancel",
                    "HadoopJarStep": { "Jar": "s3://jars/work.jar", "Args": ["--input", "s3://in"] },
                }],
            }),
        )
        .await;
        assert_eq!(
            submitted["StepIds"].as_array().map(Vec::len),
            Some(1),
            "case {label:?}: the step must be submitted, got {submitted}"
        );

        // The locked cluster sits at a different place in the batch on each row, and the
        // unprotected companion is always beside it.
        let batch: Vec<&String> = if index % 2 == 0 {
            vec![&unlocked, cluster_id]
        } else {
            vec![cluster_id, &unlocked]
        };
        let (status, payload) = emr_call(
            &emulator.router,
            "TerminateJobFlows",
            json!({ "JobFlowIds": batch }),
        )
        .await;
        assert_refused(label, status, &payload, "ValidationException");
        assert_eq!(
            message(label, &payload),
            PROTECTED_SENTENCE,
            "case {label:?}: expected {PROTECTED_SENTENCE:?}, got {payload}"
        );

        // Neither cluster the batch named lost anything.
        assert_nothing_released(&emulator, label, cluster_id, &instance_ids).await;
        assert_nothing_released(
            &emulator,
            &format!("{label}, the companion beside it"),
            &unlocked,
            &unlocked_instances,
        )
        .await;

        // The control: clearing the lock lets the identical call through, and THEN the
        // cluster's capacity is released in the service that owns it — so the assertions
        // above are about the refusal and not about a shutdown this emulator never performs.
        emr(
            &emulator.router,
            "SetTerminationProtection",
            json!({ "JobFlowIds": [cluster_id], "TerminationProtected": false }),
        )
        .await;
        emr(
            &emulator.router,
            "TerminateJobFlows",
            json!({ "JobFlowIds": [cluster_id] }),
        )
        .await;
        let shutting_down = describe_cluster(&emulator, cluster_id).await;
        assert_eq!(
            shutting_down["Status"]["State"].as_str(),
            Some("TERMINATING"),
            "case {label:?}: the unlocked cluster must start shutting down, got {shutting_down}"
        );
        await_released_in_ec2(&emulator, label, &instance_ids).await;

        // And the companion, which no served shutdown named, is still up with its own
        // instance running.
        assert_nothing_released(
            &emulator,
            &format!("{label}, the companion after the served shutdown"),
            &unlocked,
            &unlocked_instances,
        )
        .await;
    }
}
