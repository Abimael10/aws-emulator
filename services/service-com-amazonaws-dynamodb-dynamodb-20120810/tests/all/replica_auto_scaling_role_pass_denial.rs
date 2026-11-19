// SPDX-License-Identifier: CC0-1.0

//! WHAT an auto scaling update naming a role of a customer's own is refused with.
//!
//! The rule these tests pin:
//!
//! > a replica's capacity is scaled by Application Auto Scaling, reached under DynamoDB's
//! > OWN replication service-linked role, so an `AutoScalingRoleArn` the request names is a
//! > role that role must pass — and the refusal that pass earns is relayed verbatim: the
//! > message carries the scaling service's own `AccessDeniedException` sentence, naming the
//! > principal that ATTEMPTED the pass (the assumed-role session of the replication
//! > service-linked role, as STS spells a session — never the client's own identity) and the
//! > role from the request, and it ends with the envelope the SDK layer states around a
//! > caught service exception, `(Service: <service>; Status Code: <status>; Error Code:
//! > AccessDeniedException; Request ID: <id>; Proxy: null)`. This surface states no opening
//! > and no exception class of its own, so the message BEGINS at `User: `.
//!
//! The rows span what the rule generalises over: which member of the update carries the
//! role (the group's write capacity, one global secondary index's, one replica's read
//! capacity, one replica's index read capacity), the role ARN named (a plain role, a role
//! under a path, a role whose name is a session-unfriendly length) and the table the
//! scaling was attempted for.
//!
//! Every call is driven over the wire (`POST /` with
//! `X-Amz-Target: DynamoDB_20120810.<OperationName>`) through the same `MultiServiceRouter`
//! the emulator's front doors use.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::emulator::{call_in, emulator, error_code, message, ok_in, REGION};

/// The Region the replica of the global table below lives in, so an update may address a
/// replica other than the one the request reached.
const REPLICA_REGION: &str = "eu-west-1";

/// The global secondary index the table below carries, so an update may address an index.
const INDEX: &str = "ByStatus";

/// The service-linked role DynamoDB replicates — and so scales — a global table through,
/// as the DynamoDB Developer Guide publishes it ("Using service-linked roles for Amazon
/// DynamoDB": `AWSServiceRoleForDynamoDBReplication`). The session attempting the pass is a
/// session of THIS role, not of the caller's identity.
const REPLICATION_ROLE: &str = "AWSServiceRoleForDynamoDBReplication";

/// The longest session name STS mints (`AssumeRole`'s `RoleSessionName`, @length 2..64), so
/// a long table name still names a session of a form STS would report.
const SESSION_NAME_MAX: usize = 64;

/// Wait out one transient span, so whatever the clock owes a table or a replica is due.
async fn settle() {
    tokio::time::sleep(
        service_com_amazonaws_dynamodb_dynamodb_20120810::state::STATUS_TRANSITION
            + std::time::Duration::from_millis(150),
    )
    .await;
}

/// One provisioned table with a global secondary index, replicated into [`REPLICA_REGION`]
/// so its group spans two Regions and every member of the update has somewhere to address.
async fn global_table(router: &MultiServiceRouter, name: &str) {
    ok_in(
        router,
        REGION,
        "CreateTable",
        json!({
            "TableName": name,
            "KeySchema": [{"AttributeName": "Id", "KeyType": "HASH"}],
            "AttributeDefinitions": [
                {"AttributeName": "Id", "AttributeType": "S"},
                {"AttributeName": "Status", "AttributeType": "S"},
            ],
            "BillingMode": "PROVISIONED",
            "ProvisionedThroughput": {"ReadCapacityUnits": 5, "WriteCapacityUnits": 5},
            "GlobalSecondaryIndexes": [{
                "IndexName": INDEX,
                "KeySchema": [{"AttributeName": "Status", "KeyType": "HASH"}],
                "Projection": {"ProjectionType": "ALL"},
                "ProvisionedThroughput": {"ReadCapacityUnits": 5, "WriteCapacityUnits": 5},
            }],
        }),
    )
    .await;
    settle().await;
    ok_in(
        router,
        REGION,
        "UpdateTable",
        json!({
            "TableName": name,
            "ReplicaUpdates": [{"Create": {"RegionName": REPLICA_REGION}}],
        }),
    )
    .await;
    settle().await;
}

