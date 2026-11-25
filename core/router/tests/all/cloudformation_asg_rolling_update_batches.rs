// SPDX-License-Identifier: CC0-1.0

//! The BATCHED replacement an `UpdatePolicy`'s `AutoScalingRollingUpdate` asks a stack update
//! for: what a client watching Amazon EC2 Auto Scaling sees while the update runs, and what
//! decides whether the update gets through at all.
//!
//! The RULE under test: when an `UpdateStack` changes the LAUNCH CONFIGURATION of an
//! `AWS::AutoScaling::AutoScalingGroup` whose declaration carries an
//! `AutoScalingRollingUpdate`, CloudFormation does not swap the group's instances in one go.
//! It works through them a BATCH at a time — at most `MaxBatchSize` of them, and never so many
//! that fewer than `MinInstancesInService` are left serving — terminating the batch's
//! instances so the group launches replacements from the configuration the update put in
//! force, and starting the next batch only once that batch has come through. With
//! `WaitOnResourceSignals`, "the Auto Scaling group waits on signals from new instances during
//! an update": each replacement has to signal (`SignalResource` with
//! `Status=SUCCESS`) before the batch counts as through, and a replacement that signals
//! `FAILURE` cancels the update — "a failure signal causes CloudFormation to immediately fail
//! the stack creation or update" (`SignalResource$Status`) — which rolls the stack back to the
//! configuration it started from.
//!
//! This needs CloudFormation, Amazon EC2 Auto Scaling and Amazon EC2 to agree over one engine
//! — the stack declares the launch template and the group, that service launches the
//! replacements and Amazon EC2 runs them — which is why the suite lives here.
//!
//! The rows span what the rule generalises over: how many instances the group holds, how many
//! one batch may take, how many must stay in service — which is what CAPS a batch below
//! `MaxBatchSize` — and whether the policy waits on the replacements' own signals at all.
//! Nothing here waits for a span to pass: every wait polls for the CONDITION it is about, and
//! a signalled row cannot finish at all until this suite signals, which is what makes each
//! batch boundary observable rather than raced for.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{block_of, element, elements, form_encode, members_of, outputs_of};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The API versions the three services' query requests carry.
const CFN_VERSION: &str = "2010-05-15";
const ASG_VERSION: &str = "2011-01-01";

/// The image and instance type the declared launch data names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// The lifecycle state a group reports an instance of its own capacity in once it is serving
/// ("`InService` — The instance is running and in service", `Instance$LifecycleState`).
const IN_SERVICE: &str = "InService";

/// The logical id the template declares the group under, which is what a signal names it by
/// (`SignalResource$LogicalResourceId`).
const GROUP: &str = "Asg";

/// How long a wait gives a condition the emulator owes, and how often it looks. Generous — the
/// whole suite settles in seconds — and only here so a failure is a failure rather than a
/// hang.
const DEADLINE: Duration = Duration::from_secs(180);
const POLL: Duration = Duration::from_millis(25);

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// Call one query operation of `service`, answered with its status and XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One CloudFormation operation that must succeed.
async fn cfn(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "cloudformation", CFN_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "CloudFormation {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Amazon EC2 Auto Scaling operation that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "autoscaling", ASG_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One row of the table: a group of `desired` instances whose declaration asks for the batch
/// size, the floor and the signal gate named here.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// `DesiredCapacity`: how many instances the group holds, and the size the replacement
    /// leaves it at.
    desired: usize,
    /// `MaxSize`: "the maximum size of the group", which is the ceiling a replacement that has
    /// to LAUNCH before it can terminate is bounded by.
    max_size: usize,
    /// `MaxBatchSize`: "the maximum number of instances that CloudFormation updates".
    max_batch_size: usize,
    /// `MinInstancesInService`: "the minimum number of instances that must be in service
    /// within the Auto Scaling group while CloudFormation updates old instances".
    min_instances_in_service: usize,
    /// `WaitOnResourceSignals`.
    wait_on_signals: bool,
    /// How many instances each batch of this row really replaces: `MaxBatchSize`, capped by
    /// what the group can spare above `MinInstancesInService` once it holds [`Self::working`].
    batch: usize,
    /// How many instances the group holds WHILE the replacement runs: its declared capacity,
    /// or more where the floor leaves it nothing to spare — a group that must keep every
    /// instance it has in service launches the replacement first, up to `MaxSize`, which is
    /// why "this value must be less than the `MaxSize` of the Auto Scaling group".
    working: usize,
}

