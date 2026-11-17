// SPDX-License-Identifier: CC0-1.0

//! The LAUNCH-TEMPLATE PROVENANCE of an instance an Auto Scaling group launched for a
//! CloudFormation stack: the reserved tags that join a running instance back to the launch
//! template — and the template VERSION — the stack declared.
//!
//! The RULE under test: an instance an Auto Scaling group launches from a launch template
//! carries, beside the reserved tag naming the group and the group's tags whose
//! `PropagateAtLaunch` is true ("determines whether the tag is added to new instances as they
//! are launched in the group", `autoscaling#Tag$PropagateAtLaunch`), the two reserved tags
//! Amazon EC2 stamps for a template launch: `aws:ec2launchtemplate:id`, whose value is "the ID
//! of the launch template" the group resolved
//! (`autoscaling#LaunchTemplateSpecification$LaunchTemplateId`), and
//! `aws:ec2launchtemplate:version`, whose value is the version number the launch RESOLVED as a
//! decimal string — never the `$Latest` or `$Default` word a group may float on ("the launch
//! template version number, $Latest, or $Default",
//! `autoscaling#LaunchTemplateSpecification$Version`). The tags are a fact of the instance at
//! the instant it was launched, so an instance keeps the version it came from after the
//! template has moved on.
//!
//! This suite drives that rule the way a CDK app does — through a CloudFormation stack
//! declaring an `AWS::EC2::LaunchTemplate` and `AWS::AutoScaling::AutoScalingGroup` resources
//! — so it needs CloudFormation, Amazon EC2 Auto Scaling and Amazon EC2 to agree over one
//! engine, which is why it lives here. The sibling suite
//! `asg_launch_template_instance_tags.rs` drives the same rule through the two services' own
//! APIs; what is new here is the stack: the id the instance's tag must carry is the one the
//! stack published for the template it created, which is how a rollout asks "did this instance
//! come from the template version my stack declares?".
//!
//! The rows span what the rule generalises over — HOW the group's declaration names the
//! template and its version:
//!
//! * by id, at the number the stack's `Fn::GetAtt … LatestVersionNumber` resolved;
//! * by name, floating on `$Latest`;
//! * by id, with no `Version` at all, which is `$Default`;
//!
//! and then, after the stack UPDATE that gives the floating template a second version, a
//! fresh instance of the floating group, which must name version 2 while the instance
//! launched before the update still names version 1.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{
    block_of, blocks_of, element, elements, form_encode, items_of, members_of, outputs_of,
};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The API versions the three services' query requests carry.
const CFN_VERSION: &str = "2010-05-15";
const ASG_VERSION: &str = "2011-01-01";
const EC2_API_VERSION: &str = "2016-11-15";

/// The stack under test.
const STACK: &str = "cdk-l2-asg-provenance";

/// The names the stack gives the two launch templates it declares. The group that names its
/// template BY NAME names one of them, so the name has to be the declaration's own.
const PINNED_TEMPLATE_NAME: &str = "cdk-l2-asg-provenance-pinned";
const FLOATING_TEMPLATE_NAME: &str = "cdk-l2-asg-provenance-floating";

/// The AMIs and the instance type the declared launch data names. The floating template's
/// image is what the stack UPDATE changes, which is what gives that template a second version.
const IMAGE_ONE: &str = "ami-0abcdef1234567890";
const IMAGE_TWO: &str = "ami-0abcdef1234567891";
const INSTANCE_TYPE: &str = "t3.micro";

/// The group tag every declared group propagates to the instances it launches, so the reserved
/// tags are asserted BESIDE a tag the declaration asked for rather than on their own.
const PROPAGATED_KEY: &str = "Owner";
const PROPAGATED_VALUE: &str = "platform";

/// The reserved tag keys the rule is about, spelled out here rather than imported so a wrong
/// constant in the implementation cannot make this suite pass.
const GROUP_NAME_TAG: &str = "aws:autoscaling:groupName";
const TEMPLATE_ID_TAG: &str = "aws:ec2launchtemplate:id";
const TEMPLATE_VERSION_TAG: &str = "aws:ec2launchtemplate:version";

/// The lifecycle state a group reports an instance of its own capacity in once it is serving
/// ("`InService` — The instance is running and in service", `Instance$LifecycleState`), which
/// is the state a client waits for before reading the instance.
const IN_SERVICE: &str = "InService";

/// How long a wait gives a state the clock owes (the stack's statuses, a launch reaching
/// `InService`) and how often it looks. Generous — the whole suite settles in seconds — and
/// only here so a failure is a failure rather than a hang: every wait below waits for its
/// CONDITION, never for a time.
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

