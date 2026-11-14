// SPDX-License-Identifier: CC0-1.0

//! An instance an Auto Scaling group launches RUNS AS the IAM instance profile its launch
//! source names.
//!
//! RULE under test: a capacity change launches each instance under the `IamInstanceProfile`
//! the group's launch source names, so Amazon EC2 holds an instance-profile association
//! over that instance reporting the ARN and the unique id IAM minted for the profile — and
//! every consumer of that association reads the instance's role from it. Systems Manager is
//! one such consumer: its agent registers the instance as a managed node exactly when the
//! role the profile carries lets it, which is why a group's instance appears in
//! `DescribeInstanceInformation` at all, and why a sibling whose role withholds that call
//! never does.
//!
//! The rows below differ along what the rule generalises over: how the launch source spells
//! the reference (`Name`, `Arn`, or none at all), where the group names the template (its
//! own `LaunchTemplate` or the launch template of a `MixedInstancesPolicy`), and what the
//! profile's role says about the call the agent registers with (an AWS-managed policy
//! attachment whose document this emulator holds none of, an inline grant of exactly that
//! call, an inline wildcard over the service, an explicit deny beside such a wildcard, and
//! no policy at all).
//!
//! All four services are mounted on ONE engine and driven over their own wire protocols —
//! awsQuery for Auto Scaling and IAM, ec2Query for Amazon EC2, `awsJson1_1` for Systems
//! Manager — so every assertion is made where a client makes it.
//!
//! Nothing here waits a fixed time: each span is a stored due instant, so a case LOOKS
//! AGAIN until the condition holds or [`DEADLINE`] passes. The groups whose instances must
//! NOT register are created FIRST, so by the time the last registration has been observed
//! every earlier instance's own span has passed too — which is what makes "it is not
//! reported" an answer rather than a race.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_ec2_amazonec2 as ec2_service;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam_service;
use service_com_amazonaws_ssm_amazonssm as ssm_service;

use crate::support::{
    ec2_query, iam_query, json_rpc, p, query, seed_subnets, tag_value, tag_values,
};

/// How long a case waits for a condition another component's clock owes.
///
/// Generous on purpose: an instance's move to `running` and the registration that follows
/// it are each a stored due instant and nothing sleeps for them, so a case looks again
/// until the condition holds rather than waiting a fixed time.
const DEADLINE: Duration = Duration::from_secs(120);

/// How long one look-again cycle pauses before asking again.
const POLL_PAUSE: Duration = Duration::from_millis(50);

/// The AWS-managed policy a client attaches to an instance profile's role so the agent on
/// the instance may register it. Its document is AWS's own, so this emulator holds none for
/// it: a role carrying it carries a grant that cannot be read.
const SSM_CORE_POLICY: &str = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore";

/// The image and instance type every launch template here carries, so one row's template
/// differs from another's only in the reference under test.
const IMAGE_ID: &str = "ami-00000000000000031";
const INSTANCE_TYPE: &str = "t3.micro";

/// Systems Manager's own wire: the signing name, the `X-Amz-Target` prefix and the
/// `awsJson` version a client sends it.
const SSM_SIGNING_NAME: &str = "ssm";
const SSM_TARGET_PREFIX: &str = "AmazonSSM";
const SSM_JSON_VERSION: &str = "1.1";

/// The inline document granting exactly the call the agent registers a node with.
const GRANTS_REGISTRATION: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
    \"Allow\",\"Action\":\"ssm:UpdateInstanceInformation\",\"Resource\":\"*\"}]}";

/// The inline document granting every action of that service.
const GRANTS_EVERY_ACTION: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
    \"Allow\",\"Action\":\"ssm:*\",\"Resource\":\"*\"}]}";

/// The inline document granting every action of that service and DENYING the one the agent
/// registers with — IAM's order, where an explicit deny is final.
const DENIES_REGISTRATION: &str = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\
    \"Allow\",\"Action\":\"ssm:*\",\"Resource\":\"*\"},{\"Effect\":\"Deny\",\"Action\":\
    \"ssm:UpdateInstanceInformation\",\"Resource\":\"*\"}]}";

