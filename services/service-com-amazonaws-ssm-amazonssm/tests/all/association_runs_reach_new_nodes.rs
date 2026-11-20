// SPDX-License-Identifier: CC0-1.0

//! The EXECUTION a State Manager association has when the nodes its targets select change —
//! driven end to end over each service's own wire, with Systems Manager, Amazon EC2 and IAM on
//! one engine ([`super::ec2_instance_managed_nodes`]).
//!
//! RULE under test: an association's targets are CRITERIA rather than a list of nodes, so
//! State Manager applies the association to the nodes they select AS THAT SELECTION CHANGES —
//! a tag target "automatically applies the association to new managed nodes that you add to
//! the tag group" (`Target`). A managed node that registers while an execution is the
//! association's current run is therefore a node that execution reached: it is counted in that
//! execution's count of resources by status and resolved as one of its targets, under the
//! execution id the association already had. What it reports as reached is the nodes this
//! Region STILL holds, so a node whose instance is gone drops out of it again. An execution
//! that reached no node reports the EMPTY count, `{}`, rather than none at all — which is what
//! lets a client tell the two apart — and a selection that does not change leaves the
//! execution where it is, however often it is read.
//!
//! The rows differ along what the rule generalises over: whether the association's targets
//! select the node (a tag the instance carries, a tag value nothing carries, a tag key nothing
//! carries) and whether the node was registered when the association was created.
//!
//! Nothing here waits a fixed time: the registration span and a run's own span are stored due
//! instants, which a case moves into the past through the store the way this crate's other
//! clock-owed suites do, and every read LOOKS AGAIN until the condition holds or a generous
//! deadline passes.

use std::time::{Duration, Instant, SystemTime};

use serde_json::{json, Value};

use super::ec2_instance_managed_nodes::{
    launched_long_enough_ago, Emulator, Grant, AL2023_NAME_PATTERN, REGION, SSM_CORE_POLICY_ARN,
};

/// The published document an association applies.
const RUN_SHELL_SCRIPT: &str = "AWS-RunShellScript";

/// The tag key the association's target names, and the value the instance carries.
const TAG_KEY: &str = "Sim2realEstate";
const TAG_VALUE: &str = "estate-2f91c4";

/// How long a case looks for an execution to settle, far longer than any span the clock owes
/// one, so the case does not depend on how fast the host runs.
const DEADLINE: Duration = Duration::from_secs(60);

/// How long one look-again cycle pauses.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// The empty aggregate an execution that counted no resource reports.
const NO_RESOURCES: &str = "{}";