/// One Amazon EC2 Auto Scaling operation that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "autoscaling", ASG_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Amazon EC2 operation that must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, body) = query(router, "ec2", EC2_API_VERSION, action, members).await;
    assert_eq!(
        status, 200,
        "Amazon EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// The template the stack declares: two launch templates and three groups launching from them,
/// in the shape a CDK L2 Auto Scaling group renders to (sizes as strings, the version read off
/// the template's own `LatestVersionNumber` attribute, the subnets as a list of handles).
///
/// `floating_image` is the image the FLOATING template's launch data names; changing it is the
/// declaration change that gives that template a new version.
fn template(floating_image: &str) -> String {
    format!(
        r#"{{"Resources":{{
  "Vpc":{{"Type":"AWS::EC2::VPC","Properties":{{"CidrBlock":"10.0.0.0/16"}}}},
  "Subnet":{{"Type":"AWS::EC2::Subnet","Properties":{{
    "VpcId":{{"Ref":"Vpc"}},"CidrBlock":"10.0.0.0/24","AvailabilityZone":"{REGION}a",
    "MapPublicIpOnLaunch":true}}}},
  "Sg":{{"Type":"AWS::EC2::SecurityGroup","Properties":{{
    "GroupDescription":"the instances of the declared groups","VpcId":{{"Ref":"Vpc"}}}}}},
  "PinnedLt":{{"Type":"AWS::EC2::LaunchTemplate","Properties":{{
    "LaunchTemplateName":"{PINNED_TEMPLATE_NAME}",
    "LaunchTemplateData":{{"ImageId":"{IMAGE_ONE}","InstanceType":"{INSTANCE_TYPE}",
      "SecurityGroupIds":[{{"Fn::GetAtt":["Sg","GroupId"]}}]}}}}}},
  "FloatingLt":{{"Type":"AWS::EC2::LaunchTemplate","Properties":{{
    "LaunchTemplateName":"{FLOATING_TEMPLATE_NAME}",
    "LaunchTemplateData":{{"ImageId":"{floating_image}","InstanceType":"{INSTANCE_TYPE}",
      "SecurityGroupIds":[{{"Fn::GetAtt":["Sg","GroupId"]}}]}}}}}},
  "PinnedAsg":{{"Type":"AWS::AutoScaling::AutoScalingGroup","Properties":{{
    "MinSize":"1","MaxSize":"1","DesiredCapacity":"1",
    "LaunchTemplate":{{"LaunchTemplateId":{{"Ref":"PinnedLt"}},
      "Version":{{"Fn::GetAtt":["PinnedLt","LatestVersionNumber"]}}}},
    "VPCZoneIdentifier":[{{"Ref":"Subnet"}}],
    "Tags":[{{"Key":"{PROPAGATED_KEY}","Value":"{PROPAGATED_VALUE}",
      "PropagateAtLaunch":true}}]}}}},
  "FloatingAsg":{{"Type":"AWS::AutoScaling::AutoScalingGroup","DependsOn":"FloatingLt",
    "Properties":{{
    "MinSize":"1","MaxSize":"2","DesiredCapacity":"1",
    "LaunchTemplate":{{"LaunchTemplateName":"{FLOATING_TEMPLATE_NAME}","Version":"$Latest"}},
    "VPCZoneIdentifier":[{{"Ref":"Subnet"}}],
    "Tags":[{{"Key":"{PROPAGATED_KEY}","Value":"{PROPAGATED_VALUE}",
      "PropagateAtLaunch":true}}]}}}},
  "DefaultAsg":{{"Type":"AWS::AutoScaling::AutoScalingGroup","Properties":{{
    "MinSize":"1","MaxSize":"1","DesiredCapacity":"1",
    "LaunchTemplate":{{"LaunchTemplateId":{{"Ref":"PinnedLt"}}}},
    "VPCZoneIdentifier":[{{"Ref":"Subnet"}}],
    "Tags":[{{"Key":"{PROPAGATED_KEY}","Value":"{PROPAGATED_VALUE}",
      "PropagateAtLaunch":true}}]}}}}}},
  "Outputs":{{
    "PinnedLtId":{{"Value":{{"Ref":"PinnedLt"}}}},
    "FloatingLtId":{{"Value":{{"Ref":"FloatingLt"}}}},
    "PinnedAsgName":{{"Value":{{"Ref":"PinnedAsg"}}}},
    "FloatingAsgName":{{"Value":{{"Ref":"FloatingAsg"}}}},
    "DefaultAsgName":{{"Value":{{"Ref":"DefaultAsg"}}}}}}}}"#
    )
}