/// One emulator with all four services on one engine: the group, the instance, the instance
/// profile and the managed node belong to ONE account, and one engine is what makes the
/// four views that account.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ec2_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(iam_service::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(ssm_service::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// What the profile's role says about the call the agent registers a node with.
#[derive(Debug, Clone, Copy)]
enum Grant {
    /// The AWS-managed policy a client attaches for it, whose document this emulator holds
    /// none of.
    AwsManaged,
    /// An inline policy document of the account's own.
    Inline(&'static str),
    /// No policy at all.
    None,
}

/// How the launch template spells the instance profile its instances run as.
#[derive(Debug, Clone, Copy)]
enum Reference {
    /// `IamInstanceProfile.Name`, the name IAM holds the profile under.
    ByName,
    /// `IamInstanceProfile.Arn`, the ARN IAM minted for it.
    ByArn,
    /// The template carries no `IamInstanceProfile` at all.
    Absent,
}

/// Where the group names the launch template under test.
#[derive(Debug, Clone, Copy)]
enum Source {
    /// The group's own `LaunchTemplate`, by name.
    Own,
    /// The launch template of the group's `MixedInstancesPolicy`.
    MixedPolicy,
}

/// One row: a launch template naming a profile a given way, a role granted (or withheld)
/// the registration call, and whether the launched instance must become a managed node.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    reference: Reference,
    source: Source,
    grant: Grant,
    registers: bool,
}

/// The instance profile IAM holds: the name it is addressed by, and the ARN and unique id
/// IAM minted for it — never values this suite spells out.
#[derive(Debug, Clone)]
struct HeldProfile {
    name: String,
    arn: String,
    id: String,
}

/// A trust policy admitting `principal` and nothing else.
fn trust_policy(principal: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"Allow\",\
         \"Principal\":{{\"Service\":\"{principal}\"}},\"Action\":\"sts:AssumeRole\"}}]}}"
    )
}

/// One IAM action that must succeed.
async fn iam_ok(router: &MultiServiceRouter, case: &str, action: &str, params: &[(String, String)]) {
    let (status, body) = iam_query(router, action, params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup {action} must succeed, got {status}: {body}"
    );
}

