// SPDX-License-Identifier: CC0-1.0

//! Whether the AUTHORITIES in force let an Auto Scaling group's own role use the KMS key its
//! instances' root volume is encrypted under decides whether a capacity change can place an
//! instance at all.
//!
//! RULE under test: an encrypted volume is created by asking KMS for the volume's data key,
//! and that ask is made as the group's service-linked role — "the service-linked role that the
//! Auto Scaling group uses to call other Amazon Web Services service on your behalf"
//! (`CreateAutoScalingGroupType$ServiceLinkedRoleARN`) — so a capacity change places an
//! instance only where the authorities over that key let THAT role perform it: the key's own
//! key policy, the role's identity-based policies and the grants the role holds on the key. A
//! key whose authorities do not cover the ask creates no volume, so Amazon EC2 takes the
//! instance it launched back under `Client.InvalidKMSKey.InvalidState`, the instance never
//! reaches `InService`, and the capacity change's terminal scaling activity is `Cancelled`
//! carrying that termination reason — while the capacity the caller asked for stands, so
//! authorizing the role is all it takes to reach it.
//!
//! The rows span what the rule generalises over: the default key policy a key is created with
//! (the account root and nobody else), the statements AWS documents for the role, a policy
//! naming the role for an action the ask is not among, a policy naming another role, the
//! documented statements beside an explicit deny over the role, those statements scoped by the
//! service the ask arrives through and scoped by another service, a grant to the role instead
//! of a policy statement, and the Amazon Web Services managed key of Amazon EBS, whose policy
//! AWS manages for the service that owns it. The last test is the lifecycle the rule turns on:
//! ONE group, cancelled while the key authorizes nobody but the account root and launching
//! once the documented statements name its role.
//!
//! The key STATE half of the same rule — a key no state of which serves, and a reference
//! naming no key — is held to its own table in `launch_volume_encryption_key`, whose harness
//! this suite drives the three services through.

use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::launch_volume_encryption_key::{
    authorize_role_on_key, create_group, create_key, create_launch_template, described_group,
    emulator, group_instances, kms, put_key_policy, service_linked_role_key_policy,
    set_desired_capacity, settled_activity, Volume, DEADLINE, DEFAULT_KEY_POLICY_NAME,
    EBS_MANAGED_KEY_ALIAS, POLL_PAUSE, ROOT_DEVICE, SERVICE_LINKED_ROLE_KEY_ACTIONS,
};
use crate::support::{ec2_query, element_body, p, tag_value, tag_values, REGION};

/// The tag Amazon EC2 Auto Scaling puts on every instance it launches, naming the group the
/// instance belongs to — how a client asks the service that owns the instances which of them
/// a group launched.
///
/// SOURCE: the `aws:autoscaling:groupName` tag of the Amazon EC2 Auto Scaling User Guide's
/// "Tag your Auto Scaling groups and instances". Written out here rather than taken from the
/// implementation's constant: a case that re-used that literal would assert nothing about it.
const GROUP_NAME_TAG: &str = "aws:autoscaling:groupName";

/// The `StateReason$Code` Amazon EC2 reports on an instance it took back because the KMS key
/// its volumes were to be encrypted under produced no data key, and the message that goes
/// with it.
///
/// SOURCE: the recorded Amazon EC2 state reason for this class of refusal. Written out for the
/// same reason the tag above is.
const INVALID_KEY_STATE_CODE: &str = "Client.InvalidKMSKey.InvalidState";
const INVALID_KEY_STATE_MESSAGE: &str =
    "Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state";

/// The one reason Amazon EC2 Auto Scaling records as the `StatusMessage` of a capacity change
/// whose instance was taken back for that reason, written out for the same reason.
const CANCELLED_STATUS_MESSAGE: &str =
    "Instance became unhealthy while waiting for instance to be in InService state. Termination \
     Reason: Client.InvalidKMSKey.InvalidState: The KMS key provided is in an incorrect state";