/// Whether a stack status is one the stack settles at, rather than one it passes through.
fn settled(status: &str) -> bool {
    status.ends_with("_COMPLETE") || status.ends_with("_FAILED")
}

/// Poll `DescribeStacks` until the stack reports `wanted`, answering `(status, body)` — or the
/// OTHER settled status it reached instead, so a deployment that did not get there fails with
/// what the stack has to say rather than on the deadline.
///
/// `through` names the settled statuses this wait polls THROUGH: a deployment that follows an
/// earlier one starts with the earlier one's terminal status still reported, which is not this
/// deployment's outcome.
async fn wait_for_status(
    router: &MultiServiceRouter,
    wanted: &str,
    through: &[&str],
) -> (String, String) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = cfn(router, "DescribeStacks", &[("StackName", STACK)]).await;
        let status = element(&body, "StackStatus");
        if status == wanted || (settled(&status) && !through.contains(&status.as_str())) {
            return (status, body);
        }
        assert!(
            Instant::now() < deadline,
            "the stack never reached {wanted}: it is {status:?} in {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The stack's FAILED resource events, each as `logical id: status: reason` — what a
/// deployment that did not complete has to say for itself.
async fn failed_events(router: &MultiServiceRouter) -> Vec<String> {
    let body = cfn(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
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

/// The ids of the instances the group `group` reports IN SERVICE, polled until it reports at
/// least `wanted` of them.
///
/// Each entry is read inside the `<member>` that carries BOTH its id and its lifecycle state,
/// scoped to the group's own `Instances` list, so an id is never paired with another entry's
/// state.
async fn in_service_instances(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = asg(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", group)],
        )
        .await;
        let mut serving = Vec::new();
        for entry in block_of(&body, "Instances")
            .map(members_of)
            .unwrap_or_default()
        {
            let state = elements(entry, "LifecycleState").into_iter().next();
            let instance_id = elements(entry, "InstanceId").into_iter().next();
            if let (Some(state), Some(instance_id)) = (state, instance_id) {
                if state == IN_SERVICE {
                    serving.push(instance_id);
                }
            }
        }
        if serving.len() >= wanted {
            return serving;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {group} to report {wanted} {IN_SERVICE} instance(s), got \
             {serving:?} in {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The tags Amazon EC2 holds for ONE instance, as the map a client reads them as.
///
/// Read through `DescribeInstances` naming exactly that instance, which is where a client reads
/// an instance's tags: the read is scoped to the answer's TAG SETS, so no other keyed list of
/// the document is folded in, and each pair is read inside the `<item>` that carries both
/// halves, so a tag never takes another's value.
async fn instance_tags(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> BTreeMap<String, String> {
    let body = ec2(router, "DescribeInstances", &[("InstanceId.1", instance_id)]).await;
    assert!(
        body.contains(instance_id),
        "case {case:?}: expected Amazon EC2 to report {instance_id} in {REGION}, got {body}"
    );
    blocks_of(&body, "tagSet")
        .into_iter()
        .flat_map(items_of)
        .filter_map(|item| {
            let key = elements(item, "key").into_iter().next()?;
            let value = elements(item, "value").into_iter().next()?;
            Some((key, value))
        })
        .collect()
}

/// Assert that the instance `instance_id` carries the launch-template provenance of
/// `launch_template_id` at version `version`, beside the group's own two tags.
async fn assert_provenance(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
    group: &str,
    launch_template_id: &str,
    version: &str,
) {
    let read = instance_tags(router, case, instance_id).await;
    for (key, expected) in [
        (TEMPLATE_ID_TAG, launch_template_id),
        (TEMPLATE_VERSION_TAG, version),
        (GROUP_NAME_TAG, group),
        (PROPAGATED_KEY, PROPAGATED_VALUE),
    ] {
        let held = read.get(key).map(String::as_str);
        assert_eq!(
            held,
            Some(expected),
            "case {case:?}: expected the instance {instance_id} of {group} to carry \
             {key}={expected:?}, got {held:?} among {read:?}"
        );
    }
}

/// One row: the group the stack declared, the output naming the launch template it launches
/// from, and the version number its instance must report having been launched from.
struct Row {
    case: &'static str,
    group_output: &'static str,
    template_output: &'static str,
    version: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the group naming its template by id at the version the stack resolved",
        group_output: "PinnedAsgName",
        template_output: "PinnedLtId",
        version: "1",
    },
    Row {
        case: "the group naming its template by name and floating on $Latest",
        group_output: "FloatingAsgName",
        template_output: "FloatingLtId",
        version: "1",
    },
    Row {
        case: "the group naming its template by id with no version, which is $Default",
        group_output: "DefaultAsgName",
        template_output: "PinnedLtId",
        version: "1",
    },
];

#[tokio::test]
async fn an_instance_a_declared_group_launched_names_the_stacks_launch_template_and_version() {
    let router = router();

    cfn(
        &router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", template(IMAGE_ONE).as_str()),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    let (status, settled_body) = wait_for_status(&router, "CREATE_COMPLETE", &[]).await;
    // The failure events are read for the message's sake, and only where there is a failure to
    // explain: a create that did not complete says which resource refused and why.
    let failures = if status == "CREATE_COMPLETE" {
        Vec::new()
    } else {
        failed_events(&router).await
    };
    assert_eq!(
        status, "CREATE_COMPLETE",
        "expected the stack to complete, got {status:?} with the failures {failures:?}"
    );
    let outputs: BTreeMap<String, String> = outputs_of(&settled_body);

    // LEG 1: every group of the stack launched its instance from the template the stack
    // created, and the instance says which template and which version that was.
    let mut floating_group = String::new();
    let mut floating_before: Vec<String> = Vec::new();
    for row in ROWS {
        let case = row.case;
        let group = outputs
            .get(row.group_output)
            .unwrap_or_else(|| panic!("case {case:?}: the stack publishes {}", row.group_output));
        let launch_template_id = outputs.get(row.template_output).unwrap_or_else(|| {
            panic!("case {case:?}: the stack publishes {}", row.template_output)
        });
        let serving = in_service_instances(&router, case, group, 1).await;
        for instance_id in &serving {
            assert_provenance(
                &router,
                case,
                instance_id,
                group,
                launch_template_id,
                row.version,
            )
            .await;
        }
        if row.group_output == "FloatingAsgName" {
            floating_group = group.clone();
            floating_before = serving;
        }
    }

    // LEG 2: the stack UPDATE gives the floating template a second version, and the next
    // instance that group launches names THAT version — while the one launched before the
    // update still names the version it came from.
    cfn(
        &router,
        "UpdateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", template(IMAGE_TWO).as_str()),
            ("Capabilities.member.1", "CAPABILITY_IAM"),
        ],
    )
    .await;
    let (status, _) = wait_for_status(&router, "UPDATE_COMPLETE", &["CREATE_COMPLETE"]).await;
    let failures = if status == "UPDATE_COMPLETE" {
        Vec::new()
    } else {
        failed_events(&router).await
    };
    assert_eq!(
        status, "UPDATE_COMPLETE",
        "expected the update to complete, got {status:?} with the failures {failures:?}"
    );
    let floating_template = outputs
        .get("FloatingLtId")
        .expect("the stack publishes its floating template's id")
        .clone();
    // The version EC2 now holds as that template's latest, read from the service that owns it
    // rather than counted here.
    let described = ec2(
        &router,
        "DescribeLaunchTemplates",
        &[("LaunchTemplateId.1", floating_template.as_str())],
    )
    .await;
    let latest = element(&described, "latestVersionNumber");
    assert_eq!(
        latest, "2",
        "the update must give the floating template a second version, got {latest:?} in \
         {described}"
    );

    let case = "a group on $Latest after the stack moved its template on";
    asg(
        &router,
        "SetDesiredCapacity",
        &[
            ("AutoScalingGroupName", floating_group.as_str()),
            ("DesiredCapacity", "2"),
        ],
    )
    .await;
    let serving = in_service_instances(&router, case, &floating_group, 2).await;
    let launched_after: Vec<String> = serving
        .iter()
        .filter(|instance_id| !floating_before.contains(instance_id))
        .cloned()
        .collect();
    assert_eq!(
        launched_after.len(),
        1,
        "case {case:?}: expected the scale-out to launch one further instance, got {serving:?} \
         beside {floating_before:?}"
    );
    for instance_id in &launched_after {
        assert_provenance(
            &router,
            case,
            instance_id,
            &floating_group,
            &floating_template,
            &latest,
        )
        .await;
    }
    // The instance launched BEFORE the update still names the version it was launched from:
    // the tags record the launch, not the template's current state.
    for instance_id in &floating_before {
        assert_provenance(
            &router,
            "an instance launched before the template moved on",
            instance_id,
            &floating_group,
            &floating_template,
            "1",
        )
        .await;
    }
}
