// SPDX-License-Identifier: CC0-1.0

//! WHO may attach a target group to an Auto Scaling group, and what that attachment then
//! REGISTERS — one rule, driven over the wires of the five services it takes.
//!
//! RULE: an Auto Scaling action that is not an account-wide read is authorized by IAM against
//! the ARN of the group the request addresses BEFORE the group is read or written, so a caller
//! whose identity-based documents hold no `Allow` of `autoscaling:AttachLoadBalancerTargetGroups`
//! — or hold a `Deny` of it — is refused HTTP 403 `AccessDenied` naming the principal the
//! credential speaks for, the action, the group's own ARN and which of IAM's rules withheld the
//! grant, and the group is left attached to nothing; and because an attachment is what makes the
//! group's instances targets of that target group — `CreateAutoScalingGroup$TargetGroupARNs` is
//! "the Amazon Resource Names (ARN) of the target groups to associate with the Auto Scaling
//! group. Instances are registered as targets in a target group" — a REFUSED attach registers
//! nothing with Elastic Load Balancing v2, while a permitted one registers every instance the
//! group already holds in service, which `DescribeTargetHealth` then reports.
//!
//! The two halves only mean anything together, and only across services, which is why the suite
//! lives here: IAM holds the documents, STS mints the session a refusal blames, Amazon EC2 holds
//! the VPC, the security group and the launch template the group launches from, Amazon EC2 Auto
//! Scaling takes the decision and owns the group, and Elastic Load Balancing v2 holds the target
//! group and answers which targets are registered in it. Every expected value is either a
//! literal this suite declared or an identity read back out of the service that minted it.
//!
//! The rows span what the rule generalises over: the documents in force on the caller (none at
//! all, an allow of this service's reads only, an allow of the whole service, an allow of
//! exactly this action, an allow of the whole service beside a deny of this action, and an allow
//! of the whole service beside a deny of a DIFFERENT action), and how many instances the group
//! holds in service when the attach arrives (one, two) — since the registrations an attachment
//! owes are per instance.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{block_of, elements, form_encode};

/// The Region every request below is signed for, so the ARNs the decisions are taken about are
/// the ones this Region's resources carry.
const REGION: &str = "us-east-1";

/// The API versions each service's query request carries.
const EC2_VERSION: &str = "2016-11-15";
const ELBV2_VERSION: &str = "2015-12-01";
const ASG_VERSION: &str = "2011-01-01";
const IAM_VERSION: &str = "2010-05-08";
const STS_VERSION: &str = "2011-06-15";

/// Elastic Load Balancing's two models share one endpoint, so the v2 model is addressed by the
/// routing key that names IT and the API version only it declares.
const ELBV2_KEY: &str = "elasticloadbalancingv2";

/// The AMI and instance type the launch template every group here launches from names, and the
/// name of that launch template.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";
const LAUNCH_TEMPLATE: &str = "asg-attach-template";

/// The access-key id standing for the account's own published credential: a key neither STS nor
/// IAM holds a record for, so it resolves to the account's ambient identity, which holds no IAM
/// documents — the position AWS account root credentials are in. Every setup and read-back leg
/// is signed with it, so a row asserts what the RESTRICTED caller changed and nothing else.
const ACCOUNT_KEY: &str = "EMULATORTESTKEY12345";

/// This service's IAM action prefix, the operation under test, and the code an unauthorized
/// caller is refused with — spelled out here rather than imported, so a wrong constant in the
/// implementation cannot make a row pass.
const ACTION_PREFIX: &str = "autoscaling";
const OPERATION: &str = "AttachLoadBalancerTargetGroups";
const ACCESS_DENIED: &str = "AccessDenied";

/// The `Instance$LifecycleState` of an instance the group has finished putting into service.
const IN_SERVICE: &str = "InService";

/// The port and target type of every target group here.
const TARGET_GROUP_PORT: &str = "80";
const TARGET_TYPE_INSTANCE: &str = "instance";

