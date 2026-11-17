// SPDX-License-Identifier: CC0-1.0

//! A CloudFormation stack that declares an Auto Scaling group the way a CDK app renders one
//! — a VPC with two subnets, a security group, an `AWS::EC2::LaunchTemplate` and an
//! `AWS::AutoScaling::AutoScalingGroup` placed across those subnets — settles, and its drift
//! detection answers for what the services that own those resources report NOW.
//!
//! The RULE under test: `DetectStackDrift` "compares the actual configuration of the
//! resource with its expected template configuration", and "only resource properties
//! explicitly defined in the stack template are checked for drift" — so a stack NOTHING has
//! been changed in since it was created is `IN_SYNC`, whatever shape the owning service's own
//! API happens to report a declared property in, and a stack one of whose declared properties
//! was changed OUT OF BAND is `DRIFTED` with that one resource `MODIFIED` and one
//! `PropertyDifference` at the path of the property that was changed. Both halves need
//! CloudFormation, Amazon EC2 and Amazon EC2 Auto Scaling to agree over one engine, which is
//! why the suite lives here.
//!
//! The two drift legs are the two sides of that one rule: the fresh stack (no difference
//! anywhere, for a declaration that spans a property the group's own API takes as a delimited
//! string, a list of resource handles, a nested structure and numbers written as strings) and
//! the same stack after `UpdateAutoScalingGroup` changed one declared property out of band
//! (exactly one difference, at that property's path, with both values as the two sides spell
//! them).
//!
//! The third leg is the same agreement read at the end of the stack's life: "CloudFormation
//! deletes the resources in reverse order", so a deletion settles at `DELETE_COMPLETE` only if
//! the group's instances are terminated before the subnets they sat in and the security group
//! they carried are taken away — and each of those resources is then gone from the service
//! that owns it.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{
    block_of, element, elements, form_encode, members_of, outputs_of, physical_ids_of,
};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The API versions the three services' query requests carry.
const CFN_VERSION: &str = "2010-05-15";
const ASG_VERSION: &str = "2011-01-01";
const EC2_API_VERSION: &str = "2016-11-15";

/// The stack under test, and the logical id of the group it declares.
const STACK: &str = "cdk-shaped-asg-stack";
const GROUP_LOGICAL_ID: &str = "Asg";

/// The sizes the template declares for the group, as the template spells them — a CDK app
/// renders an Auto Scaling group's sizes as strings.
const DECLARED_MIN: &str = "1";
const DECLARED_MAX: &str = "2";

/// The size the out-of-band change puts the group on.
const CHANGED_MAX: &str = "3";

/// How long a leg waits for a state the clock owes (the stack's own status moves, the
/// detection's completion) and how often it looks. Generous, and only here so a failure is a
/// failure rather than a hang: every wait below waits for its CONDITION, never for a time.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(50);

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

/// One Auto Scaling operation that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "autoscaling", ASG_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// The template the stack declares: the shape a CDK L2 Auto Scaling group renders to.
fn template() -> String {
    // `AvailabilityZone` values are the Region's own, so the subnets are placed where the
    // group's instances can run.
    format!(
        r#"{{"Resources":{{
  "Vpc":{{"Type":"AWS::EC2::VPC","Properties":{{"CidrBlock":"10.0.0.0/16"}}}},
  "SubnetA":{{"Type":"AWS::EC2::Subnet","Properties":{{
    "VpcId":{{"Ref":"Vpc"}},"CidrBlock":"10.0.0.0/24","AvailabilityZone":"{REGION}a",
    "MapPublicIpOnLaunch":true}}}},
  "SubnetB":{{"Type":"AWS::EC2::Subnet","Properties":{{
    "VpcId":{{"Ref":"Vpc"}},"CidrBlock":"10.0.1.0/24","AvailabilityZone":"{REGION}b",
    "MapPublicIpOnLaunch":true}}}},
  "Sg":{{"Type":"AWS::EC2::SecurityGroup","Properties":{{
    "GroupDescription":"instances of the group","VpcId":{{"Ref":"Vpc"}}}}}},
  "Lt":{{"Type":"AWS::EC2::LaunchTemplate","Properties":{{"LaunchTemplateData":{{
    "ImageId":"ami-0abcdef1234567890","InstanceType":"t3.micro",
    "SecurityGroupIds":[{{"Fn::GetAtt":["Sg","GroupId"]}}],
    "TagSpecifications":[{{"ResourceType":"instance",
      "Tags":[{{"Key":"Name","Value":"from-the-template"}}]}}]}}}}}},
  "{GROUP_LOGICAL_ID}":{{"Type":"AWS::AutoScaling::AutoScalingGroup","Properties":{{
    "MinSize":"{DECLARED_MIN}","MaxSize":"{DECLARED_MAX}",
    "LaunchTemplate":{{"LaunchTemplateId":{{"Ref":"Lt"}},
      "Version":{{"Fn::GetAtt":["Lt","LatestVersionNumber"]}}}},
    "VPCZoneIdentifier":[{{"Ref":"SubnetA"}},{{"Ref":"SubnetB"}}],
    "Tags":[{{"Key":"Name","Value":"cdk-shaped","PropagateAtLaunch":true}}]}}}}}},
  "Outputs":{{
    "AsgName":{{"Value":{{"Ref":"{GROUP_LOGICAL_ID}"}}}},
    "VpcId":{{"Value":{{"Ref":"Vpc"}}}},
    "LaunchTemplateId":{{"Value":{{"Ref":"Lt"}}}}}}}}"#
    )
}