/// RULE: the execution of an association GAINS a node that registers while it is the
/// association's current run — counting it and resolving it as a target under the execution id
/// the association already had — and an execution that has reached none reports the empty
/// count.
#[tokio::test]
async fn an_association_execution_gains_a_node_that_registered_after_it_was_dispatched() {
    // (what the case is, the target the association carries, whether it selects the node)
    let rows: &[(&str, Value, bool)] = &[
        (
            "a tag target the instance carries",
            json!({"Key": format!("tag:{TAG_KEY}"), "Values": [TAG_VALUE]}),
            true,
        ),
        (
            "a tag target naming a value nothing carries",
            json!({"Key": format!("tag:{TAG_KEY}"), "Values": ["another-estate"]}),
            false,
        ),
        (
            "a tag target naming a key nothing carries",
            json!({"Key": "tag:AnotherKey", "Values": [TAG_VALUE]}),
            false,
        ),
    ];
    for (case, target, selects) in rows {
        let emulator = Emulator::new();
        let image = emulator.image(case, AL2023_NAME_PATTERN).await;
        let profile = emulator
            .profile(case, "estate-role", &Grant::Attached(SSM_CORE_POLICY_ARN))
            .await;
        // The instance is launched BEFORE the association is created, and its registration is
        // not due yet: the association's first run therefore reaches nothing, which is the
        // position a template that creates both at once leaves it in.
        let instance = launch_tagged(&emulator, case, &image, &profile).await;
        let association = created(&emulator, case, target).await;
        let first = executions(&emulator, case, &association).await;
        assert_eq!(
            first.len(),
            1,
            "case {case:?}: the create dispatches the association's one run, got {first:?}"
        );
        assert_eq!(
            first[0]["ResourceCountByStatus"],
            json!(NO_RESOURCES),
            "case {case:?}: the run reached no node, so it reports the empty count, got {:?}",
            first[0]
        );
        // The execution id a client holds from here on: what it reads targets by, and what it
        // must still read them by once the node has registered.
        let execution_id = first[0]["ExecutionId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the execution must report its id: {first:?}"))
            .to_string();

        // Now the clock: the instance's registration falls due, and the run the association
        // already had is one the clock has brought to its end.
        emulator.launched_ago(&instance, launched_long_enough_ago());
        finish_the_run(&emulator, case, &association);

        if !*selects {
            // A selection that never changes is never counted, however often it is read.
            let reported = executions(&emulator, case, &association).await;
            let again = executions(&emulator, case, &association).await;
            assert_eq!(
                again.len(),
                1,
                "case {case:?}: reading must not dispatch a further run, got {reported:?} then \
                 {again:?}"
            );
            assert_eq!(
                again[0]["ResourceCountByStatus"],
                json!(NO_RESOURCES),
                "case {case:?}: the run still counted nothing, got {:?}",
                again[0]
            );
            assert!(
                targets_of(&emulator, case, &association, &execution_id)
                    .await
                    .is_empty(),
                "case {case:?}: the execution must resolve no target at all"
            );
            continue;
        }

        let counted = look_until(&emulator, case, &association, |reported| {
            reported
                .first()
                .is_some_and(|entry| entry["ResourceCountByStatus"] != json!(NO_RESOURCES))
        })
        .await;
        assert_eq!(
            counted.len(),
            1,
            "case {case:?}: the node is reached by the run the association already had, \
             got {counted:?}"
        );
        assert_eq!(
            counted[0]["ExecutionId"].as_str(),
            Some(execution_id.as_str()),
            "case {case:?}: the execution keeps the id it was dispatched under, got {:?}",
            counted[0]
        );
        assert_eq!(
            counted[0]["ResourceCountByStatus"],
            json!("{Success=1}"),
            "case {case:?}: expected the one node it reached counted, got {:?}",
            counted[0]
        );
        assert_eq!(
            counted[0]["Status"],
            json!("Success"),
            "case {case:?}: expected the finished run's own status, got {:?}",
            counted[0]
        );
        // Reading again changes nothing: the targets a run reached only ever grow, so two
        // reads of one association report the same execution.
        let again = executions(&emulator, case, &association).await;
        assert_eq!(
            again, counted,
            "case {case:?}: two reads of the association must report the same executions"
        );

        // And the execution RESOLVES that node as its target, under the id the client already
        // held, with the kind the node store holds for it.
        let targets = targets_of(&emulator, case, &association, &execution_id).await;
        let resolved: Vec<&str> = targets
            .iter()
            .filter_map(|target| target["ResourceId"].as_str())
            .collect();
        assert_eq!(
            resolved,
            vec![instance.as_str()],
            "case {case:?}: expected the execution to resolve the node it reached, got \
             {targets:?}"
        );
        assert_eq!(
            targets[0]["ResourceType"],
            json!("EC2Instance"),
            "case {case:?}: expected the kind the node store holds, got {:?}",
            targets[0]
        );
    }
}

/// RULE (the other side of the same settle): what an execution reports as reached is the nodes
/// THIS REGION STILL HOLDS, so a node whose instance is gone — which Systems Manager no longer
/// holds a managed node for at all — is no longer reported as a target of it and no longer
/// counted in it. That is what bounds a run's reached nodes by the nodes the node store holds
/// rather than by every instance ever launched.
#[tokio::test]
async fn an_execution_stops_reporting_a_node_the_account_no_longer_holds() {
    let case = "the instance that is terminated after the execution reached it";
    let emulator = Emulator::new();
    let image = emulator.image(case, AL2023_NAME_PATTERN).await;
    let profile = emulator
        .profile(case, "estate-gone-role", &Grant::Attached(SSM_CORE_POLICY_ARN))
        .await;
    let instance = launch_tagged(&emulator, case, &image, &profile).await;
    let association = created(
        &emulator,
        case,
        &json!({"Key": format!("tag:{TAG_KEY}"), "Values": [TAG_VALUE]}),
    )
    .await;
    emulator.launched_ago(&instance, launched_long_enough_ago());
    finish_the_run(&emulator, case, &association);
    let counted = look_until(&emulator, case, &association, |reported| {
        reported
            .first()
            .is_some_and(|entry| entry["ResourceCountByStatus"] != json!(NO_RESOURCES))
    })
    .await;
    let execution_id = counted[0]["ExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the execution must report its id"))
        .to_string();
    assert_eq!(
        targets_of(&emulator, case, &association, &execution_id)
            .await
            .len(),
        1,
        "case {case:?}: the execution must have reached the node first"
    );

    // The one change: the instance is terminated, so Amazon EC2 no longer holds it running and
    // Systems Manager holds no managed node for it.
    emulator
        .ec2(
            case,
            "TerminateInstances",
            &[("InstanceId.1", instance.as_str())],
        )
        .await;
    let emptied = look_until(&emulator, case, &association, |reported| {
        reported
            .first()
            .is_some_and(|entry| entry["ResourceCountByStatus"] == json!(NO_RESOURCES))
    })
    .await;
    assert_eq!(
        emptied.len(),
        1,
        "case {case:?}: the association still has its one execution, got {emptied:?}"
    );
    assert_eq!(
        emptied[0]["ExecutionId"].as_str(),
        Some(execution_id.as_str()),
        "case {case:?}: and it is the same execution, got {:?}",
        emptied[0]
    );
    let targets = targets_of(&emulator, case, &association, &execution_id).await;
    assert!(
        targets.is_empty(),
        "case {case:?}: expected no target for a node the account no longer holds, got \
         {targets:?}"
    );
}