/// How long a row waits for the clock-owed moves it depends on — the instances the group
/// launches reaching service, and the registrations an attachment then owes — and how often it
/// looks again. Generous, because the deadline is only here so a failure is a failure rather
/// than a hang: every wait below looks for its CONDITION, so machine load decides when a row
/// finishes and never whether it passes.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(100);

/// How many times a row re-reads a target group that must stay EMPTY after a refused attach: the
/// absence is asserted over a run of reads, so a registration that arrives late still fails.
const HELD_READS: usize = 15;

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern; which IDENTITY the access key id of a
    // request speaks for is this suite's subject, and that is read off the credential either way.
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

/// One query request to the service `signing_name` addresses, signed with `access_key_id`, and
/// the status and body it answers.
async fn call(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    access_key_id: &str,
    action: &str,
    params: &[(&str, &str)],
) -> (u16, String) {
    // IAM is a global service, addressed without a Region; every other service here is
    // addressed in the Region its resources belong to.
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
                "AWS4-HMAC-SHA256 Credential={access_key_id}/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .body(Bytes::from(form(action, version, params)))
        .expect("a well-formed query request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One call signed as the account itself, which must succeed: the suite's setup and read-back
/// legs.
async fn served(
    router: &MultiServiceRouter,
    signing_name: &str,
    version: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let (status, body) = call(router, signing_name, version, ACCOUNT_KEY, action, params).await;
    assert_eq!(
        status, 200,
        "the account's own {signing_name} {action} must succeed, got {status} with {body}"
    );
    body
}

/// The first `<name>` element of an answer, with the XML entities decoded.
fn value(body: &str, name: &str) -> String {
    elements(body, name)
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("no <{name}> element in {body}"))
}

/// The network one row's instances and target group belong to, created through Amazon EC2 itself
/// so every id asserted against is one EC2 minted.
struct Network {
    vpc: String,
    subnet: String,
}

/// Create the VPC, the subnet, the security group and the launch template every group here
/// launches from.
async fn seed_network(router: &MultiServiceRouter) -> Network {
    let vpc = value(
        &served(router, "ec2", EC2_VERSION, "CreateVpc", &[("CidrBlock", "10.90.0.0/16")]).await,
        "vpcId",
    );
    let subnet = value(
        &served(
            router,
            "ec2",
            EC2_VERSION,
            "CreateSubnet",
            &[
                ("VpcId", vpc.as_str()),
                ("CidrBlock", "10.90.1.0/24"),
                ("AvailabilityZone", "us-east-1a"),
            ],
        )
        .await,
        "subnetId",
    );
    let group = value(
        &served(
            router,
            "ec2",
            EC2_VERSION,
            "CreateSecurityGroup",
            &[
                ("GroupName", "asg-attach-instances"),
                ("GroupDescription", "the instances of this suite"),
                ("VpcId", vpc.as_str()),
            ],
        )
        .await,
        "groupId",
    );
    served(
        router,
        "ec2",
        EC2_VERSION,
        "CreateLaunchTemplate",
        &[
            ("LaunchTemplateName", LAUNCH_TEMPLATE),
            ("LaunchTemplateData.ImageId", IMAGE_ID),
            ("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
            // `RequestLaunchTemplateData$SecurityGroupIds` travels as its own `xmlName`,
            // `SecurityGroupId`, which is what an ec2Query client sends.
            ("LaunchTemplateData.SecurityGroupId.1", group.as_str()),
        ],
    )
    .await;
    Network { vpc, subnet }
}

/// Create one target group of `vpc` and answer the ARN Elastic Load Balancing v2 minted for it.
async fn create_target_group(router: &MultiServiceRouter, name: &str, vpc: &str) -> String {
    value(
        &served(
            router,
            ELBV2_KEY,
            ELBV2_VERSION,
            "CreateTargetGroup",
            &[
                ("Name", name),
                ("Protocol", "HTTP"),
                ("Port", TARGET_GROUP_PORT),
                ("VpcId", vpc),
                ("TargetType", TARGET_TYPE_INSTANCE),
            ],
        )
        .await,
        "TargetGroupArn",
    )
}

/// Create one Auto Scaling group holding `capacity` instances in `subnet`, attached to nothing.
async fn create_group(router: &MultiServiceRouter, name: &str, subnet: &str, capacity: usize) {
    let capacity = capacity.to_string();
    served(
        router,
        ACTION_PREFIX,
        ASG_VERSION,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            ("LaunchTemplate.LaunchTemplateName", LAUNCH_TEMPLATE),
            ("MinSize", capacity.as_str()),
            ("MaxSize", capacity.as_str()),
            ("DesiredCapacity", capacity.as_str()),
            ("VPCZoneIdentifier", subnet),
        ],
    )
    .await;
}

/// The `(InstanceId, LifecycleState)` pairs the group `name` reports, read as the account.
async fn group_instances(router: &MultiServiceRouter, name: &str) -> Vec<(String, String)> {
    let body = served(
        router,
        ACTION_PREFIX,
        ASG_VERSION,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    let Some(instances) = block_of(&body, "Instances") else {
        return Vec::new();
    };
    elements(instances, "InstanceId")
        .into_iter()
        .zip(elements(instances, "LifecycleState"))
        .collect()
}

/// Wait until the group `name` holds exactly `capacity` instances in service, and answer their
/// ids: the launch and the move into service are clock-owed, so the wait is for the CONDITION.
async fn in_service_instances(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    capacity: usize,
) -> BTreeSet<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let held = group_instances(router, name).await;
        let in_service: BTreeSet<String> = held
            .iter()
            .filter(|(_, state)| state == IN_SERVICE)
            .map(|(id, _)| id.clone())
            .collect();
        if in_service.len() == capacity {
            return in_service;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the group to hold {capacity} instance(s) in service, got \
             {held:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The ARN the group `name` carries, as the service that minted it reports it — the resource IAM
/// judges an action on that group against.
async fn group_arn(router: &MultiServiceRouter, name: &str) -> String {
    let body = served(
        router,
        ACTION_PREFIX,
        ASG_VERSION,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    value(&body, "AutoScalingGroupARN")
}

/// The target groups the group `name` holds, in the order it holds them.
async fn attached_target_groups(router: &MultiServiceRouter, name: &str) -> Vec<String> {
    let body = served(
        router,
        ACTION_PREFIX,
        ASG_VERSION,
        "DescribeAutoScalingGroups",
        &[("AutoScalingGroupNames.member.1", name)],
    )
    .await;
    block_of(&body, "TargetGroupARNs").map_or_else(Vec::new, |inner| elements(inner, "member"))
}

/// The targets registered in the target group `arn` right now, as a client reads them.
async fn registered_targets(router: &MultiServiceRouter, arn: &str) -> BTreeSet<String> {
    let body = served(
        router,
        ELBV2_KEY,
        ELBV2_VERSION,
        "DescribeTargetHealth",
        &[("TargetGroupArn", arn)],
    )
    .await;
    elements(&body, "Id").into_iter().collect()
}

/// A one-document identity-based policy carrying `statements` — `(Effect, Action)` over every
/// resource, the shape a scoped-down role carries.
fn document(statements: &[(&str, &str)]) -> String {
    let written: Vec<String> = statements
        .iter()
        .map(|(effect, action)| {
            format!(r#"{{"Effect":"{effect}","Action":"{action}","Resource":"*"}}"#)
        })
        .collect();
    format!(
        r#"{{"Version":"2012-10-17","Statement":[{}]}}"#,
        written.join(",")
    )
}

/// Create the role `role_name` carrying `statements` as its one inline policy (and no inline
/// policy at all when there are none), assume it, and answer the session's access key id with
/// the ARN `sts:GetCallerIdentity` reports for it — the principal a refusal must blame.
async fn session_of(
    router: &MultiServiceRouter,
    role_name: &str,
    statements: &[(&str, &str)],
) -> (String, String) {
    let account = engine::emulator_account_id();
    let trust = format!(
        r#"{{"Version":"2012-10-17","Statement":[{{"Effect":"Allow","Principal":{{"AWS":"arn:aws:iam::{account}:root"}},"Action":"sts:AssumeRole"}}]}}"#
    );
    served(
        router,
        "iam",
        IAM_VERSION,
        "CreateRole",
        &[
            ("RoleName", role_name),
            ("AssumeRolePolicyDocument", trust.as_str()),
        ],
    )
    .await;
    if !statements.is_empty() {
        let policy = document(statements);
        served(
            router,
            "iam",
            IAM_VERSION,
            "PutRolePolicy",
            &[
                ("RoleName", role_name),
                ("PolicyName", "inline"),
                ("PolicyDocument", policy.as_str()),
            ],
        )
        .await;
    }
    let role_arn = format!("arn:aws:iam::{account}:role/{role_name}");
    let session_name = format!("{role_name}-session");
    let assumed = served(
        router,
        "sts",
        STS_VERSION,
        "AssumeRole",
        &[
            ("RoleArn", role_arn.as_str()),
            ("RoleSessionName", session_name.as_str()),
        ],
    )
    .await;
    (value(&assumed, "AccessKeyId"), value(&assumed, "Arn"))
}

/// What a row expects of the attach it drives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expect {
    /// HTTP 200: the documents in force allow the action.
    Served,
    /// HTTP 403 `AccessDenied` because a statement about the request says `Deny`.
    ExplicitDeny,
    /// HTTP 403 `AccessDenied` because no statement about the request allows it.
    NoAllow,
}

/// The sentence AWS refuses `principal` with for `action` on `resource`, with the clause naming
/// which of IAM's rules the decision rested on.
fn refusal_message(principal: &str, action: &str, resource: &str, expected: Expect) -> String {
    let reason = match expected {
        Expect::ExplicitDeny => "with an explicit deny in an identity-based policy".to_string(),
        Expect::NoAllow => format!("because no identity-based policy allows the {action} action"),
        Expect::Served => unreachable!("a served call carries no refusal"),
    };
    format!(
        "User: {principal} is not authorized to perform: {action} on resource: {resource} {reason}"
    )
}

/// One row: the case, the role it calls as, that role's statements, what the attach answers, and
/// how many instances the group holds in service when it arrives.
type Row = (
    &'static str,
    &'static str,
    Vec<(&'static str, &'static str)>,
    Expect,
    usize,
);

/// THE RULE: the attachment is authorized against the group's own ARN before anything happens,
/// so an unauthorized caller is refused with IAM's own sentence and NOTHING is registered with
/// Elastic Load Balancing v2 — while a permitted caller's attach registers every instance the
/// group holds in service.
#[tokio::test]
async fn an_attachment_registers_the_groups_instances_only_for_a_permitted_caller() {
    let router = router();
    let network = seed_network(&router).await;

    let rows: Vec<Row> = vec![
        (
            "a role carrying no policy at all",
            "asg-attach-bare",
            vec![],
            Expect::NoAllow,
            1,
        ),
        (
            "a role allowed only the read actions a monitoring client needs",
            "asg-attach-reader",
            vec![
                ("Allow", "autoscaling:Describe*"),
                ("Allow", "elasticloadbalancing:Describe*"),
                ("Allow", "ec2:Describe*"),
            ],
            Expect::NoAllow,
            1,
        ),
        (
            "a role allowed this whole service beside an explicit deny of this action",
            "asg-attach-denied",
            vec![
                ("Allow", "autoscaling:*"),
                ("Deny", "autoscaling:AttachLoadBalancerTargetGroups"),
            ],
            Expect::ExplicitDeny,
            2,
        ),
        (
            "a role allowed every action of this service",
            "asg-attach-service",
            vec![("Allow", "autoscaling:*")],
            Expect::Served,
            1,
        ),
        (
            "a role allowed exactly this action",
            "asg-attach-action",
            vec![("Allow", "autoscaling:AttachLoadBalancerTargetGroups")],
            Expect::Served,
            2,
        ),
        (
            "a role allowed this whole service beside a deny of a DIFFERENT action",
            "asg-attach-other-deny",
            vec![
                ("Allow", "autoscaling:*"),
                ("Deny", "autoscaling:DeleteAutoScalingGroup"),
            ],
            Expect::Served,
            1,
        ),
    ];

    for (row, (case, role_name, statements, expected, capacity)) in rows.iter().enumerate() {
        let (access_key_id, principal) = session_of(&router, role_name, statements).await;
        let target_group =
            create_target_group(&router, &format!("tg-asg-attach-{row}"), &network.vpc).await;
        let name = format!("asg-attach-{row}");
        create_group(&router, &name, &network.subnet, *capacity).await;
        let instances = in_service_instances(&router, case, &name, *capacity).await;
        // The ARN the decision is taken on is the group's own, which it has as soon as it
        // exists, and nothing is registered in the target group before the attach: whatever a
        // row then reads there is what the attach caused.
        let resource = group_arn(&router, &name).await;
        let before = registered_targets(&router, &target_group).await;
        assert!(
            before.is_empty(),
            "case {case:?}: the target group must hold nothing before the attach, got {before:?}"
        );

        let (status, body) = call(
            &router,
            ACTION_PREFIX,
            ASG_VERSION,
            &access_key_id,
            OPERATION,
            &[
                ("AutoScalingGroupName", name.as_str()),
                ("TargetGroupARNs.member.1", target_group.as_str()),
            ],
        )
        .await;

        if *expected == Expect::Served {
            assert_eq!(
                status, 200,
                "case {case:?}: the permitted attach must be served, got {status} with {body}"
            );
            let attached = attached_target_groups(&router, &name).await;
            assert_eq!(
                attached,
                vec![target_group.clone()],
                "case {case:?}: expected the group to hold exactly the attached target group, \
                 got {attached:?}"
            );
            // The registrations the attachment owes are clock-owed, so the wait is for the
            // CONDITION: every instance the group holds in service, registered in the target
            // group it was just attached to, and nothing else.
            let deadline = Instant::now() + DEADLINE;
            loop {
                let registered = registered_targets(&router, &target_group).await;
                if registered == instances {
                    break;
                }
                assert!(
                    Instant::now() < deadline,
                    "case {case:?}: expected the attachment to register {instances:?}, got \
                     {registered:?}"
                );
                tokio::time::sleep(POLL).await;
            }
            continue;
        }

        assert_eq!(
            status, 403,
            "case {case:?}: the unauthorized attach must be refused, got {status} with {body}"
        );
        let code = value(&body, "Code");
        assert_eq!(
            code, ACCESS_DENIED,
            "case {case:?}: expected the {ACCESS_DENIED} code, got {code:?} in {body}"
        );
        let action = format!("{ACTION_PREFIX}:{OPERATION}");
        let message = value(&body, "Message");
        assert_eq!(
            message,
            refusal_message(&principal, &action, &resource, *expected),
            "case {case:?}: expected IAM's refusal wording, got {message:?}"
        );
        let attached = attached_target_groups(&router, &name).await;
        assert!(
            attached.is_empty(),
            "case {case:?}: the refused attach must attach nothing, got {attached:?}"
        );
        // And because nothing was attached, nothing is ever registered: the absence is asserted
        // over a run of reads, so a registration that arrives late still fails the row.
        for read in 0..HELD_READS {
            let registered = registered_targets(&router, &target_group).await;
            assert!(
                registered.is_empty(),
                "case {case:?}, read {read}: a refused attach must register no instance of the \
                 group, got {registered:?}"
            );
            tokio::time::sleep(POLL).await;
        }
    }
}