/// Whether a stack status is one the stack settles at, rather than one it passes through.
fn settled(status: &str) -> bool {
    status.ends_with("_COMPLETE") || status.ends_with("_FAILED")
}

/// Poll `DescribeStacks` until the stack settles, and answer the answer it settled in.
///
/// `stack` is the name or the id: a DELETED stack is addressed by its id, which is how a
/// client reads the outcome of a deletion.
async fn settle(router: &MultiServiceRouter, stack: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cfn(router, "DescribeStacks", &[("StackName", stack)]).await;
        let status = element(&body, "StackStatus");
        if settled(&status) {
            return body;
        }
        assert!(
            Instant::now() < deadline,
            "the stack never left {status:?}: last answer {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// Detect the stack's drift and poll until the detection is no longer in progress, answering
/// `(DetectionStatus, StackDriftStatus)` as the detection reports them.
async fn detect_drift(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let body = cfn(router, "DetectStackDrift", &[("StackName", STACK)]).await;
    let detection_id = element(&body, "StackDriftDetectionId");
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cfn(
            router,
            "DescribeStackDriftDetectionStatus",
            &[("StackDriftDetectionId", detection_id.as_str())],
        )
        .await;
        let detection_status = element(&body, "DetectionStatus");
        if detection_status != "DETECTION_IN_PROGRESS" {
            let drift_status = element(&body, "StackDriftStatus");
            return (detection_status, drift_status);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the detection never left {detection_status:?}: last answer {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The stack's FAILED resource events, each as `logical id: status: reason` — what an
/// operation that did not complete has to say for itself.
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

/// One resource's drift as `DescribeStackResourceDrifts` reports it: its logical id, its
/// status, and each difference as `path|expected|actual|type`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReportedDrift {
    logical_id: String,
    status: String,
    differences: Vec<String>,
}

/// Every drift record the stack holds for the statuses `filters` names (every record when it
/// names none), read through the operation whose subject they are.
async fn resource_drifts(
    router: &MultiServiceRouter,
    filters: &[&str],
) -> Vec<ReportedDrift> {
    let mut members: Vec<(String, String)> = vec![("StackName".to_string(), STACK.to_string())];
    for (position, filter) in filters.iter().enumerate() {
        members.push((
            format!("StackResourceDriftStatusFilters.member.{}", position + 1),
            (*filter).to_string(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = members
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    let body = cfn(router, "DescribeStackResourceDrifts", &borrowed).await;
    // One record per `<StackId>`, which is each `StackResourceDrift`'s first member and
    // appears once in each: the records carry nested `<member>` lists of their own (their
    // property differences), so splitting on `<member` would cut a record in half and read
    // the wrong half. Each record's own members are then read inside it rather than by
    // zipping document-ordered lists of the whole answer.
    let drifts = block_of(&body, "StackResourceDrifts").unwrap_or_default();
    drifts
        .split("<StackId>")
        .skip(1)
        .filter_map(|record| {
            let logical_id = elements(record, "LogicalResourceId").into_iter().next()?;
            let status = elements(record, "StackResourceDriftStatus")
                .into_iter()
                .next()?;
            let paths = elements(record, "PropertyPath");
            let expected = elements(record, "ExpectedValue");
            let actual = elements(record, "ActualValue");
            let kinds = elements(record, "DifferenceType");
            let differences = paths
                .into_iter()
                .zip(expected)
                .zip(actual)
                .zip(kinds)
                .map(|(((path, expected), actual), kind)| {
                    format!("{path}|{expected}|{actual}|{kind}")
                })
                .collect();
            Some(ReportedDrift {
                logical_id,
                status,
                differences,
            })
        })
        .collect()
}

#[tokio::test]
async fn a_cdk_shaped_asg_stack_settles_and_reports_only_the_drift_it_carries() {
    let router = router();

    // The stack settles, and publishes the handles its outputs name.
    cfn(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", template().as_str()),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    let settled_body = settle(&router, STACK).await;
    let status = element(&settled_body, "StackStatus");
    // The failure events are read for the message's sake: a create that did not complete says
    // which resource refused and why, which is what a failure has to name.
    let failures = failed_events(&router, STACK).await;
    assert_eq!(
        status, "CREATE_COMPLETE",
        "case \"the fresh stack\": expected the stack to complete, got {status:?} with the \
         failures {failures:?}"
    );
    let stack_id = element(&settled_body, "StackId");
    let outputs: BTreeMap<String, String> = outputs_of(&settled_body);
    for key in ["AsgName", "VpcId", "LaunchTemplateId"] {
        assert!(
            outputs.get(key).is_some_and(|value| !value.is_empty()),
            "case \"the fresh stack\": expected the output {key} to name a resource, got \
             {outputs:?}"
        );
    }
    let group = outputs
        .get("AsgName")
        .expect("the stack publishes its group's name")
        .clone();

    // LEG 1: nothing has been changed, so nothing differs — whatever shape each owning
    // service reports a declared property in.
    let (detection_status, drift_status) = detect_drift(&router, "the fresh stack").await;
    assert_eq!(
        detection_status, "DETECTION_COMPLETE",
        "case \"the fresh stack\": expected the detection to complete, got \
         {detection_status:?}"
    );
    let reported = resource_drifts(&router, &[]).await;
    assert_eq!(
        drift_status, "IN_SYNC",
        "case \"the fresh stack\": expected a stack nothing has changed to be IN_SYNC, got \
         {drift_status:?} with the per-resource records {reported:?}"
    );

    // LEG 2: one declared property changed out of band — through the group's own API, the
    // way a client changes a group — is the one difference the stack reports.
    asg(
        &router,
        "UpdateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", group.as_str()),
            ("MaxSize", CHANGED_MAX),
        ],
    )
    .await;
    let (detection_status, drift_status) = detect_drift(&router, "after the out-of-band change").await;
    assert_eq!(
        detection_status, "DETECTION_COMPLETE",
        "case \"after the out-of-band change\": expected the detection to complete, got \
         {detection_status:?}"
    );
    assert_eq!(
        drift_status, "DRIFTED",
        "case \"after the out-of-band change\": expected the stack to carry the group's \
         drift, got {drift_status:?}"
    );
    let modified = resource_drifts(&router, &["MODIFIED"]).await;
    let expected = vec![ReportedDrift {
        logical_id: GROUP_LOGICAL_ID.to_string(),
        status: "MODIFIED".to_string(),
        differences: vec![format!(
            "/MaxSize|{DECLARED_MAX}|{CHANGED_MAX}|NOT_EQUAL"
        )],
    }];
    assert_eq!(
        modified, expected,
        "case \"after the out-of-band change\": expected exactly {expected:?}, got {modified:?}"
    );

    // LEG 3: the stack's deletion takes every resource it created down — and in the order
    // their dependencies allow: the group's instances are terminated first, so the subnets
    // they sat in and the security group they carried can be deleted at all.
    let created = physical_ids_of(&cfn(&router, "DescribeStackResources", &[("StackName", STACK)]).await);
    cfn(&router, "DeleteStack", &[("StackName", STACK)]).await;
    let settled_body = settle(&router, &stack_id).await;
    let status = element(&settled_body, "StackStatus");
    let failures = failed_events(&router, &stack_id).await;
    assert_eq!(
        status, "DELETE_COMPLETE",
        "case \"the deleted stack\": expected the deletion to complete, got {status:?} with \
         the failures {failures:?}"
    );

    // The group is gone from Amazon EC2 Auto Scaling, and the network it launched into is
    // gone from Amazon EC2 — read where each resource lives, not off the stack's own record.
    let body = asg(
        &router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", group.as_str())],
    )
    .await;
    assert!(
        elements(&body, "AutoScalingGroupName").is_empty(),
        "case \"the deleted stack\": expected Auto Scaling to hold no group {group:?} any \
         more, got {body}"
    );
    // (the read, the parameter the resource's id travels under, the logical ids to check)
    let gone: [(&str, &str, &[&str]); 2] = [
        ("DescribeSubnets", "SubnetId.1", &["SubnetA", "SubnetB"]),
        ("DescribeSecurityGroups", "GroupId.1", &["Sg"]),
    ];
    for (action, parameter, logical_ids) in gone {
        for logical_id in logical_ids {
            let physical_id = created
                .get(*logical_id)
                .unwrap_or_else(|| panic!("the stack reported a physical id for {logical_id}"));
            let (status, body) = query(
                &router,
                "ec2",
                EC2_API_VERSION,
                action,
                &[(parameter, physical_id.as_str())],
            )
            .await;
            assert_ne!(
                status, 200,
                "case \"the deleted stack\": expected Amazon EC2 to hold no {logical_id} \
                 ({physical_id}) any more, got {status} with {body}"
            );
        }
    }
}
