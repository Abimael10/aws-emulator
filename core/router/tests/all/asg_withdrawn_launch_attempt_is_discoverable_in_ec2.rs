// SPDX-License-Identifier: CC0-1.0

//! What a client LEARNS about an Auto Scaling capacity increase whose instance cannot be
//! brought into service, over the whole assembled emulator — because the rule only holds if
//! three services agree over one engine: Amazon EC2 Auto Scaling raises the capacity and
//! records the activity, Amazon EC2 mints the attempt and takes it back, and KMS is what
//! refuses the volume's data key.
//!
//! RULE (1): a capacity increase whose launch Amazon EC2 accepts and then withdraws — the
//! encrypted volume its instance would boot cannot be created, because the key it would be
//! encrypted under produces no data key — reaches a TERMINAL scaling activity promptly:
//! `Cancelled` ("if an attempt to launch instances failed, the StatusCode value is Failed or
//! Cancelled and the StatusMessage element in the response indicates the cause of the
//! failure", `DescribeScalingActivities`) at `Progress` 100, carrying the one sentence AWS
//! words for this class, and the attempt is an instance of the account that Amazon EC2 takes
//! all the way back to `terminated` under its own `StateReason` for a key that will not
//! serve. The capacity stays owed, so the SAME group reaches it once the key serves again —
//! whatever made the key unusable, and whatever repairs it.
//!
//! RULE (2): every instance an Auto Scaling group's launch path creates in Amazon EC2 carries
//! the reserved tag `aws:autoscaling:groupName` whose value is the group's name, and EC2's
//! `tag:<key>` filter — "the key/value combination of a tag assigned to the resource. Use the
//! tag key in the filter name and the tag value as the filter value"
//! (`DescribeInstancesRequest$Filters`) — selects on it, so the filter finds that instance in
//! whatever state the launch left it and finds the instances of no other group. A group that
//! was asked for no capacity has nothing for it to find, which is what makes the positive
//! read evidence rather than a vacuous one.
//!
//! The rows of the first test span what rule (1) generalises over: WHAT makes the key unusable
//! (its key state, through `kms:DisableKey` and through `kms:ScheduleKeyDeletion`, and the
//! authorities in force over the group's own service-linked role, through a key policy that
//! names nobody but the account root) and so what REPAIRS it (`kms:EnableKey`,
//! `kms:CancelKeyDeletion` then `kms:EnableKey`, `kms:PutKeyPolicy`). The second test is the
//! differential rule (2) turns on: two groups over ONE launch template, one raised and one
//! left idle.
//!
//! Every expected sentence, code and tag key is written out here rather than taken from a
//! service's own constant: a case that re-used the implementation's literal would assert
//! nothing about it.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The ONE percent-encoder and element readers this test crate keeps for query suites
// (`all/query_wire.rs`: "Nothing new should write another one — call this module").
use crate::query_wire::{block_of, element, elements, form_encode, items_of, members_of};

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The API versions an ec2Query and an awsQuery request here carries, and the `X-Amz-Target`
/// prefix of KMS's awsJson1.1 wire.
const EC2_API_VERSION: &str = "2016-11-15";
const ASG_VERSION: &str = "2011-01-01";
const KMS_TARGET_PREFIX: &str = "TrentService";

/// The AMI, instance type and Availability Zone every launch here names.
const IMAGE_ID: &str = "ami-0abcdef1234567890";
const INSTANCE_TYPE: &str = "t3.micro";
const ZONE: &str = "us-east-1a";

/// The root device every launch template here describes, and the volume behind it.
const ROOT_DEVICE: &str = "/dev/xvda";
const ROOT_VOLUME_SIZE: &str = "8";
const ROOT_VOLUME_TYPE: &str = "gp3";

/// The reserved tag Amazon EC2 Auto Scaling puts on every instance it launches, naming the
/// group the instance belongs to.
const GROUP_NAME_TAG: &str = "aws:autoscaling:groupName";