impl Row {
    /// How many batches this row's replacement takes.
    fn batches(&self) -> usize {
        self.desired.div_ceil(self.batch)
    }

    /// The template this row deploys, with `generation` deciding the launch template's
    /// `UserData` — "the user data to make available to the instance", which is part of the
    /// launch template's data, so changing it gives the template a new VERSION and the group's
    /// own `LaunchTemplate` property a new version to point at: a change to the launch
    /// configuration, which is what the policy applies to.
    fn template(&self, generation: &str) -> String {
        let Row {
            stack,
            desired,
            max_size,
            max_batch_size,
            min_instances_in_service,
            wait_on_signals,
            ..
        } = self;
        // `PauseTime` is what "CloudFormation should wait for the Auto Scaling group to
        // receive the required number of signals" in, so a signalled row states a span far
        // longer than this suite takes to signal: the gate is released by the signal, never by
        // a span running out.
        let pause_time = if *wait_on_signals { "PT10M" } else { "PT0S" };
        format!(
            r#"{{"Resources":{{
  "Vpc":{{"Type":"AWS::EC2::VPC","Properties":{{"CidrBlock":"10.0.0.0/16"}}}},
  "Subnet":{{"Type":"AWS::EC2::Subnet","Properties":{{
    "VpcId":{{"Ref":"Vpc"}},"CidrBlock":"10.0.0.0/24","AvailabilityZone":"{REGION}a"}}}},
  "Sg":{{"Type":"AWS::EC2::SecurityGroup","Properties":{{
    "GroupDescription":"the instances of the declared group","VpcId":{{"Ref":"Vpc"}}}}}},
  "Lt":{{"Type":"AWS::EC2::LaunchTemplate","Properties":{{
    "LaunchTemplateName":"{stack}-lt",
    "LaunchTemplateData":{{"ImageId":"{IMAGE_ID}","InstanceType":"{INSTANCE_TYPE}",
      "SecurityGroupIds":[{{"Fn::GetAtt":["Sg","GroupId"]}}],
      "UserData":{{"Fn::Base64":"generation {generation}"}}}}}}}},
  "{GROUP}":{{"Type":"AWS::AutoScaling::AutoScalingGroup",
    "UpdatePolicy":{{"AutoScalingRollingUpdate":{{
      "MaxBatchSize":{max_batch_size},
      "MinInstancesInService":{min_instances_in_service},
      "MinSuccessfulInstancesPercent":100,
      "PauseTime":"{pause_time}",
      "WaitOnResourceSignals":{wait_on_signals}}}}},
    "Properties":{{
    "MinSize":"1","MaxSize":"{max_size}","DesiredCapacity":"{desired}",
    "LaunchTemplate":{{"LaunchTemplateId":{{"Ref":"Lt"}},
      "Version":{{"Fn::GetAtt":["Lt","LatestVersionNumber"]}}}},
    "VPCZoneIdentifier":[{{"Ref":"Subnet"}}]}}}}}},
  "Outputs":{{
    "AsgName":{{"Value":{{"Ref":"{GROUP}"}}}},
    "LtVersion":{{"Value":{{"Fn::GetAtt":["Lt","LatestVersionNumber"]}}}}}}}}"#
        )
    }
}

