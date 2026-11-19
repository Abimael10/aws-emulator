// SPDX-License-Identifier: CC0-1.0

//! What a stack's DELETION leaves of a REPLICATED secret it created, end to end over the
//! wire.
//!
//! RULE: **a stack's deletion removes the secret it created from every Region that secret
//! reached** — the Region the stack deployed it in and each Region it is replicated to — so
//! the teardown reaches `DELETE_COMPLETE` and no Region answers for the secret afterwards.
//! AWS Secrets Manager will not delete a primary that still has replicas ("You can't delete
//! a primary secret that is replicated to other Regions. You must first delete the replicas
//! using RemoveRegionsFromReplication, and then delete the primary secret", `DeleteSecret`),
//! so the provider's removal takes the replicas away first; a removal that sent the delete
//! straight through would be refused by that service and fail the resource, leaving the
//! stack `DELETE_FAILED` with the secret and its replicas still held under a name no
//! redeployment of the template could reuse.
//!
//! The rows differ along what the rule generalises over — how many Regions the secret
//! reached: none at all (the unreplicated control, which the same removal still has to
//! take away), one, and two — and along whether a replica's declaration names an
//! encryption key of its own, which is the other member `ReplicaRegions` publishes.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{call, create_stack, describe_stack, member, output_value, router, xml_text, REGION};
use crate::stack_owned_secret_teardown::{
    error_code, poll_to_terminal, secretsmanager_in, text, SECRET_NOT_FOUND,
};

/// The stack every row is deployed in: one teardown is what the rule is about, so the rows
/// are resources of ONE stack rather than a stack each.
const STACK: &str = "replicated-secret-teardown";

/// The status a replica reports once its creation has finished, from `StatusType`
/// (`smithy_query(query="shape",shape="ReplicationStatusType")`: "The status can be
/// InProgress, Failed, or InSync").
const IN_SYNC: &str = "InSync";

/// How many reads a poll of a replica's status takes before it gives up — generous, because
/// what it waits for is a CONDITION (the replica reaching a settled status) and not a span.
const REPLICA_POLL_ATTEMPTS: usize = 40;

/// One declared secret of the stack.
#[derive(Debug, Clone, Copy)]
struct Row {
    case: &'static str,
    /// The logical id the template declares it under, which also names its output.
    logical_id: &'static str,
    /// The `Name` the declaration states, which is the name the secret — and each replica of
    /// it — is created under.
    name: &'static str,
    /// The value the declaration states, which every Region that holds the secret serves.
    value: &'static str,
    /// The Regions the declaration replicates the secret to, and the `KmsKeyId` each entry
    /// states (or `None` for one that names no key of its own).
    replicas: &'static [(&'static str, Option<&'static str>)],
}

const ROWS: &[Row] = &[
    Row {
        // The control: a secret that reached only the Region it was deployed in. The same
        // removal has to take it away, so no replica is needed for the rule to hold.
        case: "no replica at all",
        logical_id: "Unreplicated",
        name: "replicated-teardown-unreplicated",
        value: "an-unreplicated-example-value",
        replicas: &[],
    },
    Row {
        // The recorded shape: one replica Region, naming no key of its own.
        case: "one replica Region",
        logical_id: "OneReplica",
        name: "replicated-teardown-one-replica",
        value: "{\"username\":\"probeadmin\",\"password\":\"a-replicated-example\"}",
        replicas: &[("us-west-2", None)],
    },
    Row {
        // TWO Regions, one of them naming an encryption key: the removal has to take every
        // replica away, not the first one it read.
        case: "two replica Regions, one with a declared key",
        logical_id: "TwoReplicas",
        name: "replicated-teardown-two-replicas",
        value: "a-value-held-in-three-regions",
        replicas: &[
            ("us-west-2", Some("alias/aws/secretsmanager")),
            ("eu-west-1", None),
        ],
    },
];

/// The template declaring every row, and publishing each secret's ARN as an output — "Ref
/// returns the ARN of the secret", which is the handle every Secrets Manager call takes.
fn template() -> String {
    let mut resources = serde_json::Map::new();
    let mut outputs = serde_json::Map::new();
    for row in ROWS {
        let mut properties = serde_json::Map::new();
        properties.insert("Name".to_string(), json!(row.name));
        properties.insert("SecretString".to_string(), json!(row.value));
        if !row.replicas.is_empty() {
            let declared: Vec<Value> = row
                .replicas
                .iter()
                .map(|(region, key)| match key {
                    Some(key) => json!({"Region": region, "KmsKeyId": key}),
                    None => json!({"Region": region}),
                })
                .collect();
            properties.insert("ReplicaRegions".to_string(), Value::Array(declared));
        }
        resources.insert(
            row.logical_id.to_string(),
            json!({
                "Type": "AWS::SecretsManager::Secret",
                "Properties": Value::Object(properties),
            }),
        );
        outputs.insert(
            arn_output(row),
            json!({"Value": {"Ref": row.logical_id}}),
        );
    }
    json!({"Resources": resources, "Outputs": outputs}).to_string()
}