/// Build one instance profile the way a client builds one — `CreateRole`, the grant,
/// `CreateInstanceProfile`, `AddRoleToInstanceProfile` — and answer the ARN and the id IAM
/// minted for it.
async fn create_profile(router: &MultiServiceRouter, case: &str, name: &str, grant: Grant) -> HeldProfile {
    iam_ok(
        router,
        case,
        "CreateRole",
        &[
            p("RoleName", name),
            // Amazon EC2 has to be able to act as the role for the agent on the instance to
            // have credentials at all.
            p("AssumeRolePolicyDocument", trust_policy("ec2.amazonaws.com")),
        ],
    )
    .await;
    match grant {
        Grant::AwsManaged => {
            iam_ok(
                router,
                case,
                "AttachRolePolicy",
                &[p("RoleName", name), p("PolicyArn", SSM_CORE_POLICY)],
            )
            .await;
        }
        Grant::Inline(document) => {
            iam_ok(
                router,
                case,
                "PutRolePolicy",
                &[
                    p("RoleName", name),
                    p("PolicyName", "agent-registration"),
                    p("PolicyDocument", document),
                ],
            )
            .await;
        }
        Grant::None => {}
    }
    let (status, body) =
        iam_query(router, "CreateInstanceProfile", &[p("InstanceProfileName", name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateInstanceProfile must succeed, got {status}: {body}"
    );
    let arn = tag_value(&body, "Arn");
    let id = tag_value(&body, "InstanceProfileId");
    assert!(
        !arn.is_empty() && !id.is_empty(),
        "case {case:?}: CreateInstanceProfile must report the profile's ARN and id, got {body}"
    );
    iam_ok(
        router,
        case,
        "AddRoleToInstanceProfile",
        &[p("InstanceProfileName", name), p("RoleName", name)],
    )
    .await;
    HeldProfile {
        name: name.to_string(),
        arn,
        id,
    }
}

/// Create one Amazon EC2 launch template over the wire, optionally carrying an
/// `IamInstanceProfile` reference.
async fn create_launch_template(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    reference: Option<(&str, &str)>,
) {
    let mut params = vec![
        p("LaunchTemplateName", name),
        p("LaunchTemplateData.ImageId", IMAGE_ID),
        p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
    ];
    if let Some((member, value)) = reference {
        params.push(p(
            &format!("LaunchTemplateData.IamInstanceProfile.{member}"),
            value,
        ));
    }
    let (status, body) = ec2_query(router, "CreateLaunchTemplate", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateLaunchTemplate must succeed, got {status}: {body}"
    );
}

/// Create one group of capacity one that launches from `template`, placed in `subnet`.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    template: &str,
    source: Source,
    subnet: &str,
) {
    let mut params = vec![
        p("AutoScalingGroupName", name),
        p("MinSize", "1"),
        p("MaxSize", "1"),
        p("DesiredCapacity", "1"),
        p("VPCZoneIdentifier", subnet),
    ];
    match source {
        Source::Own => params.push(p("LaunchTemplate.LaunchTemplateName", template)),
        Source::MixedPolicy => params.push(p(
            "MixedInstancesPolicy.LaunchTemplate.LaunchTemplateSpecification.LaunchTemplateName",
            template,
        )),
    }
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &params
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect::<Vec<_>>(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateAutoScalingGroup must succeed, got {status}: {body}"
    );
}

/// The one instance the group `name` holds, looked for again until it reports exactly one.
async fn sole_instance(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let until = Instant::now() + DEADLINE;
    loop {
        let (status, body) = query(
            router,
            "DescribeAutoScalingGroups",
            &[("AutoScalingGroupNames.member.1", name)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeAutoScalingGroups must answer, got {status}: {body}"
        );
        let reported = tag_values(&body, "InstanceId");
        if let [instance_id] = reported.as_slice() {
            return instance_id.clone();
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected the group to hold one instance, got {reported:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The instance profile Amazon EC2 reports `instance_id` runs as, as `(ARN, id)`, or `None`
/// when it reports none.
async fn instance_profile_of(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Option<(String, String)> {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances({instance_id}) must answer, got {status}: {body}"
    );
    let reported = tag_value(&body, "iamInstanceProfile");
    if reported.is_empty() {
        return None;
    }
    Some((
        tag_value(&reported, "arn"),
        tag_value(&reported, "id"),
    ))
}

/// The `InstanceInformation` Systems Manager reports for `instance_id`, or `None` when it
/// reports none.
async fn node(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Option<Value> {
    let (status, body) = json_rpc(
        router,
        SSM_SIGNING_NAME,
        SSM_TARGET_PREFIX,
        SSM_JSON_VERSION,
        "DescribeInstanceInformation",
        json!({ "Filters": [{ "Key": "InstanceIds", "Values": [instance_id] }] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstanceInformation({instance_id}) must answer, got {status}: \
         {body}"
    );
    body["InstanceInformationList"]
        .as_array()
        .and_then(|reported| reported.first().cloned())
}

/// Look again until Systems Manager reports `instance_id` as a managed node, or [`DEADLINE`]
/// passes.
async fn node_when_registered(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> Value {
    let until = Instant::now() + DEADLINE;
    loop {
        if let Some(reported) = node(router, case, instance_id).await {
            return reported;
        }
        assert!(
            Instant::now() < until,
            "case {case:?}: expected {instance_id} to become a managed node, and \
             DescribeInstanceInformation reported none"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// An instance a group launches runs as the instance profile the group's launch source
/// names, and Systems Manager registers it as a managed node exactly when that profile's
/// role allows the call the agent registers with.
#[tokio::test]
async fn a_groups_instances_run_as_the_launch_sources_instance_profile() {
    // The rows whose instances must NOT register come first, so their own spans have passed
    // by the time the last registration below has been observed.
    let rows = [
        Row {
            case: "a profile whose role carries no policy, named by name",
            reference: Reference::ByName,
            source: Source::Own,
            grant: Grant::None,
            registers: false,
        },
        Row {
            case: "a role denying the registration call beside a wildcard grant, named by \
                   ARN through a mixed instances policy",
            reference: Reference::ByArn,
            source: Source::MixedPolicy,
            grant: Grant::Inline(DENIES_REGISTRATION),
            registers: false,
        },
        Row {
            case: "a launch template naming no instance profile at all",
            reference: Reference::Absent,
            source: Source::Own,
            grant: Grant::None,
            registers: false,
        },
        Row {
            case: "the AWS-managed core policy, named by name",
            reference: Reference::ByName,
            source: Source::Own,
            grant: Grant::AwsManaged,
            registers: true,
        },
        Row {
            case: "an inline grant of the registration call, named by ARN",
            reference: Reference::ByArn,
            source: Source::Own,
            grant: Grant::Inline(GRANTS_REGISTRATION),
            registers: true,
        },
        Row {
            case: "an inline wildcard over the service, named by name through a mixed \
                   instances policy",
            reference: Reference::ByName,
            source: Source::MixedPolicy,
            grant: Grant::Inline(GRANTS_EVERY_ACTION),
            registers: true,
        },
    ];

    // One account for every row: the rows are told apart by the names they create, and one
    // engine is what lets the instances that must not register be launched before the ones
    // that must.
    let (router, engine) = emulator();
    let subnet = seed_subnets(&engine, 1)
        .first()
        .map(|subnet| subnet.id.clone())
        .expect("one seeded subnet");

    let mut launched: Vec<(Row, String, Option<HeldProfile>)> = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let profile = match row.reference {
            Reference::Absent => None,
            _ => Some(create_profile(&router, row.case, &format!("asg-profile-{index}"), row.grant).await),
        };
        let reference = match (row.reference, profile.as_ref()) {
            (Reference::ByName, Some(held)) => Some(("Name", held.name.clone())),
            (Reference::ByArn, Some(held)) => Some(("Arn", held.arn.clone())),
            _ => None,
        };
        let template = format!("lt-{index}");
        create_launch_template(
            &router,
            row.case,
            &template,
            reference
                .as_ref()
                .map(|(member, value)| (*member, value.as_str())),
        )
        .await;
        let group = format!("asg-{index}");
        create_group(&router, row.case, &group, &template, row.source, &subnet).await;
        let instance_id = sole_instance(&router, row.case, &group).await;

        // The half Amazon EC2 owns: the instance runs as the profile IAM minted, reported
        // under the ARN and the unique id that service holds it by.
        let reported = instance_profile_of(&router, row.case, &instance_id).await;
        let expected = profile
            .as_ref()
            .map(|held| (held.arn.clone(), held.id.clone()));
        assert_eq!(
            reported, expected,
            "case {:?}: expected Amazon EC2 to report the instance profile {expected:?} over \
             {instance_id}, got {reported:?}",
            row.case
        );

        launched.push((*row, instance_id, profile));
    }

    // The half Systems Manager owns: the node a registering row's instance becomes.
    for (row, instance_id, _) in &launched {
        if !row.registers {
            continue;
        }
        let reported = node_when_registered(&router, row.case, instance_id).await;
        assert_eq!(
            reported["InstanceId"].as_str(),
            Some(instance_id.as_str()),
            "case {:?}: expected the node to be {instance_id}, got {reported}",
            row.case
        );
        assert_eq!(
            reported["PingStatus"].as_str(),
            Some("Online"),
            "case {:?}: expected the node {instance_id} to report PingStatus Online, got \
             {reported}",
            row.case
        );
        assert_eq!(
            reported["ResourceType"].as_str(),
            Some("EC2Instance"),
            "case {:?}: expected the node {instance_id} to report ResourceType EC2Instance, \
             got {reported}",
            row.case
        );
    }

    // And the rows whose role withholds that call, or that name no profile at all: every
    // one of them was launched before the registrations just observed, so its own span has
    // passed and "no node" is an answer.
    for (row, instance_id, _) in &launched {
        if row.registers {
            continue;
        }
        let reported = node(&router, row.case, instance_id).await;
        assert!(
            reported.is_none(),
            "case {:?}: expected no managed node for {instance_id}, got {reported:?}",
            row.case
        );
    }
}