/// The `InstanceStateName` an instance Amazon EC2 has taken all the way back reports.
const TERMINATED: &str = "terminated";

/// The `LifecycleState` of an instance a group holds in service.
const IN_SERVICE: &str = "InService";

/// What a row's key says about the group's own service-linked role.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Authority {
    /// The key policy `CreateKey` writes for a key created without one: it names the account
    /// root and no other principal.
    DefaultPolicy,
    /// The statements AWS documents for an Auto Scaling group's service-linked role.
    DocumentedStatements,
    /// A policy naming the role for a cryptographic action the volume's own ask is not among.
    AnotherAction,
    /// A policy carrying the documented statements for ANOTHER role of the account.
    AnotherRole,
    /// The documented statements, beside an explicit deny over the same role.
    DocumentedStatementsAndDeny,
    /// The documented statements, scoped to the service the volume's ask arrives through.
    ThroughTheCreatingService,
    /// The documented statements, scoped to some other service.
    ThroughAnotherService,
    /// No statement for the role at all, and a GRANT to it for the operation instead.
    Grant,
    /// The Amazon Web Services managed key of Amazon EBS, whose policy AWS manages.
    AwsManagedKey,
}

impl Authority {
    /// Whether the key the row names is one the ACCOUNT holds the policy of, so the row has a
    /// policy (or a grant) to write at all.
    fn customer_managed(self) -> bool {
        self != Self::AwsManagedKey
    }

    /// Whether the key's own policy must mention the group's role — the precondition each row
    /// rests on, read back from the emulator's own state before the capacity change.
    fn policy_names_the_role(self) -> bool {
        matches!(
            self,
            Self::DocumentedStatements
                | Self::AnotherAction
                | Self::DocumentedStatementsAndDeny
                | Self::ThroughTheCreatingService
                | Self::ThroughAnotherService
        )
    }

    /// Put this row's authority in force on the key `key_id` for the group's role `role_arn`.
    async fn apply(
        self,
        router: &MultiServiceRouter,
        case: &str,
        key_id: &str,
        role_arn: &str,
    ) {
        match self {
            // A key created without a policy already carries the default one, and a grant row
            // leaves it exactly there: what it adds is the grant below.
            Self::AwsManagedKey | Self::DefaultPolicy => {}
            Self::DocumentedStatements => {
                authorize_role_on_key(router, case, key_id, role_arn).await;
            }
            Self::AnotherAction => {
                put_key_policy(
                    router,
                    case,
                    key_id,
                    &role_policy(role_arn, json!(["kms:Encrypt"]), None, None),
                )
                .await;
            }
            Self::AnotherRole => {
                authorize_role_on_key(router, case, key_id, &another_role_arn()).await;
            }
            Self::DocumentedStatementsAndDeny => {
                let mut policy = service_linked_role_key_policy(role_arn);
                let statements = policy
                    .get_mut("Statement")
                    .and_then(Value::as_array_mut)
                    .expect("the documented policy carries a statement list");
                statements.push(json!({
                    "Sid": "Deny the role",
                    "Effect": "Deny",
                    "Principal": { "AWS": role_arn },
                    "Action": "kms:*",
                    "Resource": "*"
                }));
                put_key_policy(router, case, key_id, &policy).await;
            }
            Self::ThroughTheCreatingService => {
                put_key_policy(
                    router,
                    case,
                    key_id,
                    &role_policy(
                        role_arn,
                        json!(SERVICE_LINKED_ROLE_KEY_ACTIONS),
                        Some("kms:ViaService"),
                        Some(&creating_service()),
                    ),
                )
                .await;
            }
            Self::ThroughAnotherService => {
                put_key_policy(
                    router,
                    case,
                    key_id,
                    &role_policy(
                        role_arn,
                        json!(SERVICE_LINKED_ROLE_KEY_ACTIONS),
                        Some("kms:ViaService"),
                        Some(&format!("sqs.{REGION}.amazonaws.com")),
                    ),
                )
                .await;
            }
            Self::Grant => {
                kms(
                    router,
                    case,
                    "CreateGrant",
                    json!({
                        "KeyId": key_id,
                        "GranteePrincipal": role_arn,
                        "Operations": ["GenerateDataKeyWithoutPlaintext"],
                    }),
                )
                .await;
            }
        }
    }
}