/// The signalled rows: the replacement is gated on a signal of its own, so every batch
/// boundary is a state this suite observes rather than races for.
const SIGNALLED: &[Row] = &[
    Row {
        case: "three instances replaced one at a time, one of them always in service",
        stack: "rolling-update-three-by-one",
        desired: 3,
        max_size: 4,
        max_batch_size: 1,
        min_instances_in_service: 1,
        wait_on_signals: true,
        batch: 1,
        // The group can spare one instance above the floor, so it replaces in place and never
        // holds more than it was declared with.
        working: 3,
    },
    Row {
        // `MinInstancesInService` is what caps this one: the policy asks for two at a time, and
        // a group of three that must keep two serving can only spare one — and its `MaxSize`
        // leaves no room to launch ahead, so the batch is capped rather than made up for.
        case: "a batch of two capped to one by the floor that must stay in service",
        stack: "rolling-update-capped-by-the-floor",
        desired: 3,
        max_size: 3,
        max_batch_size: 2,
        min_instances_in_service: 2,
        wait_on_signals: true,
        batch: 1,
        working: 3,
    },
    Row {
        // A group that must keep EVERY instance it has in service can spare none, so the
        // replacement launches before it terminates: the group holds one more than it was
        // declared with while the replacement runs, and is back to its declared capacity after.
        case: "a group that must keep every instance in service launches before it terminates",
        stack: "rolling-update-launches-first",
        desired: 2,
        max_size: 3,
        max_batch_size: 1,
        min_instances_in_service: 2,
        wait_on_signals: true,
        batch: 1,
        working: 3,
    },
];

/// The row whose policy waits on NO signal: the replacement is CloudFormation's own work from
/// end to end, so the update gets through without this suite sending anything.
const UNSIGNALLED: Row = Row {
    case: "two instances replaced in one batch, waiting on no signal",
    stack: "rolling-update-two-at-once",
    desired: 2,
    max_size: 3,
    max_batch_size: 2,
    min_instances_in_service: 0,
    wait_on_signals: false,
    batch: 2,
    working: 2,
};

/// Whether a stack status is one the stack settles at, rather than one it passes through.
fn settled(status: &str) -> bool {
    status.ends_with("_COMPLETE") || status.ends_with("_FAILED")
}

/// The stack's status right now.
async fn stack_status(router: &MultiServiceRouter, stack: &str) -> String {
    let body = cfn(router, "DescribeStacks", &[("StackName", stack)]).await;
    element(&body, "StackStatus")
}

/// Poll `DescribeStacks` until the stack reports `wanted`, answering the body it reported it
/// in — or the OTHER settled status it reached instead, so a deployment that did not get there
/// fails with what the stack has to say rather than on the deadline.
async fn wait_for_status(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    wanted: &str,
    through: &[&str],
) -> (String, String) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cfn(router, "DescribeStacks", &[("StackName", stack)]).await;
        let status = element(&body, "StackStatus");
        if status == wanted || (settled(&status) && !through.contains(&status.as_str())) {
            return (status, body);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the stack never reached {wanted}: it is {status:?}, with the \
             failures {:?}",
            failed_events(router, stack).await
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The stack's FAILED resource events, each as `logical id: status: reason` — what a
/// deployment that did not complete has to say for itself.
async fn failed_events(router: &MultiServiceRouter, stack: &str) -> Vec<String> {
    let body = cfn(router, "DescribeStackEvents", &[("StackName", stack)]).await;
    members_of(&body)
        .into_iter()
        .filter_map(|event| {
            let status = elements(event, "ResourceStatus").into_iter().next()?;
            if !status.contains("FAILED") {
                return None;
            }
            let logical_id = elements(event, "LogicalResourceId").into_iter().next()?;
            let reason = elements(event, "ResourceStatusReason")
                .into_iter()
                .next()
                .unwrap_or_default();
            Some(format!("{logical_id}: {status}: {reason}"))
        })
        .collect()
}

/// The ids of the instances the group reports IN SERVICE right now.
///
/// Each entry is read inside the `<member>` that carries BOTH its id and its lifecycle state,
/// scoped to the group's own `Instances` list, so an id is never paired with another entry's
/// state.
async fn in_service(router: &MultiServiceRouter, group: &str) -> BTreeSet<String> {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group)],
    )
    .await;
    let mut serving = BTreeSet::new();
    for entry in block_of(&body, "Instances")
        .map(members_of)
        .unwrap_or_default()
    {
        let state = elements(entry, "LifecycleState").into_iter().next();
        let instance_id = elements(entry, "InstanceId").into_iter().next();
        if let (Some(state), Some(instance_id)) = (state, instance_id) {
            if state == IN_SERVICE {
                serving.insert(instance_id);
            }
        }
    }
    serving
}

