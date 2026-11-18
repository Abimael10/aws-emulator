// SPDX-License-Identifier: CC0-1.0

//! Creating an Amazon ECS capacity provider over an Auto Scaling group MARKS that group as
//! Amazon ECS's to manage — and the caller needs the Auto Scaling permission that mark takes.
//!
//! TWO RULES, both of which only hold if three crates agree over one engine, which is why
//! this suite lives here: Amazon ECS makes the create, Amazon EC2 Auto Scaling owns the group
//! the provider binds and reports its tags, and IAM holds the documents the decision is taken
//! on.
//!
//! 1. **An operation that makes a resource of another service its own writes that fact where
//!    the resource lives.** A capacity provider over an Auto Scaling group has Amazon ECS
//!    drive that group's desired capacity and its termination protection, and the group
//!    carries the marker tag that says so — the same key a group prepared for a capacity
//!    provider by hand is told to carry ("add the `AmazonECSManaged` tag to the Auto Scaling
//!    group", Amazon ECS capacity providers) — so `DescribeAutoScalingGroups` reports it
//!    beside the tags the caller created the group with, whichever of the two forms
//!    `AutoScalingGroupProvider$autoScalingGroupArn` admits named the group.
//!
//! 2. **An operation that mutates another service's resource is authorized against that
//!    other service's action on it too.** Because the create has to write the group's tags,
//!    a caller whose policies do not permit `autoscaling:CreateOrUpdateTags` on that group is
//!    refused with HTTP 403 `AccessDeniedException`, and nothing is created: no provider, and
//!    no tag on the group.
//!
//! The rows of the second rule span what it generalises over: a principal whose documents
//! explicitly deny the Auto Scaling action, one whose documents never allow it, and one that
//! allows it — each holding the same grant of the Amazon ECS action, so what differs between
//! them is only the permission the tag write needs.

use std::collections::BTreeMap;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for awsQuery suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and each awsQuery request carries.
const EC2_API_VERSION: &str = "2016-11-15";
const ASG_VERSION: &str = "2011-01-01";
const IAM_API_VERSION: &str = "2010-05-08";
const STS_API_VERSION: &str = "2011-06-15";

/// The `X-Amz-Target` prefix Amazon ECS's model fixes (service shape
/// `AmazonEC2ContainerServiceV20141113`); it speaks awsJson1.1.
const ECS_TARGET_PREFIX: &str = "AmazonEC2ContainerServiceV20141113";

/// The access key id of the account's own published credential: a key neither STS nor IAM
/// holds a record for, which is the position AWS account root credentials are in.
const ACCOUNT_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// The AMI and instance type the launch template below names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";

/// The launch template every group of this suite launches from.
const TEMPLATE: &str = "ecscp-template";

/// The marker tag key Amazon ECS writes onto the group, and the error code and sentence a
/// caller that may not write it is refused with — spelled out here rather than imported, so a
/// wrong constant in the implementation cannot make a test pass.
const MANAGED_TAG: &str = "AmazonECSManaged";
const ACCESS_DENIED: &str = "AccessDeniedException";
const DENIED_MESSAGE: &str = "The capacity provider could not be created because you do not \
                              have autoscaling:CreateOrUpdateTags permissions to create tags \
                              on the Auto Scaling group.";