/// A key policy naming the account root, as every key carries, plus ONE statement for
/// `role_arn` over `actions` — scoped by `condition_key` to `condition_value` where the row
/// scopes it.
fn role_policy(
    role_arn: &str,
    actions: Value,
    condition_key: Option<&str>,
    condition_value: Option<&str>,
) -> Value {
    let mut statement = json!({
        "Sid": "Allow use of the key",
        "Effect": "Allow",
        "Principal": { "AWS": role_arn },
        "Action": actions,
        "Resource": "*"
    });
    if let Some((key, value)) = condition_key.zip(condition_value) {
        statement["Condition"] = json!({ "StringEquals": { key: value } });
    }
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Sid": "Enable IAM User Permissions",
                "Effect": "Allow",
                "Principal": {
                    "AWS": format!("arn:aws:iam::{}:root", ::engine::emulator_account_id())
                },
                "Action": "kms:*",
                "Resource": "*"
            },
            statement
        ]
    })
}

/// The regional endpoint name of the service that creates the volume, which is the value of
/// `kms:ViaService` for the data-key request a launch's encrypted volume makes.
///
/// SOURCE: `kms:ViaService` "limits use of a KMS key to requests from particular Amazon Web
/// Services services", and the service that creates an instance's volumes is Amazon EC2, whose
/// regional endpoint carries its `ec2` endpoint prefix.
fn creating_service() -> String {
    format!("ec2.{REGION}.amazonaws.com")
}

/// A role of this account that is NOT the one any group here is bound to.
fn another_role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/not-the-auto-scaling-role",
        ::engine::emulator_account_id()
    )
}