/// Poll the group until it reports `wanted` instances in service, answering their ids.
async fn wait_for_capacity(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> BTreeSet<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let serving = in_service(router, group).await;
        if serving.len() >= wanted {
            return serving;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {group} to report {wanted} {IN_SERVICE} instance(s), got \
             {serving:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Deploy one row's stack and answer the group's name and the instances it is serving with.
async fn deployed(router: &MultiServiceRouter, row: &Row) -> (String, BTreeSet<String>) {
    cfn(
        router,
        "CreateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", row.template("1").as_str()),
        ],
    )
    .await;
    let (status, body) = wait_for_status(router, row.case, row.stack, "CREATE_COMPLETE", &[]).await;
    assert_eq!(
        status, "CREATE_COMPLETE",
        "case {:?}: expected the stack to complete",
        row.case
    );
    let group = outputs_of(&body)
        .get("AsgName")
        .unwrap_or_else(|| panic!("case {:?}: the stack publishes its group's name", row.case))
        .clone();
    let serving = wait_for_capacity(router, row.case, &group, row.desired).await;
    (group, serving)
}

/// Start the update that changes the group's launch configuration.
async fn update(router: &MultiServiceRouter, row: &Row) {
    cfn(
        router,
        "UpdateStack",
        &[
            ("StackName", row.stack),
            ("TemplateBody", row.template("2").as_str()),
        ],
    )
    .await;
}

/// Send one signal to the group, exactly as an instance's own bootstrap sends it: keyed by the
/// instance's id ("when you signal Amazon EC2 instances or Auto Scaling groups, specify the
/// instance ID that you are signaling as the unique ID").
async fn signal(router: &MultiServiceRouter, stack: &str, instance_id: &str, status: &str) {
    cfn(
        router,
        "SignalResource",
        &[
            ("StackName", stack),
            ("LogicalResourceId", GROUP),
            ("UniqueId", instance_id),
            ("Status", status),
        ],
    )
    .await;
}

/// What one batch of a signalled row left the group in, polled for: the instances of the
/// original set that are still there, the replacements in service, and the lowest in-service
/// count any poll of this wait observed.
struct Boundary {
    originals_left: BTreeSet<String>,
    replacements: BTreeSet<String>,
    lowest_in_service: usize,
}

/// Poll the group until the batch in flight has replaced `replaced` of the original instances
/// and the group is back to the capacity it holds while the replacement runs — which, for a row
/// that waits on signals, is where the batch STAYS until its replacements have signalled.
async fn wait_for_batch(
    router: &MultiServiceRouter,
    row: &Row,
    group: &str,
    originals: &BTreeSet<String>,
    replaced: usize,
) -> Boundary {
    let deadline = Instant::now() + DEADLINE;
    let mut lowest_in_service = row.desired;
    loop {
        let serving = in_service(router, group).await;
        lowest_in_service = lowest_in_service.min(serving.len());
        let originals_left: BTreeSet<String> = serving.intersection(originals).cloned().collect();
        let replacements: BTreeSet<String> = serving.difference(originals).cloned().collect();
        // The floor the policy states holds through the WHOLE replacement, and so does the
        // ceiling: the group is never taken below `MinInstancesInService`, and it holds no more
        // than the capacity this replacement works at — its declared capacity, or the one it
        // had to launch into because the floor left it nothing to spare, which is bounded by
        // the group's own `MaxSize`.
        assert!(
            serving.len() >= row.min_instances_in_service,
            "case {:?}: the replacement must keep at least {} instance(s) in service, got \
             {serving:?}",
            row.case,
            row.min_instances_in_service
        );
        assert!(
            serving.len() <= row.working && row.working <= row.max_size,
            "case {:?}: the replacement must not take the group past the {} instance(s) it \
             works at (its MaxSize is {}), got {serving:?}",
            row.case,
            row.working,
            row.max_size
        );
        // ...and one batch replaces at most `MaxBatchSize` instances, so no more of the
        // original set can be gone than the batches so far were allowed to take.
        let gone = originals.len() - originals_left.len();
        assert!(
            gone <= replaced,
            "case {:?}: at most {replaced} of the original instances {originals:?} may be gone \
             by this batch, but {gone} are: the group is serving {serving:?}",
            row.case
        );
        if gone == replaced && serving.len() == row.working {
            return Boundary {
                originals_left,
                replacements,
                lowest_in_service,
            };
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: the update never reached the boundary of the batch that replaces \
             instance {replaced} of {originals:?}: the group is serving {serving:?} and the \
             stack is {:?}",
            row.case,
            stack_status(router, row.stack).await
        );
        tokio::time::sleep(POLL).await;
    }
}

#[tokio::test]
async fn a_rolling_update_replaces_a_groups_instances_one_signal_gated_batch_at_a_time() {
    let router = router();
    for row in SIGNALLED {
        let case = row.case;
        let (group, originals) = deployed(&router, row).await;
        assert_eq!(
            originals.len(),
            row.desired,
            "case {case:?}: expected the group to serve its desired capacity"
        );
        update(&router, row).await;

        let mut signalled: BTreeSet<String> = BTreeSet::new();
        let mut lowest_in_service = row.desired;
        for batch in 1..=row.batches() {
            let replaced = (batch * row.batch).min(row.desired);
            let boundary = wait_for_batch(&router, row, &group, &originals, replaced).await;
            lowest_in_service = lowest_in_service.min(boundary.lowest_in_service);
            // The batch is through only when its replacements have signalled, so the update is
            // still in flight here however long ago it was asked for: nothing but this suite's
            // own signal releases it.
            let status = stack_status(&router, row.stack).await;
            assert_eq!(
                status, "UPDATE_IN_PROGRESS",
                "case {case:?}: batch {batch} of {} has replaced {replaced} instance(s) and is \
                 waiting on their signals, so the update must still be in flight; the group is \
                 serving {:?} of the original {originals:?}",
                row.batches(),
                boundary.originals_left
            );
            assert_eq!(
                boundary.originals_left.len(),
                row.desired - replaced,
                "case {case:?}: batch {batch} of {} must leave exactly {} of the original \
                 instances {originals:?} in service, got {:?} beside the replacements {:?}",
                row.batches(),
                row.desired - replaced,
                boundary.originals_left,
                boundary.replacements
            );
            assert_eq!(
                boundary.replacements.len(),
                row.working - (row.desired - replaced),
                "case {case:?}: batch {batch} must leave the group's remaining capacity in \
                 replacements, got {:?} beside {:?}",
                boundary.replacements,
                boundary.originals_left
            );
            for instance_id in &boundary.replacements {
                if signalled.insert(instance_id.clone()) {
                    signal(&router, row.stack, instance_id, "SUCCESS").await;
                }
            }
        }
        let (status, _) = wait_for_status(
            &router,
            case,
            row.stack,
            "UPDATE_COMPLETE",
            &["CREATE_COMPLETE"],
        )
        .await;
        assert_eq!(
            status, "UPDATE_COMPLETE",
            "case {case:?}: every batch has signalled, so the update must complete, with the \
             failures {:?}",
            failed_events(&router, row.stack).await
        );
        let serving = in_service(&router, &group).await;
        assert!(
            serving.is_disjoint(&originals),
            "case {case:?}: the replacement must leave none of the original instances \
             {originals:?} in the group, which is serving {serving:?}"
        );
        assert_eq!(
            serving.len(),
            row.desired,
            "case {case:?}: the group must be back to its desired capacity, got {serving:?}"
        );
        // The dip the batches took is what `MinInstancesInService` is a floor UNDER: the
        // observations above never saw it breached, and the replacement really did take
        // instances out of service rather than swapping them in place.
        assert!(
            lowest_in_service >= row.min_instances_in_service,
            "case {case:?}: the lowest in-service count observed ({lowest_in_service}) must be \
             at least the declared floor of {}",
            row.min_instances_in_service
        );
    }
}

#[tokio::test]
async fn a_rolling_update_that_waits_on_no_signals_replaces_every_instance_itself() {
    let router = router();
    let row = &UNSIGNALLED;
    let case = row.case;
    let (group, originals) = deployed(&router, row).await;
    update(&router, row).await;
    let (status, _) = wait_for_status(
        &router,
        case,
        row.stack,
        "UPDATE_COMPLETE",
        &["CREATE_COMPLETE"],
    )
    .await;
    assert_eq!(
        status, "UPDATE_COMPLETE",
        "case {case:?}: a replacement that waits on no signal must complete on its own, with \
         the failures {:?}",
        failed_events(&router, row.stack).await
    );
    let serving = in_service(&router, &group).await;
    assert!(
        serving.is_disjoint(&originals),
        "case {case:?}: every original instance of {originals:?} must have been replaced, but \
         the group is serving {serving:?}"
    );
    assert_eq!(
        serving.len(),
        row.desired,
        "case {case:?}: the group must be back to its desired capacity, got {serving:?}"
    );
}

/// The row the FAILURE leg is driven on: one instance replaced at a time, gated on signals, so
/// the replacement whose bootstrap reports a failure is the first batch's.
const FAILING: Row = Row {
    case: "a replacement that reports its own bootstrap failed",
    stack: "rolling-update-failed-signal",
    desired: 2,
    max_size: 3,
    max_batch_size: 1,
    min_instances_in_service: 1,
    wait_on_signals: true,
    batch: 1,
    working: 2,
};

#[tokio::test]
async fn a_replacement_that_signals_failure_cancels_the_update_and_rolls_the_stack_back() {
    let router = router();
    let row = &FAILING;
    let case = row.case;
    let (group, originals) = deployed(&router, row).await;
    update(&router, row).await;
    let boundary = wait_for_batch(&router, row, &group, &originals, 1).await;
    let replacement = boundary
        .replacements
        .iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the first batch launched a replacement"))
        .clone();
    signal(&router, row.stack, &replacement, "FAILURE").await;

    let (status, _) = wait_for_status(
        &router,
        case,
        row.stack,
        "UPDATE_ROLLBACK_COMPLETE",
        &["CREATE_COMPLETE", "UPDATE_COMPLETE"],
    )
    .await;
    assert_eq!(
        status, "UPDATE_ROLLBACK_COMPLETE",
        "case {case:?}: a replacement that signals FAILURE must cancel the update and roll the \
         stack back, with the failures {:?}",
        failed_events(&router, row.stack).await
    );
    // The group the stack started with is still there, serving what it was asked to serve:
    // nothing past the batch that failed was replaced.
    let serving = wait_for_capacity(&router, case, &group, row.desired).await;
    assert!(
        serving.iter().any(|instance_id| originals.contains(instance_id)),
        "case {case:?}: the instances the update had not reached must still be serving in \
         {group}, which is serving {serving:?} of the original {originals:?}"
    );
    // ...and the failure is reported on the GROUP, naming the signal that cancelled the
    // update, rather than only on the stack.
    let reported = failed_events(&router, row.stack).await;
    assert!(
        reported
            .iter()
            .any(|event| event.starts_with(&format!("{GROUP}:")) && event.contains("FAILURE")),
        "case {case:?}: the group's own event must report the FAILURE signal that cancelled the \
         update, got {reported:?}"
    );
}