/// Launch one instance running as `profile` and carrying the tag the association targets, as
/// the instance id EC2 minted.
async fn launch_tagged(emulator: &Emulator, case: &str, image_id: &str, profile: &str) -> String {
    let body = emulator
        .ec2(
            case,
            "RunInstances",
            &[
                ("ImageId", image_id),
                ("InstanceType", "t3.micro"),
                ("MinCount", "1"),
                ("MaxCount", "1"),
                ("IamInstanceProfile.Name", profile),
                ("TagSpecification.1.ResourceType", "instance"),
                ("TagSpecification.1.Tag.1.Key", TAG_KEY),
                ("TagSpecification.1.Tag.1.Value", TAG_VALUE),
            ],
        )
        .await;
    Emulator::answered(&body, "instanceId")
        .unwrap_or_else(|| panic!("case {case:?}: RunInstances must mint an instance: {body}"))
}

/// Create one association applying the published shell document to `target`, as the id the
/// create minted.
async fn created(emulator: &Emulator, case: &str, target: &Value) -> String {
    let (status, answered) = emulator
        .ssm(
            REGION,
            "CreateAssociation",
            &json!({
                "Name": RUN_SHELL_SCRIPT,
                "Targets": [target],
                "Parameters": {"commands": ["echo applied"]},
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAssociation must be served, got {status}: {answered}"
    );
    answered["AssociationDescription"]["AssociationId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create must mint an id: {answered}"))
        .to_string()
}

/// The executions this service reports for `association_id`, most recent first.
async fn executions(emulator: &Emulator, case: &str, association_id: &str) -> Vec<Value> {
    let (status, answered) = emulator
        .ssm(
            REGION,
            "DescribeAssociationExecutions",
            &json!({"AssociationId": association_id}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeAssociationExecutions must be served, got {status}: {answered}"
    );
    answered["AssociationExecutions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The targets this service resolves for one execution of an association.
async fn targets_of(
    emulator: &Emulator,
    case: &str,
    association_id: &str,
    execution_id: &str,
) -> Vec<Value> {
    let (status, answered) = emulator
        .ssm(
            REGION,
            "DescribeAssociationExecutionTargets",
            &json!({"AssociationId": association_id, "ExecutionId": execution_id}),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the targets read must be served, got {status}: {answered}"
    );
    answered["AssociationExecutionTargets"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Move the instant the association's run was dispatched at far enough into the past that the
/// run has finished, so a case about WHAT that run reached is not spent waiting out the span
/// the clock owes it — the way this crate's other clock-owed suites move a stored due instant.
fn finish_the_run(emulator: &Emulator, case: &str, association_id: &str) {
    let state = emulator.ssm_state();
    let mut store = state.lock();
    let association = store
        .association_mut(REGION, association_id)
        .unwrap_or_else(|| panic!("case {case:?}: the store must hold the association"));
    let run = association
        .last_run_mut()
        .unwrap_or_else(|| panic!("case {case:?}: the create must have dispatched a run"));
    run.started_at = SystemTime::now() - Duration::from_secs(60);
}

/// Read the association's executions again until `settled` holds, or [`DEADLINE`] passes.
async fn look_until(
    emulator: &Emulator,
    case: &str,
    association_id: &str,
    settled: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = executions(emulator, case, association_id).await;
        if settled(&reported) {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the association's execution to settle on the nodes it \
             reaches with nothing but the clock running, got {reported:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}