/// The `StateReason` Amazon EC2 records on an instance it took back because the KMS key its
/// volumes were to be encrypted under will not produce their data key: the code, and the
/// message that code carries.
const INVALID_KMS_KEY_CODE: &str = "Client.InvalidKMSKey.InvalidState";
const INVALID_KMS_KEY_MESSAGE: &str =
    "Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state";

/// The `Activity$StatusMessage` of the cancelled capacity change: the instance never reached
/// `InService` and was taken back, under the Amazon EC2 reason that says why. Fixed text for
/// this class — it carries no part of the request.
const CANCELLED_STATUS_MESSAGE: &str = "Instance became unhealthy while waiting for instance to \
                                        be in InService state. Termination Reason: \
                                        Client.InvalidKMSKey.InvalidState: The KMS key provided \
                                        is in an incorrect state";

/// What the `Activity$Description` of a launch step opens with, whether the launch landed or
/// not.
const LAUNCH_DESCRIPTION: &str = "Launching a new EC2 instance";

/// The status codes a scaling activity has FINISHED in, from the model's own
/// `ScalingActivityStatusCode`: a client polling for the outcome of a capacity change stops at
/// one of these.
const TERMINAL_STATUS_CODES: &[&str] = &["Successful", "Failed", "Cancelled"];

/// The `Activity$Progress` of a finished activity, which the model bounds to 0..100.
const FINISHED_PROGRESS: &str = "100";

/// The group lifecycle state of a member that is in service, and the instance state names
/// Amazon EC2 reports for an attempt it has taken all the way back and for one that is coming
/// up or up (`instance-state-name` - "pending | running | shutting-down | terminated |
/// stopping | stopped").
const IN_SERVICE: &str = "InService";
const TERMINATED: &str = "terminated";
const COMING_UP: &[&str] = &["pending", "running"];

/// The one key policy a KMS key carries: "the only valid name is `default`"
/// (`PutKeyPolicyRequest$PolicyName`).
const DEFAULT_KEY_POLICY_NAME: &str = "default";

/// The least days `kms:ScheduleKeyDeletion` accepts as its waiting period, so the row that
/// schedules a deletion states a value the operation serves.
const PENDING_WINDOW_IN_DAYS: u32 = 7;