/// The service-linked role the group `group` is BOUND to, read from the group's own record
/// where a client reads it — never written out, so what the policies here authorize is the
/// principal the emulator itself says makes the launch.
async fn group_service_linked_role(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> String {
    let described = described_group(router, case, group).await;
    let role = tag_value(&described, "ServiceLinkedRoleARN");
    assert!(
        !role.is_empty(),
        "case {case:?}: the group must report the service-linked role it is bound to, got \
         {described}"
    );
    role
}

/// The key policy KMS holds for `key_id`, read where a client reads it.
async fn key_policy(router: &MultiServiceRouter, case: &str, key_id: &str) -> String {
    let answered = kms(
        router,
        case,
        "GetKeyPolicy",
        json!({ "KeyId": key_id, "PolicyName": DEFAULT_KEY_POLICY_NAME }),
    )
    .await;
    answered
        .get("Policy")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The instance ids the account's own Amazon EC2 view holds for this group, asked of the
/// service that owns the instances through the tag Auto Scaling marks them with.
async fn ec2_attempts(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let (status, body) = ec2_query(
        router,
        "DescribeInstances",
        &[
            p("Filter.1.Name", format!("tag:{GROUP_NAME_TAG}")),
            p("Filter.1.Value.1", group),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances expected 200, got {status}: {body}"
    );
    let mut ids = tag_values(&body, "instanceId");
    ids.sort();
    ids.dedup();
    ids
}

/// The `(state, state reason code, state reason message)` Amazon EC2 reports for ONE instance,
/// read out of a `DescribeInstances` that named exactly that instance so every member belongs
/// to it.
async fn described_instance(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (String, String, String) {
    let (status, body) =
        ec2_query(router, "DescribeInstances", &[p("InstanceId.1", instance_id)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeInstances({instance_id}) expected 200, got {status}: {body}"
    );
    let state = element_body(&body, "instanceState")
        .map(|state| tag_value(state, "name"))
        .unwrap_or_default();
    let reason = element_body(&body, "stateReason");
    (
        state,
        reason.map(|reason| tag_value(reason, "code")).unwrap_or_default(),
        reason
            .map(|reason| tag_value(reason, "message"))
            .unwrap_or_default(),
    )
}

/// Look again until Amazon EC2 has taken the attempt `instance_id` all the way back, and answer
/// the state reason it reports then.
///
/// The take-back is owed on Amazon EC2's own clock, so this waits for the OUTCOME rather than
/// for any length of time.
async fn withdrawn_attempt(
    router: &MultiServiceRouter,
    case: &str,
    instance_id: &str,
) -> (String, String) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (state, code, message) = described_instance(router, case, instance_id).await;
        if state == TERMINATED {
            return (code, message);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the attempt {instance_id} must be taken back, got {state:?} with \
             {code:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// Look again until the group holds exactly `wanted` instances in service, and answer them.
async fn in_service_instances(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: usize,
) -> Vec<(String, String, String)> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let instances = group_instances(router, case, group).await;
        let in_service: Vec<(String, String, String)> = instances
            .iter()
            .filter(|instance| instance.1 == IN_SERVICE)
            .cloned()
            .collect();
        if in_service.len() == wanted && instances.len() == wanted {
            return in_service;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the group must hold {wanted} instance(s) in service, got \
             {instances:?}"
        );
        tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// What the capacity change does under a row's authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// The launch is made and the instance comes into service.
    Served,
    /// The attempt is taken back and the activity is cancelled.
    Cancelled,
}

/// One row: the authority in force over the key, and what the capacity change does under it.
struct Row {
    case: &'static str,
    group: &'static str,
    template: &'static str,
    authority: Authority,
    expected: Expected,
}

/// A capacity change places an instance exactly when the authorities over its volume's key let
/// the group's own service-linked role ask that key for the volume's data key.
#[tokio::test]
async fn a_launch_is_placed_only_where_the_groups_role_may_use_the_volumes_key() {
    let rows = [
        Row {
            case: "the default key policy, which authorizes nobody but the account root",
            group: "key-auth-default-policy",
            template: "lt-key-auth-default-policy",
            authority: Authority::DefaultPolicy,
            expected: Expected::Cancelled,
        },
        Row {
            case: "the statements AWS documents for the group's service-linked role",
            group: "key-auth-documented",
            template: "lt-key-auth-documented",
            authority: Authority::DocumentedStatements,
            expected: Expected::Served,
        },
        Row {
            case: "a policy naming the role for an action the volume's ask is not among",
            group: "key-auth-another-action",
            template: "lt-key-auth-another-action",
            authority: Authority::AnotherAction,
            expected: Expected::Cancelled,
        },
        Row {
            case: "a policy carrying those statements for another role of the account",
            group: "key-auth-another-role",
            template: "lt-key-auth-another-role",
            authority: Authority::AnotherRole,
            expected: Expected::Cancelled,
        },
        Row {
            case: "those statements beside an explicit deny over the same role",
            group: "key-auth-denied",
            template: "lt-key-auth-denied",
            authority: Authority::DocumentedStatementsAndDeny,
            expected: Expected::Cancelled,
        },
        Row {
            case: "those statements scoped to the service the volume's ask arrives through",
            group: "key-auth-via-creating-service",
            template: "lt-key-auth-via-creating-service",
            authority: Authority::ThroughTheCreatingService,
            expected: Expected::Served,
        },
        Row {
            case: "those statements scoped to another service",
            group: "key-auth-via-another-service",
            template: "lt-key-auth-via-another-service",
            authority: Authority::ThroughAnotherService,
            expected: Expected::Cancelled,
        },
        Row {
            case: "a grant to the role for the operation, with the policy left as created",
            group: "key-auth-grant",
            template: "lt-key-auth-grant",
            authority: Authority::Grant,
            expected: Expected::Served,
        },
        Row {
            case: "the Amazon Web Services managed key of Amazon EBS, whose policy AWS manages",
            group: "key-auth-managed-key",
            template: "lt-key-auth-managed-key",
            authority: Authority::AwsManagedKey,
            expected: Expected::Served,
        },
    ];

    for row in &rows {
        let case = row.case;
        let router = emulator();
        // The key the row's launch-template version encrypts the root volume under: a customer
        // managed key of the account, or the managed key of Amazon EBS for the row that names
        // it.
        let key_id = if row.authority.customer_managed() {
            Some(create_key(&router, case).await.0)
        } else {
            None
        };
        let reference = key_id
            .clone()
            .unwrap_or_else(|| EBS_MANAGED_KEY_ALIAS.to_string());
        let (template_id, version) = create_launch_template(
            &router,
            case,
            row.template,
            &[Volume {
                device: ROOT_DEVICE,
                kms_key_id: Some(&reference),
            }],
        )
        .await;
        create_group(&router, case, row.group, &template_id, &version).await;

        // The principal the authorities are weighed for is the one the GROUP says it makes the
        // launch under, read from its own record.
        let role = group_service_linked_role(&router, case, row.group).await;
        if let Some(key_id) = key_id.as_deref() {
            row.authority.apply(&router, case, key_id, &role).await;
            // The precondition the row rests on, asserted from the emulator's own state rather
            // than assumed: whether the key's policy names the group's role at all.
            let policy = key_policy(&router, case, key_id).await;
            assert_eq!(
                policy.contains(&role),
                row.authority.policy_names_the_role(),
                "case {case:?}: expected the key policy to name {role} \
                 ({}), got {policy}",
                row.authority.policy_names_the_role()
            );
        }

        set_desired_capacity(&router, case, row.group, "1").await;
        let outcome = settled_activity(&router, case, row.group).await;
        match row.expected {
            Expected::Served => {
                assert_eq!(
                    (outcome.status_code.as_str(), outcome.status_message.as_str()),
                    ("Successful", ""),
                    "case {case:?}: expected a successful activity carrying no status message, \
                     got {outcome:?}"
                );
                let in_service = in_service_instances(&router, case, row.group, 1).await;
                assert_eq!(
                    in_service.len(),
                    1,
                    "case {case:?}: the group must hold the one instance it launched, got \
                     {in_service:?}"
                );
            }
            Expected::Cancelled => {
                assert_eq!(
                    (
                        outcome.status_code.as_str(),
                        outcome.status_message.as_str(),
                        outcome.progress.as_str()
                    ),
                    ("Cancelled", CANCELLED_STATUS_MESSAGE, "100"),
                    "case {case:?}: expected a cancelled activity carrying the key's refusal, \
                     got {outcome:?}"
                );
                // No instance of the group: the attempt never reached service.
                let instances = group_instances(&router, case, row.group).await;
                assert!(
                    instances.is_empty(),
                    "case {case:?}: the group must hold no instance when its volume's key \
                     serves its role nothing, got {instances:?}"
                );
                // And the attempt Amazon EC2 made for it is taken back where every instance of
                // the account lives, under the reason it was taken back for.
                let attempts = ec2_attempts(&router, case, row.group).await;
                assert!(
                    !attempts.is_empty(),
                    "case {case:?}: the launch must leave the attempt Amazon EC2 took back"
                );
                for attempt in &attempts {
                    let (code, message) = withdrawn_attempt(&router, case, attempt).await;
                    assert_eq!(
                        (code.as_str(), message.as_str()),
                        (INVALID_KEY_STATE_CODE, INVALID_KEY_STATE_MESSAGE),
                        "case {case:?}: expected the attempt {attempt} to be taken back under \
                         {INVALID_KEY_STATE_CODE}, got {code:?} with {message:?}"
                    );
                }
                // The capacity the caller asked for STANDS, so authorizing the role is all it
                // takes to reach it.
                let described = described_group(&router, case, row.group).await;
                assert_eq!(
                    tag_value(&described, "DesiredCapacity"),
                    "1",
                    "case {case:?}: the group must keep the capacity that was asked for, got \
                     {described}"
                );
            }
        }
    }
}

/// ONE group, over the lifecycle the rule turns on: cancelled while the key's policy authorizes
/// nobody but the account root, and in service once the documented statements name the role the
/// group itself reports — with nothing about the group, the template or the key changing.
#[tokio::test]
async fn authorizing_the_groups_role_lets_the_same_group_reach_its_capacity() {
    let case = "a group whose key authorizes its role only after the launch was cancelled";
    let group = "key-auth-lifecycle";
    let template = "lt-key-auth-lifecycle";
    let router = emulator();

    let (key_id, _) = create_key(&router, case).await;
    let (template_id, version) = create_launch_template(
        &router,
        case,
        template,
        &[Volume {
            device: ROOT_DEVICE,
            kms_key_id: Some(&key_id),
        }],
    )
    .await;
    create_group(&router, case, group, &template_id, &version).await;
    let role = group_service_linked_role(&router, case, group).await;

    // The precondition: the key is one KMS serves, and its policy names no principal matching
    // the group's role.
    let state = kms(&router, case, "DescribeKey", json!({ "KeyId": &key_id })).await;
    assert_eq!(
        state
            .get("KeyMetadata")
            .and_then(|metadata| metadata.get("KeyState"))
            .and_then(Value::as_str),
        Some("Enabled"),
        "case {case:?}: the key must be observably enabled before the launch, got {state}"
    );
    let policy = key_policy(&router, case, &key_id).await;
    assert!(
        !policy.contains(&role),
        "case {case:?}: the key's policy must name no principal matching {role}, got {policy}"
    );

    set_desired_capacity(&router, case, group, "1").await;
    let blocked = settled_activity(&router, case, group).await;
    assert_eq!(
        (blocked.status_code.as_str(), blocked.status_message.as_str()),
        ("Cancelled", CANCELLED_STATUS_MESSAGE),
        "case {case:?}: a launch the role is not authorized for must be cancelled, got \
         {blocked:?}"
    );
    assert!(
        group_instances(&router, case, group).await.is_empty(),
        "case {case:?}: no instance may be in the group while its role may not use the key"
    );
    let attempts = ec2_attempts(&router, case, group).await;
    assert!(
        !attempts.is_empty(),
        "case {case:?}: the launch must leave the attempt Amazon EC2 took back"
    );
    for attempt in &attempts {
        let (code, _) = withdrawn_attempt(&router, case, attempt).await;
        assert_eq!(
            code, INVALID_KEY_STATE_CODE,
            "case {case:?}: expected the attempt {attempt} to be taken back under \
             {INVALID_KEY_STATE_CODE}, got {code:?}"
        );
    }

    // Only the key's policy changes: the documented statements now name the group's own role.
    authorize_role_on_key(&router, case, &key_id, &role).await;
    let authorized = key_policy(&router, case, &key_id).await;
    assert!(
        authorized.contains(&role),
        "case {case:?}: the key's policy must now name {role}, got {authorized}"
    );

    set_desired_capacity(&router, case, group, "1").await;
    let in_service = in_service_instances(&router, case, group, 1).await;
    assert_eq!(
        in_service.len(),
        1,
        "case {case:?}: the same group must reach one instance in service once its role may \
         use the key, got {in_service:?}"
    );
    let served = settled_activity(&router, case, group).await;
    assert_eq!(
        (served.status_code.as_str(), served.status_message.as_str()),
        ("Successful", ""),
        "case {case:?}: the launch made after the role was authorized must be successful, got \
         {served:?}"
    );
    // The instance in service is NOT one of the attempts that were taken back: it is the
    // launch the group made after the key authorized its role.
    for attempt in &attempts {
        assert!(
            in_service.iter().all(|instance| &instance.0 != attempt),
            "case {case:?}: the instance in service must not be the withdrawn attempt \
             {attempt}, got {in_service:?}"
        );
    }
}