/// The whole emulator over one engine, exactly as the binary assembles it: Amazon ECS, Amazon
/// EC2 Auto Scaling, Amazon EC2, IAM and STS are all reachable through it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's: the credential
    // scope in the `Authorization` header is what names the caller.
    router.set_request_authentication(false);
    router
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(&str, &str)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to `signing_name`, presented as signed by the access key id
/// `akid` — the only place the credential appears on the wire, and so what a service reads the
/// caller's identity from.
async fn send_as(
    router: &MultiServiceRouter,
    signing_name: &str,
    akid: &str,
    body: String,
) -> (u16, String) {
    let host = if signing_name == "iam" {
        "iam.amazonaws.com".to_string()
    } else {
        format!("{signing_name}.{REGION}.amazonaws.com")
    };
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={akid}/20260101/{REGION}/{signing_name}/\
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

/// One ec2Query action, by the account's own credential, that must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send_as(
        router,
        "ec2",
        ACCOUNT_KEY_ID,
        form(action, EC2_API_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Auto Scaling action, by the account's own credential, that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send_as(
        router,
        "autoscaling",
        ACCOUNT_KEY_ID,
        form(action, ASG_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One IAM action, by the account's own credential — the identity that provisions the role a
/// row signs with.
async fn iam(router: &MultiServiceRouter, action: &str, params: &[(&str, &str)]) -> String {
    let (status, body) = send_as(
        router,
        "iam",
        ACCOUNT_KEY_ID,
        form(action, IAM_API_VERSION, params),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: IAM {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Amazon ECS call signed by `akid`, answered with its status and parsed body.
async fn ecs_as(
    router: &MultiServiceRouter,
    akid: &str,
    operation: &str,
    request: &Value,
) -> (u16, Value) {
    let http_request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("ecs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{ECS_TARGET_PREFIX}.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={akid}/20260101/{REGION}/ecs/aws4_request, \
                 SignedHeaders=host;x-amz-target, Signature=0000"
            ),
        )
        .body(Bytes::from(request.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(http_request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// Create the network and the launch template every group of this suite is built on.
async fn seed_launch_source(router: &MultiServiceRouter) -> String {
    let vpc = element(
        &ec2(router, "CreateVpc", &[("CidrBlock", "10.78.0.0/16")]).await,
        "vpcId",
    );
    let subnet = element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.78.1.0/24"),
                ("AvailabilityZone", "us-east-1a"),
            ],
        )
        .await,
        "subnetId",
    );
    ec2(
        router,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", TEMPLATE),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
        ],
    )
    .await;
    subnet
}

/// Create one Auto Scaling group holding no instances, carrying the tags `tags`, and answer
/// the ARN Amazon EC2 Auto Scaling minted for it.
async fn given_group(
    router: &MultiServiceRouter,
    subnet: &str,
    name: &str,
    tags: &[(&str, &str)],
) -> String {
    let mut params: Vec<(String, String)> = vec![
        ("AutoScalingGroupName".to_string(), name.to_string()),
        ("MinSize".to_string(), "0".to_string()),
        ("MaxSize".to_string(), "2".to_string()),
        ("DesiredCapacity".to_string(), "0".to_string()),
        (
            "LaunchTemplate.LaunchTemplateName".to_string(),
            TEMPLATE.to_string(),
        ),
        ("VPCZoneIdentifier".to_string(), subnet.to_string()),
    ];
    for (index, (key, value)) in tags.iter().enumerate() {
        let position = index + 1;
        params.push((format!("Tags.member.{position}.Key"), (*key).to_string()));
        params.push((format!("Tags.member.{position}.Value"), (*value).to_string()));
        params.push((
            format!("Tags.member.{position}.PropagateAtLaunch"),
            "false".to_string(),
        ));
    }
    let borrowed: Vec<(&str, &str)> = params
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect();
    asg(router, "CreateAutoScalingGroup", &borrowed).await;
    group_arn(router, name).await
}

/// The ARN Amazon EC2 Auto Scaling reports for the group it holds under `name`.
async fn group_arn(router: &MultiServiceRouter, name: &str) -> String {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    element(&body, "AutoScalingGroupARN")
}

/// The tags Amazon EC2 Auto Scaling reports for the group `name`, as
/// `key -> (value, PropagateAtLaunch)` — read through the group's own describe, so what is
/// asserted is what that service persisted.
async fn group_tags(router: &MultiServiceRouter, name: &str) -> BTreeMap<String, (String, String)> {
    let body = asg(
        router,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    // One tag per member, each carrying its key, its value and its propagation decision; the
    // readers take document order, which pairs them.
    elements(&body, "Key")
        .into_iter()
        .zip(elements(&body, "Value"))
        .zip(elements(&body, "PropagateAtLaunch"))
        .map(|((key, value), propagates)| (key, (value, propagates)))
        .collect()
}

/// Create a capacity provider over `group_reference`, signed by `akid`.
async fn create_provider(
    router: &MultiServiceRouter,
    akid: &str,
    name: &str,
    group_reference: &str,
    managed_scaling: bool,
) -> (u16, Value) {
    let mut provider = json!({ "autoScalingGroupArn": group_reference });
    if managed_scaling {
        provider["managedScaling"] = json!({ "status": "ENABLED", "targetCapacity": 80 });
        provider["managedTerminationProtection"] = json!("DISABLED");
    }
    ecs_as(
        router,
        akid,
        "CreateCapacityProvider",
        &json!({ "name": name, "autoScalingGroupProvider": provider }),
    )
    .await
}

/// One row of the marking table: how the provider's binding named the group, and the tags the
/// group was created with.
struct MarkRow {
    case: &'static str,
    group: &'static str,
    provider: &'static str,
    /// Whether the binding names the group by the ARN Auto Scaling minted; otherwise by its
    /// bare name, the other form the member admits.
    by_arn: bool,
    managed_scaling: bool,
    /// The tags the caller created the group with, which the marking must leave alone.
    tags: &'static [(&'static str, &'static str)],
}

const MARK_ROWS: [MarkRow; 3] = [
    MarkRow {
        case: "a provider naming the group by its bare name, over a group the caller tagged",
        group: "ecscp-by-name",
        provider: "cpmark-by-name-provider",
        by_arn: false,
        managed_scaling: false,
        tags: &[("owner", "platform")],
    },
    MarkRow {
        case: "a managed-scaling provider naming the group by the ARN Auto Scaling minted",
        group: "ecscp-by-arn",
        provider: "cpmark-by-arn-provider",
        by_arn: true,
        managed_scaling: true,
        tags: &[],
    },
    MarkRow {
        case: "a provider over a group carrying two tags of the caller's own",
        group: "ecscp-two-tags",
        provider: "cpmark-two-tags-provider",
        by_arn: false,
        managed_scaling: false,
        tags: &[("owner", "platform"), ("tier", "batch")],
    },
];

/// RULE 1: the group a capacity provider binds carries Amazon ECS's marker tag afterwards,
/// beside the tags the caller created it with.
#[tokio::test]
async fn a_capacity_provider_marks_the_auto_scaling_group_it_binds() {
    let router = router();
    let subnet = seed_launch_source(&router).await;
    for row in &MARK_ROWS {
        let arn = given_group(&router, &subnet, row.group, row.tags).await;
        // Before the create, the group carries the caller's tags and nothing else.
        let before = group_tags(&router, row.group).await;
        assert!(
            !before.contains_key(MANAGED_TAG),
            "case {:?}: a group no provider binds carries no {MANAGED_TAG} tag, got {before:?}",
            row.case
        );

        let reference = if row.by_arn { arn.as_str() } else { row.group };
        let (status, answer) = create_provider(
            &router,
            ACCOUNT_KEY_ID,
            row.provider,
            reference,
            row.managed_scaling,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected the provider to be created, got {status} with {answer}",
            row.case
        );

        // THE RULE: the group Amazon EC2 Auto Scaling holds now carries the marker.
        let after = group_tags(&router, row.group).await;
        let marker = after.get(MANAGED_TAG).cloned().unwrap_or_else(|| {
            panic!(
                "case {:?}: expected the bound group to carry the {MANAGED_TAG} tag, got \
                 {after:?}",
                row.case
            )
        });
        assert_eq!(
            marker.1, "true",
            "case {:?}: the marker is added to the instances the group launches, got {marker:?}",
            row.case
        );
        // ...and every tag the caller created the group with is still its own.
        for (key, value) in row.tags {
            assert_eq!(
                after.get(*key).map(|(held, _)| held.as_str()),
                Some(*value),
                "case {:?}: expected the caller's tag {key}={value} to survive the marking, \
                 got {after:?}",
                row.case
            );
        }
        assert_eq!(
            after.len(),
            row.tags.len() + 1,
            "case {:?}: expected the caller's tags and the one marker, got {after:?}",
            row.case
        );
    }
}

/// What a row's principal may do with the Auto Scaling action the marking takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Permitted {
    /// An inline statement allows it.
    Allowed,
    /// An inline statement denies it explicitly.
    ExplicitlyDenied,
    /// No statement mentions it at all, so IAM's default deny stands.
    NeverAllowed,
}

/// One row of the authorization table: what the principal's documents say about
/// `autoscaling:CreateOrUpdateTags`, and whether the create is then served.
struct AuthRow {
    case: &'static str,
    role: &'static str,
    group: &'static str,
    provider: &'static str,
    permitted: Permitted,
}

const AUTH_ROWS: [AuthRow; 3] = [
    AuthRow {
        case: "a principal that may write the group's tags",
        role: "ecscp-allowed",
        group: "ecscp-allowed-group",
        provider: "cpauth-allowed-provider",
        permitted: Permitted::Allowed,
    },
    AuthRow {
        case: "a principal explicitly denied the Auto Scaling tag write",
        role: "ecscp-denied",
        group: "ecscp-denied-group",
        provider: "cpauth-denied-provider",
        permitted: Permitted::ExplicitlyDenied,
    },
    AuthRow {
        case: "a principal whose documents never allow the Auto Scaling tag write",
        role: "ecscp-silent",
        group: "ecscp-silent-group",
        provider: "cpauth-silent-provider",
        permitted: Permitted::NeverAllowed,
    },
];

/// The inline document a row's role carries: the Amazon ECS grant every row holds, plus
/// whatever the row says about the Auto Scaling action.
fn role_document(permitted: Permitted) -> String {
    let autoscaling = match permitted {
        Permitted::Allowed => {
            r#",{"Effect":"Allow","Action":"autoscaling:CreateOrUpdateTags","Resource":"*"}"#
        }
        Permitted::ExplicitlyDenied => {
            r#",{"Effect":"Deny","Action":["autoscaling:CreateOrUpdateTags","autoscaling:UpdateAutoScalingGroup"],"Resource":"*"}"#
        }
        Permitted::NeverAllowed => "",
    };
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Action":"ecs:*","Resource":"*"}}{autoscaling}]}}"#
    )
}

/// Create the role `role` with that document and assume it: answers the session's access key
/// id, which is what the create below is signed by.
async fn session_of(router: &MultiServiceRouter, role: &str, permitted: Permitted) -> String {
    let account = ::engine::emulator_account_id();
    let trust = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{account}:root"}},"Action":"sts:AssumeRole"}}]}}"#
    );
    iam(
        router,
        "CreateRole",
        &[("RoleName", role), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let document = role_document(permitted);
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", role),
            ("PolicyName", "capacity-provider"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    let role_arn = format!("arn:aws:iam::{account}:role/{role}");
    let session = format!("{role}-session");
    let (status, assumed) = send_as(
        router,
        "sts",
        ACCOUNT_KEY_ID,
        form(
            "AssumeRole",
            STS_API_VERSION,
            &[
                ("RoleArn", role_arn.as_str()),
                ("RoleSessionName", session.as_str()),
            ],
        ),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: the role {role} must be assumable, got {status} {assumed}"
    );
    element(&assumed, "AccessKeyId")
}

/// RULE 2: the create needs the Auto Scaling permission its marking takes, and a caller
/// without it gets a 403 and no provider — and no tag on the group either.
#[tokio::test]
async fn a_capacity_provider_create_needs_the_auto_scaling_tag_permission() {
    let router = router();
    let subnet = seed_launch_source(&router).await;
    for row in &AUTH_ROWS {
        given_group(&router, &subnet, row.group, &[("owner", "platform")]).await;
        let akid = session_of(&router, row.role, row.permitted).await;
        let (status, answer) =
            create_provider(&router, &akid, row.provider, row.group, false).await;

        if row.permitted == Permitted::Allowed {
            assert_eq!(
                status, 200,
                "case {:?}: expected the create to be served, got {status} with {answer}",
                row.case
            );
            let tags = group_tags(&router, row.group).await;
            assert!(
                tags.contains_key(MANAGED_TAG),
                "case {:?}: expected the served create to mark the group, got {tags:?}",
                row.case
            );
            continue;
        }

        assert_eq!(
            status, 403,
            "case {:?}: expected HTTP 403, got {status} with {answer}",
            row.case
        );
        let code = answer["__type"]
            .as_str()
            .unwrap_or_default()
            .rsplit(['#', '/'])
            .next()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            code, ACCESS_DENIED,
            "case {:?}: expected the modelled {ACCESS_DENIED}, got {answer}",
            row.case
        );
        assert_eq!(
            answer["message"].as_str().or(answer["Message"].as_str()),
            Some(DENIED_MESSAGE),
            "case {:?}: expected the refusal to say which permission the create needed, got \
             {answer}",
            row.case
        );

        // Nothing was created: the group is left exactly as the caller made it, and Amazon
        // ECS holds no provider under the name the refused call named.
        let tags = group_tags(&router, row.group).await;
        assert!(
            !tags.contains_key(MANAGED_TAG),
            "case {:?}: a refused create must leave the group untagged, got {tags:?}",
            row.case
        );
        let (status, described) = ecs_as(
            &router,
            ACCOUNT_KEY_ID,
            "DescribeCapacityProviders",
            &json!({ "capacityProviders": [row.provider] }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: the read of the refused provider must be served, got {status} \
             {described}",
            row.case
        );
        assert_eq!(
            described["capacityProviders"].as_array().map(Vec::len),
            Some(0),
            "case {:?}: a refused create leaves no provider behind, got {described}",
            row.case
        );
    }
}