/// How long a leg waits for the outcome it polls for, and how often it looks again. The
/// deadline is generous and only here so a failure is a failure rather than a hang: every leg
/// waits for the CONDITION, never for a length of time.
const DEADLINE: Duration = Duration::from_secs(120);
const POLL: Duration = Duration::from_millis(25);

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`, which is where
/// a service reads the Region and the caller's access key id.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// A `(key, value)` request parameter.
fn p(key: &str, value: impl Into<String>) -> (String, String) {
    (key.to_string(), value.into())
}

/// One form body: the action, the version and the parameters.
fn form(action: &str, version: &str, params: &[(String, String)]) -> String {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in params {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    body
}

/// Send one form-encoded request to the service `signing_name` addresses.
async fn send(router: &MultiServiceRouter, signing_name: &str, body: String) -> (u16, String) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{signing_name}.{REGION}.amazonaws.com/"))
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One Amazon EC2 action that must succeed.
async fn ec2(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = send(router, "ec2", form(action, EC2_API_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "EC2 {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One Auto Scaling action that must succeed.
async fn asg(router: &MultiServiceRouter, action: &str, params: &[(String, String)]) -> String {
    let (status, body) = send(router, "autoscaling", form(action, ASG_VERSION, params)).await;
    assert_eq!(
        status, 200,
        "Auto Scaling {action} must succeed, got {status} with body {body}"
    );
    body
}

/// One KMS operation over its awsJson1.1 wire that must succeed, answered as the parsed body.
async fn kms(router: &MultiServiceRouter, case: &str, operation: &str, body: Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://kms.{REGION}.amazonaws.com/"))
        .header("host", format!("kms.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{KMS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("kms"))
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    assert_eq!(
        status, 200,
        "case {case:?}: KMS {operation} must succeed, got {status} with body {parsed}"
    );
    parsed
}

/// One subnet of one VPC, created through Amazon EC2 itself so the id a group places into is
/// one EC2 minted.
async fn seed_subnet(router: &MultiServiceRouter) -> String {
    let vpc = element(
        &ec2(router, "CreateVpc", &[p("CidrBlock", "10.0.0.0/16")]).await,
        "vpcId",
    );
    element(
        &ec2(
            router,
            "CreateSubnet",
            &[
                p("VpcId", vpc),
                p("CidrBlock", "10.0.1.0/24"),
                p("AvailabilityZone", ZONE),
            ],
        )
        .await,
        "subnetId",
    )
}

/// Create one symmetric customer managed key and answer the `(key id, key ARN)` KMS minted.
async fn create_key(router: &MultiServiceRouter, case: &str) -> (String, String) {
    let answered = kms(router, case, "CreateKey", json!({})).await;
    let metadata = &answered["KeyMetadata"];
    let key_id = metadata["KeyId"].as_str().unwrap_or_default().to_string();
    let arn = metadata["Arn"].as_str().unwrap_or_default().to_string();
    assert!(
        !key_id.is_empty() && !arn.is_empty(),
        "case {case:?}: CreateKey must report the key's id and ARN, got {answered}"
    );
    (key_id, arn)
}

/// The key state KMS reports for `key_id`, read where a client reads it.
async fn key_state(router: &MultiServiceRouter, case: &str, key_id: &str) -> String {
    let answered = kms(router, case, "DescribeKey", json!({ "KeyId": key_id })).await;
    answered["KeyMetadata"]["KeyState"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The key policy that authorizes `role_arn` to use the key for an encrypted volume, beside
/// the account's own root.
///
/// SOURCE: the Amazon EC2 Auto Scaling User Guide's required key policy for a group whose
/// instances boot an encrypted volume — the cryptographic set Amazon EBS performs through the
/// role, and the grant it creates for the volume.
fn service_linked_role_key_policy(role_arn: &str, account: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "Enable IAM User Permissions",
                "Effect": "Allow",
                "Principal": { "AWS": format!("arn:aws:iam::{account}:root") },
                "Action": "kms:*",
                "Resource": "*"
            },
            {
                "Sid": "Allow use of the key",
                "Effect": "Allow",
                "Principal": { "AWS": role_arn },
                "Action": [
                    "kms:Decrypt",
                    "kms:GenerateDataKey*",
                    "kms:DescribeKey",
                    "kms:ReEncrypt*"
                ],
                "Resource": "*"
            },
            {
                "Sid": "Allow attachment of persistent resources",
                "Effect": "Allow",
                "Principal": { "AWS": role_arn },
                "Action": "kms:CreateGrant",
                "Resource": "*",
                "Condition": { "Bool": { "kms:GrantIsForAWSResource": "true" } }
            }
        ]
    })
}

/// Authorize the Auto Scaling service-linked role `role_arn` on the key `key_id`, through the
/// service that owns the key.
async fn authorize_role_on_key(
    router: &MultiServiceRouter,
    case: &str,
    key_id: &str,
    role_arn: &str,
) {
    let account = ::engine::emulator_account_id();
    kms(
        router,
        case,
        "PutKeyPolicy",
        json!({
            "KeyId": key_id,
            "PolicyName": DEFAULT_KEY_POLICY_NAME,
            "Policy": service_linked_role_key_policy(role_arn, account).to_string(),
        }),
    )
    .await;
}

/// Create one launch template whose only version boots an ENCRYPTED root volume under the key
/// `key_reference` names, and answer the template id Amazon EC2 minted.
async fn create_template(
    router: &MultiServiceRouter,
    name: &str,
    key_reference: &str,
) -> String {
    let mapping = "LaunchTemplateData.BlockDeviceMapping.1";
    element(
        &ec2(
            router,
            "CreateLaunchTemplate",
            &[
                p("LaunchTemplateName", name),
                p("LaunchTemplateData.ImageId", IMAGE_ID),
                p("LaunchTemplateData.InstanceType", INSTANCE_TYPE),
                p(&format!("{mapping}.DeviceName"), ROOT_DEVICE),
                p(&format!("{mapping}.Ebs.Encrypted"), "true"),
                p(&format!("{mapping}.Ebs.KmsKeyId"), key_reference),
                p(&format!("{mapping}.Ebs.VolumeSize"), ROOT_VOLUME_SIZE),
                p(&format!("{mapping}.Ebs.VolumeType"), ROOT_VOLUME_TYPE),
                p(&format!("{mapping}.Ebs.DeleteOnTermination"), "true"),
            ],
        )
        .await,
        "launchTemplateId",
    )
}

/// Create one group of at most one instance, holding nothing yet, launching from version 1 of
/// `template_id`, and answer the `ServiceLinkedRoleARN` it reports — the principal the
/// authorities over its launches' references are weighed for.
async fn create_group(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    subnet: &str,
    template_id: &str,
) -> String {
    asg(
        router,
        "CreateAutoScalingGroup",
        &[
            p("AutoScalingGroupName", group),
            p("MinSize", "0"),
            p("MaxSize", "1"),
            p("DesiredCapacity", "0"),
            p("LaunchTemplate.LaunchTemplateId", template_id),
            p("LaunchTemplate.Version", "1"),
            p("VPCZoneIdentifier", subnet),
        ],
    )
    .await;
    let described = described_group(router, group).await;
    let role = elements(&described, "ServiceLinkedRoleARN")
        .into_iter()
        .next()
        .unwrap_or_default();
    assert!(
        !role.is_empty(),
        "case {case:?}: the group must report the service-linked role it is bound to, got \
         {described}"
    );
    role
}

/// Ask `group` for `capacity` units of capacity.
async fn set_desired_capacity(router: &MultiServiceRouter, group: &str, capacity: u32) {
    asg(
        router,
        "SetDesiredCapacity",
        &[
            p("AutoScalingGroupName", group),
            p("DesiredCapacity", capacity.to_string()),
        ],
    )
    .await;
}

/// One group's whole read.
async fn described_group(router: &MultiServiceRouter, group: &str) -> String {
    asg(
        router,
        "DescribeAutoScalingGroups",
        &[p("AutoScalingGroupNames.member.1", group)],
    )
    .await
}

/// The `(instance id, lifecycle state)` of every instance `group` holds, as the group's own
/// read reports them.
async fn group_instances(router: &MultiServiceRouter, group: &str) -> Vec<(String, String)> {
    let body = described_group(router, group).await;
    let Some(block) = block_of(&body, "Instances") else {
        return Vec::new();
    };
    members_of(block)
        .into_iter()
        .filter_map(|member| {
            let id = elements(member, "InstanceId").into_iter().next()?;
            let state = elements(member, "LifecycleState").into_iter().next()?;
            Some((id, state))
        })
        .collect()
}

/// Look again until `group` holds exactly `wanted` instances and every one of them is in
/// service, and answer them.
async fn in_service_instances(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let held = group_instances(router, group).await;
        if held.len() == wanted && held.iter().all(|(_, state)| state == IN_SERVICE) {
            return held.into_iter().map(|(id, _)| id).collect();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {group} must hold {wanted} instance(s) in service, got {held:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// What a client reads off one scaling activity.
#[derive(Debug, Clone)]
struct Activity {
    status_code: String,
    status_message: String,
    progress: String,
    description: String,
}

/// Look again until the NEWEST activity of `group` is one that has finished, and answer it.
///
/// `DescribeScalingActivities` reports the activities in progress first and the rest
/// newest-started first, so the first entry is the step a client polling for the outcome of
/// its capacity change is watching. This waits for that OUTCOME rather than for any length of
/// time.
async fn settled_activity(router: &MultiServiceRouter, case: &str, group: &str) -> Activity {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let body = asg(
            router,
            "DescribeScalingActivities",
            &[p("AutoScalingGroupName", group)],
        )
        .await;
        let members = block_of(&body, "Activities").map(members_of).unwrap_or_default();
        if let Some(newest) = members.first() {
            let read = |name: &str| {
                elements(newest, name)
                    .into_iter()
                    .next()
                    .unwrap_or_default()
            };
            let status_code = read("StatusCode");
            if TERMINAL_STATUS_CODES.contains(&status_code.as_str()) {
                return Activity {
                    status_code,
                    status_message: read("StatusMessage"),
                    progress: read("Progress"),
                    description: read("Description"),
                };
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the capacity change on {group} must record a terminal activity, got \
             {body}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The instance ids Amazon EC2 answers for the group `group` through the reserved tag Auto
/// Scaling marks its instances with, narrowed to one `instance-state-name` when `state` names
/// one.
async fn tagged_instances(
    router: &MultiServiceRouter,
    group: &str,
    state: Option<&str>,
) -> Vec<String> {
    let mut params = vec![
        p("Filter.1.Name", format!("tag:{GROUP_NAME_TAG}")),
        p("Filter.1.Value.1", group),
    ];
    if let Some(state) = state {
        params.push(p("Filter.2.Name", "instance-state-name"));
        params.push(p("Filter.2.Value.1", state));
    }
    let mut ids = elements(&ec2(router, "DescribeInstances", &params).await, "instanceId");
    ids.sort();
    ids.dedup();
    ids
}

/// Look again until the tag-filtered read answers at least one instance for `group`, and
/// answer the ids.
async fn attempted_instances(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let ids = tagged_instances(router, group, None).await;
        if !ids.is_empty() {
            return ids;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Amazon EC2 must hold an instance tagged {GROUP_NAME_TAG}={group} \
             after the group's launch attempt, got {ids:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// What Amazon EC2 reports about ONE instance: its state name, its `(code, message)` state
/// reason, and its tags — read out of a `DescribeInstances` that named exactly that instance,
/// so every member belongs to it.
#[derive(Debug, Clone)]
struct Instance {
    state: String,
    reason_code: String,
    reason_message: String,
    tags: BTreeMap<String, String>,
}

async fn described_instance(router: &MultiServiceRouter, instance_id: &str) -> Instance {
    let body = ec2(
        router,
        "DescribeInstances",
        &[p("InstanceId.1", instance_id)],
    )
    .await;
    let state = block_of(&body, "instanceState")
        .and_then(|state| elements(state, "name").into_iter().next())
        .unwrap_or_default();
    let reason = block_of(&body, "stateReason");
    let read = |name: &str| {
        reason
            .and_then(|reason| elements(reason, name).into_iter().next())
            .unwrap_or_default()
    };
    let tags = block_of(&body, "tagSet")
        .map(|tag_set| {
            items_of(tag_set)
                .into_iter()
                .filter_map(|item| {
                    let key = elements(item, "key").into_iter().next()?;
                    let value = elements(item, "value").into_iter().next()?;
                    Some((key, value))
                })
                .collect()
        })
        .unwrap_or_default();
    Instance {
        state,
        reason_code: read("code"),
        reason_message: read("message"),
        tags,
    }
}

/// Look again until Amazon EC2 has taken the attempt `instance_id` all the way back, and
/// answer what it reports then.
///
/// The take-back is owed on Amazon EC2's own clock — the launch was accepted, so the instance
/// is `pending`, then `shutting-down`, then `terminated` — so this waits for the OUTCOME.
async fn withdrawn_attempt(router: &MultiServiceRouter, case: &str, instance_id: &str) -> Instance {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let reported = described_instance(router, instance_id).await;
        if reported.state == TERMINATED {
            return reported;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the attempt {instance_id} must be taken back, got {reported:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// What makes a row's key unusable for the volume the group's instance would boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Unusable {
    /// The key is turned off through `kms:DisableKey`.
    Disabled,
    /// The key is scheduled for deletion through `kms:ScheduleKeyDeletion`.
    PendingDeletion,
    /// The key is one KMS serves, but its policy authorizes nobody but the account root, so
    /// the group's own service-linked role may not use it.
    Unauthorized,
}

impl Unusable {
    /// The key state a client must be able to read before the launch: the precondition the
    /// case rests on, asserted from the emulator's own state rather than assumed.
    fn state_before(self) -> &'static str {
        match self {
            Self::Disabled => "Disabled",
            Self::PendingDeletion => "PendingDeletion",
            Self::Unauthorized => "Enabled",
        }
    }

    /// Whether the key's policy names the group's role from the start: everything but the
    /// authorization row varies the key and not the authorities over it.
    fn authorized_from_the_start(self) -> bool {
        self != Self::Unauthorized
    }

    /// Make the key unusable, through the service that owns it.
    async fn apply(self, router: &MultiServiceRouter, case: &str, key_id: &str) {
        match self {
            Self::Disabled => {
                kms(router, case, "DisableKey", json!({ "KeyId": key_id })).await;
            }
            Self::PendingDeletion => {
                kms(
                    router,
                    case,
                    "ScheduleKeyDeletion",
                    json!({ "KeyId": key_id, "PendingWindowInDays": PENDING_WINDOW_IN_DAYS }),
                )
                .await;
            }
            // The key as `CreateKey` made it already carries the default policy, which names
            // the account root and nobody else: nothing to do.
            Self::Unauthorized => {}
        }
    }

    /// Repair it, so the very same key now serves the volume.
    async fn repair(self, router: &MultiServiceRouter, case: &str, key_id: &str, role: &str) {
        match self {
            Self::Disabled => {
                kms(router, case, "EnableKey", json!({ "KeyId": key_id })).await;
            }
            Self::PendingDeletion => {
                // Cancelling the deletion leaves the key DISABLED, so the repair is both
                // calls: the key is usable again only once it is turned back on.
                kms(router, case, "CancelKeyDeletion", json!({ "KeyId": key_id })).await;
                kms(router, case, "EnableKey", json!({ "KeyId": key_id })).await;
            }
            Self::Unauthorized => authorize_role_on_key(router, case, key_id, role).await,
        }
    }
}

/// One row of the first test: what made the key unusable, and the names the row's own
/// resources take.
struct Row {
    case: &'static str,
    group: &'static str,
    template: &'static str,
    unusable: Unusable,
}

#[tokio::test]
async fn a_launch_withdrawn_under_an_unusable_volume_key_is_answerable_and_repairable() {
    let rows = [
        Row {
            case: "a key turned off while the group is raised",
            group: "asg-kms-withdrawn-disabled",
            template: "lt-asg-kms-withdrawn-disabled",
            unusable: Unusable::Disabled,
        },
        Row {
            case: "a key scheduled for deletion",
            group: "asg-kms-withdrawn-pending-deletion",
            template: "lt-asg-kms-withdrawn-pending-deletion",
            unusable: Unusable::PendingDeletion,
        },
        Row {
            case: "a key the group's service-linked role may not use",
            group: "asg-kms-withdrawn-unauthorized",
            template: "lt-asg-kms-withdrawn-unauthorized",
            unusable: Unusable::Unauthorized,
        },
    ];

    let router = router();
    let subnet = seed_subnet(&router).await;

    for row in rows {
        let case = row.case;
        let (key_id, key_arn) = create_key(&router, case).await;
        let template = create_template(&router, row.template, &key_arn).await;
        let group = create_group(&router, case, row.group, &subnet, &template).await;
        if row.unusable.authorized_from_the_start() {
            authorize_role_on_key(&router, case, &key_id, &group).await;
        }
        row.unusable.apply(&router, case, &key_id).await;
        let state = key_state(&router, case, &key_id).await;
        assert_eq!(
            state,
            row.unusable.state_before(),
            "case {case:?}: expected the key to be observably {} before the launch, got \
             {state:?}",
            row.unusable.state_before()
        );

        // The capacity increase the whole case is about.
        set_desired_capacity(&router, row.group, 1).await;

        // (1) It reaches a terminal activity, and that activity says what became of the
        // launch: cancelled, finished, carrying the sentence for this class and the step it
        // set out to take.
        let blocked = settled_activity(&router, case, row.group).await;
        assert_eq!(
            (
                blocked.status_code.as_str(),
                blocked.status_message.as_str(),
                blocked.progress.as_str()
            ),
            (
                "Cancelled",
                CANCELLED_STATUS_MESSAGE,
                FINISHED_PROGRESS
            ),
            "case {case:?}: expected a cancelled activity at {FINISHED_PROGRESS} carrying the \
             key's refusal, got {blocked:?}"
        );
        assert!(
            blocked.description.starts_with(LAUNCH_DESCRIPTION),
            "case {case:?}: expected the activity to describe {LAUNCH_DESCRIPTION:?}, got \
             {blocked:?}"
        );
        let held = group_instances(&router, row.group).await;
        assert!(
            held.is_empty(),
            "case {case:?}: the group must hold no instance while its volume's key will not \
             serve, got {held:?}"
        );

        // (2) The attempt is an instance of the account, found through the reserved tag and
        // carrying it — the only way a client can attribute it to this group at all.
        let attempts = attempted_instances(&router, case, row.group).await;
        assert_eq!(
            attempts.len(),
            1,
            "case {case:?}: the one unit of capacity asked for makes one attempt, got \
             {attempts:?}"
        );
        let attempt = attempts[0].clone();
        let taken_back = withdrawn_attempt(&router, case, &attempt).await;
        assert_eq!(
            (
                taken_back.reason_code.as_str(),
                taken_back.reason_message.as_str()
            ),
            (INVALID_KMS_KEY_CODE, INVALID_KMS_KEY_MESSAGE),
            "case {case:?}: expected the attempt {attempt} to be taken back under \
             {INVALID_KMS_KEY_CODE}, got {taken_back:?}"
        );
        assert_eq!(
            taken_back.tags.get(GROUP_NAME_TAG).map(String::as_str),
            Some(row.group),
            "case {case:?}: expected the attempt {attempt} to carry \
             {GROUP_NAME_TAG}={}, got {:?}",
            row.group,
            taken_back.tags
        );
        // And the filter finds it in the state the launch left it in, which is a terminal one:
        // "tag:<key>" selects on the tag, not on what became of the instance.
        assert_eq!(
            tagged_instances(&router, row.group, Some(TERMINATED)).await,
            vec![attempt.clone()],
            "case {case:?}: the terminated attempt must still answer the group's tag filter"
        );

        // Repairing the key is all it takes: the capacity stayed owed, so the same group
        // reaches it.
        row.unusable
            .repair(&router, case, &key_id, &group)
            .await;
        let repaired = key_state(&router, case, &key_id).await;
        assert_eq!(
            repaired, "Enabled",
            "case {case:?}: the repaired key must be observably enabled, got {repaired:?}"
        );
        set_desired_capacity(&router, row.group, 1).await;
        let served = settled_activity(&router, case, row.group).await;
        assert_eq!(
            (served.status_code.as_str(), served.status_message.as_str()),
            ("Successful", ""),
            "case {case:?}: the launch made once the key serves must be successful and carry no \
             status message, got {served:?}"
        );
        let in_service = in_service_instances(&router, case, row.group, 1).await;
        let member = in_service[0].clone();
        assert_ne!(
            member, attempt,
            "case {case:?}: the instance in service must be a new launch, not the withdrawn \
             attempt {attempt}"
        );
        let running = described_instance(&router, &member).await;
        assert!(
            COMING_UP.contains(&running.state.as_str()),
            "case {case:?}: expected the member {member} to be one of {COMING_UP:?} in Amazon \
             EC2, got {running:?}"
        );
        assert_eq!(
            running.tags.get(GROUP_NAME_TAG).map(String::as_str),
            Some(row.group),
            "case {case:?}: expected the member {member} to carry {GROUP_NAME_TAG}={}, got {:?}",
            row.group,
            running.tags
        );
        // Both the attempt and the member answer the group's tag filter: the tag is on every
        // instance the group's launch path created, whatever state each is in.
        let mut both = vec![attempt, member];
        both.sort();
        assert_eq!(
            tagged_instances(&router, row.group, None).await,
            both,
            "case {case:?}: the group's tag filter must answer every instance its launches \
             created"
        );
    }
}

#[tokio::test]
async fn the_group_name_tag_tells_a_raised_group_from_an_idle_one() {
    let case = "two groups over one launch template, one raised and one left idle";
    let control = "asg-kms-tag-control";
    let idle = "asg-kms-tag-idle";

    let router = router();
    let subnet = seed_subnet(&router).await;
    // One key, disabled before either group is created, so the control group's launch is one
    // Amazon EC2 takes back: the attempt is what the tag filter has to find.
    let (key_id, key_arn) = create_key(&router, case).await;
    kms(&router, case, "DisableKey", json!({ "KeyId": &key_id })).await;
    let state = key_state(&router, case, &key_id).await;
    assert_eq!(
        state, "Disabled",
        "case {case:?}: the key must be observably disabled before either group is created, got \
         {state:?}"
    );
    let template = create_template(&router, "lt-asg-kms-tag-shared", &key_arn).await;
    let role = create_group(&router, case, control, &subnet, &template).await;
    create_group(&router, case, idle, &subnet, &template).await;
    authorize_role_on_key(&router, case, &key_id, &role).await;

    // Only the control group is asked for capacity.
    set_desired_capacity(&router, control, 1).await;
    let blocked = settled_activity(&router, case, control).await;
    assert_eq!(
        blocked.status_code, "Cancelled",
        "case {case:?}: the control group's launch attempt must reach a terminal activity, got \
         {blocked:?}"
    );
    assert!(
        blocked.description.starts_with(LAUNCH_DESCRIPTION),
        "case {case:?}: expected the control group's activity to describe \
         {LAUNCH_DESCRIPTION:?}, got {blocked:?}"
    );

    let attempts = attempted_instances(&router, case, control).await;
    assert_eq!(
        attempts.len(),
        1,
        "case {case:?}: the control group's one unit of capacity makes one attempt, got \
         {attempts:?}"
    );
    // The differential the rule turns on: the group that was asked for nothing has nothing for
    // the same filter to find, which is what makes the control group's answer evidence.
    let idle_ids = tagged_instances(&router, idle, None).await;
    assert!(
        idle_ids.is_empty(),
        "case {case:?}: the idle group has launched nothing, so its tag filter must answer no \
         instance, got {idle_ids:?}"
    );
    // And the filter is exact in the other direction too: the control group's attempt carries
    // the control group's name, so a filter naming the idle group never selects it.
    let attempt = attempts[0].clone();
    let reported = described_instance(&router, &attempt).await;
    assert_eq!(
        reported.tags.get(GROUP_NAME_TAG).map(String::as_str),
        Some(control),
        "case {case:?}: expected the attempt {attempt} to carry {GROUP_NAME_TAG}={control}, got \
         {reported:?}"
    );
}