/// One `AutoScalingSettingsUpdate` body naming `role`: the capacity window, the policy that
/// steers it, and the role the request asks the scaling to be performed under.
fn settings_update(role: &str) -> Value {
    json!({
        "MinimumUnits": 5,
        "MaximumUnits": 15,
        "AutoScalingRoleArn": role,
        "ScalingPolicyUpdate": {
            "TargetTrackingScalingPolicyConfiguration": {"TargetValue": 70.0},
        },
    })
}

/// Every member of the update that can carry an `AutoScalingRoleArn`, as the request that
/// carries it there: the group's write capacity, one index's write capacity, one replica's
/// read capacity, and one replica's index read capacity.
fn requests(name: &str, role: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "the group's write capacity",
            json!({
                "TableName": name,
                "ProvisionedWriteCapacityAutoScalingUpdate": settings_update(role),
            }),
        ),
        (
            "an index's write capacity",
            json!({
                "TableName": name,
                "GlobalSecondaryIndexUpdates": [{
                    "IndexName": INDEX,
                    "ProvisionedWriteCapacityAutoScalingUpdate": settings_update(role),
                }],
            }),
        ),
        (
            "one replica's read capacity",
            json!({
                "TableName": name,
                "ReplicaUpdates": [{
                    "RegionName": REPLICA_REGION,
                    "ReplicaProvisionedReadCapacityAutoScalingUpdate": settings_update(role),
                }],
            }),
        ),
        (
            "one replica's index read capacity",
            json!({
                "TableName": name,
                "ReplicaUpdates": [{
                    "RegionName": REPLICA_REGION,
                    "ReplicaGlobalSecondaryIndexUpdates": [{
                        "IndexName": INDEX,
                        "ProvisionedReadCapacityAutoScalingUpdate": settings_update(role),
                    }],
                }],
            }),
        ),
    ]
}

/// The role ARNs a request may name that the replication role cannot pass, each a role of
/// the account's own rather than the one the scaling is performed under: a plain role, one
/// under a path, and one whose name alone is longer than a session name may be.
fn unpassable_roles() -> Vec<(&'static str, String)> {
    vec![
        (
            "a plain role",
            ::engine::emulator_arn("iam", "", "role/ddb-scaling"),
        ),
        (
            "a role under a path",
            ::engine::emulator_arn("iam", "", "role/service-roles/team/ddb-scaling"),
        ),
        (
            "a role with a long name",
            ::engine::emulator_arn("iam", "", &format!("role/{}", "r".repeat(70))),
        ),
    ]
}