/// The output key a row's ARN is published under.
fn arn_output(row: &Row) -> String {
    format!("{}Arn", row.logical_id)
}

/// The `ReplicationStatus` entries a `DescribeSecret` answer reports, keyed by Region.
fn replica_statuses(parsed: &Value) -> Vec<(String, String)> {
    parsed
        .get("ReplicationStatus")
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry
                            .get("Region")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        entry
                            .get("Status")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Read the primary until every replica of it reports a status that is no longer being
/// created, and answer that read's body. Waits for the CONDITION: each attempt looks again.
async fn poll_until_replicas_settle(
    router: &MultiServiceRouter,
    case: &str,
    secret_id: &str,
) -> Value {
    let mut described = Value::Null;
    for _ in 0..REPLICA_POLL_ATTEMPTS {
        let (status_code, body) =
            secretsmanager_in(router, REGION, "DescribeSecret", &json!({"SecretId": secret_id}))
                .await;
        assert_eq!(
            status_code, 200,
            "case {case:?}: DescribeSecret must answer 200 for the secret the stack created, \
             got {status_code} {body}"
        );
        described = body;
        if replica_statuses(&described)
            .iter()
            .all(|(_, status)| status != "InProgress")
        {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    described
}

#[tokio::test]
async fn a_deleted_stack_removes_its_secret_from_every_region_it_reached() {
    let router = router();
    let (status_code, body) = create_stack(&router, STACK, &template(), &[]).await;
    assert_eq!(
        status_code, 200,
        "CreateStack must answer 200, got {status_code} {body}"
    );
    let described = poll_to_terminal(&router, "the stack", STACK).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "expected the create to complete, got {described}"
    );

    // BEFORE the deletion: every Region the declaration reached holds the secret and serves
    // the declared value, and the primary reports each replica as created.
    let mut arns = Vec::new();
    for row in ROWS {
        let case = row.case;
        let arn = output_value(&described, &arn_output(row))
            .unwrap_or_else(|| panic!("case {case:?}: the stack published no ARN in {described}"));
        let primary = poll_until_replicas_settle(&router, case, &arn).await;
        assert_eq!(
            text(case, &primary, "/Name"),
            row.name,
            "case {case:?}: expected the secret under the declared name, got {primary}"
        );
        let mut reported = replica_statuses(&primary);
        reported.sort();
        let mut expected: Vec<(String, String)> = row
            .replicas
            .iter()
            .map(|(region, _)| ((*region).to_string(), IN_SYNC.to_string()))
            .collect();
        expected.sort();
        assert_eq!(
            reported, expected,
            "case {case:?}: expected the declared Regions reported {IN_SYNC}, got {primary}"
        );

        for (region, _) in row.replicas {
            let (status_code, served) = secretsmanager_in(
                &router,
                region,
                "GetSecretValue",
                &json!({"SecretId": row.name}),
            )
            .await;
            assert_eq!(
                status_code, 200,
                "case {case:?}: expected {region} to serve the replicated secret, got \
                 {status_code} {served}"
            );
            assert_eq!(
                text(case, &served, "/SecretString"),
                row.value,
                "case {case:?}: expected {region} to serve the declared value, got {served}"
            );
        }
        arns.push(arn);
    }

    // The teardown itself: a stack holding a replicated secret is deletable.
    let (status_code, body) = call(&router, "DeleteStack", &[member("StackName", STACK)]).await;
    assert_eq!(
        status_code, 200,
        "DeleteStack must answer 200, got {status_code} {body}"
    );
    let (status_code, described) = describe_stack(&router, STACK).await;
    assert_eq!(
        status_code, 200,
        "DescribeStacks must answer 200 while the teardown runs, got {status_code} {described}"
    );
    let stack_id = xml_text(&described, "StackId").unwrap_or_default();
    let described = poll_to_terminal(&router, "the teardown", &stack_id).await;
    assert_eq!(
        xml_text(&described, "StackStatus").as_deref(),
        Some("DELETE_COMPLETE"),
        "expected the teardown of a stack holding a replicated secret to reach \
         DELETE_COMPLETE, got {described}"
    );

    // AFTER it: no Region the secret reached answers for it any more, whether it is
    // addressed by the ARN the stack published or by the name every Region held it under.
    for (row, arn) in ROWS.iter().zip(&arns) {
        let case = row.case;
        for (region, addressed) in std::iter::once((REGION, arn.as_str()))
            .chain(std::iter::once((REGION, row.name)))
            .chain(row.replicas.iter().map(|(region, _)| (*region, row.name)))
        {
            for operation in ["DescribeSecret", "GetSecretValue"] {
                let (status_code, refused) =
                    secretsmanager_in(&router, region, operation, &json!({"SecretId": addressed}))
                        .await;
                assert_eq!(
                    (status_code, error_code(case, &refused).as_str()),
                    (400, SECRET_NOT_FOUND),
                    "case {case:?}: expected {operation}({addressed}) in {region} to be refused \
                     with {SECRET_NOT_FOUND} once the stack that owned the secret is deleted, \
                     got {status_code} {refused}"
                );
            }
        }
    }
}