/// Assert one refusal against the rule, and return the relayed request id it named.
fn assert_relayed_denial(case: &str, status: u16, answered: &Value, role: &str) -> String {
    assert_eq!(
        (status, error_code(answered).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected HTTP 400 ValidationException, got {status} with {answered}"
    );
    let reported = message(answered);
    // The message begins at the relayed sentence: the session that attempted the pass,
    // spelled as STS reports an assumed-role session of the replication role.
    let head = format!(
        "User: {}/",
        ::engine::emulator_arn("sts", "", &format!("assumed-role/{REPLICATION_ROLE}"))
    );
    assert!(
        reported.starts_with(&head),
        "case {case:?}: expected the message to start {head:?}, got {reported:?}"
    );
    // ... and NOT at an exception class this surface added, nor at the client's own
    // identity: the principal that attempts the pass is the service's role, not the caller.
    assert!(
        !reported.contains(":user/"),
        "case {case:?}: expected no IAM user principal in the denial, got {reported:?}"
    );
    let middle = format!(
        " is not authorized to perform: iam:PassRole on resource: {role} because no \
         identity-based policy allows the iam:PassRole action (Service: "
    );
    assert!(
        reported.contains(&middle),
        "case {case:?}: expected the message to state {middle:?}, got {reported:?}"
    );
    // The envelope the SDK layer states around the caught refusal closes the message.
    let tail = "; Proxy: null)";
    assert!(
        reported.ends_with(tail),
        "case {case:?}: expected the message to end {tail:?}, got {reported:?}"
    );
    for stated in [
        "Status Code: 400;",
        "Error Code: AccessDeniedException;",
        "Request ID: ",
    ] {
        assert!(
            reported.contains(stated),
            "case {case:?}: expected the envelope to state {stated:?}, got {reported:?}"
        );
    }
    // The session name is one STS could have minted: inside the published length limit.
    let session = reported
        .strip_prefix(&head)
        .and_then(|rest| rest.split(' ').next())
        .unwrap_or_default();
    assert!(
        (2..=SESSION_NAME_MAX).contains(&session.chars().count()),
        "case {case:?}: expected a session name of 2..={SESSION_NAME_MAX} characters, got \
         {session:?}"
    );
    relayed_request_id(&reported)
}

/// The id the envelope names the relayed call by.
fn relayed_request_id(reported: &str) -> String {
    reported
        .rsplit_once("Request ID: ")
        .and_then(|(_, rest)| rest.split(';').next())
        .unwrap_or_default()
        .to_string()
}

#[tokio::test]
async fn a_role_the_replication_role_cannot_pass_is_refused_with_the_relayed_denial() {
    let router = emulator();
    let name = "Passed.rolesglobal";
    global_table(&router, name).await;

    for (role_case, role) in unpassable_roles() {
        for (member_case, request) in requests(name, &role) {
            let case = format!("{role_case}, named by {member_case}");
            let (status, answered) =
                call_in(&router, REGION, "UpdateTableReplicaAutoScaling", request).await;
            let request_id = assert_relayed_denial(&case, status, &answered, &role);
            assert!(
                ::service_common::query_error_envelope::is_request_id(&request_id),
                "case {case:?}: expected the envelope to name a request id, got {request_id:?}"
            );
        }
    }

    // A refused pass configures nothing: every capacity the refused requests named is still
    // unconfigured, because the settings are what a caller configured and no caller did.
    let described = ok_in(
        &router,
        REGION,
        "DescribeTableReplicaAutoScaling",
        json!({"TableName": name}),
    )
    .await["TableAutoScalingDescription"]
        .clone();
    for replica in described["Replicas"].as_array().cloned().unwrap_or_default() {
        assert_eq!(
            (
                replica["ReplicaProvisionedWriteCapacityAutoScalingSettings"].clone(),
                replica["ReplicaProvisionedReadCapacityAutoScalingSettings"].clone(),
            ),
            (Value::Null, Value::Null),
            "case a refused pass writes nothing: expected no settings on {}, got {replica}",
            replica["RegionName"]
        );
    }
}

#[tokio::test]
async fn each_relayed_denial_names_its_own_call() {
    let router = emulator();
    // The table the scaling is attempted for is what the relaying session is named after,
    // so a name longer than a session name is still reported as a session STS would mint.
    let rows: [(&str, &str); 2] = [
        ("a short table name", "Passed.short"),
        (
            "a table name longer than a session name",
            "Passed.a-table-name-far-longer-than-the-sixty-four-characters-a-session-name-may-be",
        ),
    ];
    let role = ::engine::emulator_arn("iam", "", "role/ddb-scaling");
    let mut seen: Vec<String> = Vec::new();
    for (case, name) in rows {
        global_table(&router, name).await;
        let (status, answered) = call_in(
            &router,
            REGION,
            "UpdateTableReplicaAutoScaling",
            json!({
                "TableName": name,
                "ProvisionedWriteCapacityAutoScalingUpdate": settings_update(&role),
            }),
        )
        .await;
        seen.push(assert_relayed_denial(case, status, &answered, &role));
    }
    // The request id belongs to the relayed CALL, so two refusals never report one id.
    assert_ne!(
        seen[0], seen[1],
        "case \"two relayed calls\": expected each call its own request id, got {:?} twice",
        seen[0]
    );
}
