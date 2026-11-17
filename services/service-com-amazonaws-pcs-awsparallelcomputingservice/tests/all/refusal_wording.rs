// SPDX-License-Identifier: CC0-1.0

//! How PCS WORDS a refusal, end to end over the awsJson1_0 wire (`POST /` with
//! `X-Amz-Target: AWSParallelComputingService.<Operation>`) through the same router the
//! emulator serves from.
//!
//! Three rules under test:
//!
//! 1. a value that violates a member constraint is refused with `ValidationException`
//!    whose message is built from the REQUEST and the MODEL in the standard
//!    constraint-violation framing — how many violations were detected, then
//!    `Value at '<member>' failed to satisfy constraint: <constraint>` per violation —
//!    carrying reason `fieldValidationFailed` and one `fieldList` entry per violation, and
//!    persisting nothing. The rows span the constraint CLASSES (a `@pattern`, a length
//!    ceiling), the three resource-name members that carry the same pattern, and the forms
//!    a name can miss that pattern in.
//! 2. a resource a request named by IDENTIFIER and the service cannot find is
//!    `ResourceNotFoundException` carrying the service's fixed sentence for that KIND
//!    of resource, the identifier exactly as supplied in `resourceId`, and the kind of
//!    IDENTIFIER that did not resolve in `resourceType`. The rows span the three kinds, the
//!    operations that resolve them, and the two identifier forms the `*Identifier` patterns
//!    allow. A call naming no CLUSTER is refused about the caller instead, which is
//!    `crate::cluster_scoped_refusals`.
//! 3. a resource a request addressed by ARN and the service cannot find is
//!    `ResourceNotFoundException` carrying the service's fixed sentence for the
//!    ARN-addressed path and the ARN the request supplied, in a `resourceArn` member. The
//!    rows span the three ARN-addressed operations and the ARN forms the member's
//!    `@pattern` admits — partition, region, account, and the resource part naming each
//!    kind of resource. An identifier-addressed refusal carries no such member: the two
//!    paths are not collapsed into one.
//! 4. `RegisterComputeNodeGroupInstance` is how a compute node Amazon EC2 instance PCS
//!    launched collects its Slurm bootstrap material — "this API action isn't intended for
//!    you to use" (its own documentation) — so a caller that cannot be attributed to such
//!    an instance is denied `AccessDeniedException` and handed none of that material.
//!    The rows span the cluster-identifier forms, whether the cluster exists at all, and
//!    the length bounds `bootstrapId` admits.
//!
//! Every refusal of a client carries one status, the service's [`CLIENT_ERROR_STATUS`],
//! and names its class in the body.
//!
//! Each test builds its own engine, so the state one test writes is invisible to the
//! others.

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use service_com_amazonaws_pcs_awsparallelcomputingservice::state::MAX_STORED_LIST_ENTRIES;
use smithy_server_runtime::MultiServiceRouter;

use super::cluster_lifecycle::{call, emulator, wait_past_the_owed_transition};

/// The constraint clause PCS states for a resource name outside its member's `@pattern`.
///
/// `ClusterName`, `QueueName` and `ComputeNodeGroupName` all carry the same expression
/// (`smithy_query(query="shape", shape="ClusterName")`: `^(?!pcs_)^[A-Za-z][A-Za-z0-9-]+$`),
/// quoted here as the service publishes it: the `^`/`$` pair the model writes around a
/// full-match expression is that wrapping and is not part of the expression a client
/// re-validates against.
const RESOURCE_NAME_CONSTRAINT: &str =
    "Member must satisfy regular expression pattern: (?!pcs_)^[A-Za-z][A-Za-z0-9-]+";

/// The region the harness's credentials sign for (`super::cluster_lifecycle`), which the
/// ARN rows below carry unless a row varies it.
const REGION: &str = "us-east-1";

/// The sentence PCS states for an ARN that names no resource it holds — the ARN-addressed
/// lookup the tagging operations take, as against the identifier-addressed lookups, whose
/// sentence names the kind of resource.
const ARN_ADDRESSED_NOT_FOUND: &str = "Resource cannot be found.";

/// The awsJson1_0 error code, without the namespace the envelope may prefix.
fn error_code(payload: &Value) -> String {
    payload["__type"]
        .as_str()
        .unwrap_or_default()
        .rsplit(['#', ':'])
        .next()
        .unwrap_or_default()
        .to_string()
}

/// A `CreateCluster` request carrying every `@required` member, under `name`.
fn create_cluster(name: &str) -> Value {
    json!({
        "clusterName": name,
        "scheduler": {"type": "SLURM", "version": "24.11"},
        "size": "SMALL",
        "networking": {"subnetIds": ["subnet-0123456789abcdef0"]},
    })
}

/// A `CreateComputeNodeGroup` request carrying every `@required` member.
fn create_compute_node_group(cluster: &str, name: &str) -> Value {
    json!({
        "clusterIdentifier": cluster,
        "computeNodeGroupName": name,
        "subnetIds": ["subnet-0123456789abcdef0"],
        "customLaunchTemplate": {"id": "lt-0123456789abcdef0", "version": "1"},
        "iamInstanceProfileArn": format!(
            "arn:aws:iam::{}:instance-profile/AWSPCS-demo",
            engine::EMULATOR_ACCOUNT_ID
        ),
        "scalingConfiguration": {"minInstanceCount": 0, "maxInstanceCount": 4},
        "instanceConfigs": [{"instanceType": "c6i.large"}],
    })
}

/// Create a cluster under `name`, wait until the clock has made it ACTIVE, and answer the
/// id PCS minted for it.
async fn active_cluster(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(router, "CreateCluster", &create_cluster(name)).await;
    assert_eq!(status, 200, "case create {name}: got {created}");
    let id = created["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    wait_past_the_owed_transition().await;
    id
}

/// The clusters `ListClusters` reports, by name.
async fn cluster_names(router: &MultiServiceRouter) -> Vec<String> {
    let (status, listed) = call(router, "ListClusters", &json!({"maxResults": 100})).await;
    assert_eq!(status, 200, "ListClusters must answer 200, got {listed}");
    listed["clusters"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|row| row["name"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the constraint-violation table: the call, the member whose value it refuses,
/// and the constraint that value failed.
struct Violation {
    case: &'static str,
    operation: &'static str,
    request: Value,
    member: &'static str,
    constraint: String,
}

#[tokio::test]
async fn a_member_constraint_violation_is_stated_in_the_standard_framing() {
    let router = emulator();
    let cluster = active_cluster(&router, "wording-cluster").await;
    // A list longer than the ceiling a stored record admits: the same framing over the
    // other constraint class.
    let oversized: Vec<String> = (0..=MAX_STORED_LIST_ENTRIES)
        .map(|index| format!("subnet-{index:017x}"))
        .collect();
    let length_constraint =
        format!("Member must have length less than or equal to {MAX_STORED_LIST_ENTRIES}");

    let rows = vec![
        Violation {
            case: "a cluster name carrying the reserved generated-id prefix",
            operation: "CreateCluster",
            request: create_cluster("pcs_reserved-name"),
            member: "clusterName",
            constraint: RESOURCE_NAME_CONSTRAINT.to_string(),
        },
        Violation {
            case: "a cluster name starting with a digit",
            operation: "CreateCluster",
            request: create_cluster("9-lives-cluster"),
            member: "clusterName",
            constraint: RESOURCE_NAME_CONSTRAINT.to_string(),
        },
        Violation {
            case: "a cluster name carrying an underscore",
            operation: "CreateCluster",
            request: create_cluster("has_underscore"),
            member: "clusterName",
            constraint: RESOURCE_NAME_CONSTRAINT.to_string(),
        },
        Violation {
            case: "a cluster name starting with a hyphen",
            operation: "CreateCluster",
            request: create_cluster("-leading-hyphen"),
            member: "clusterName",
            constraint: RESOURCE_NAME_CONSTRAINT.to_string(),
        },
        Violation {
            case: "a cluster name carrying a space",
            operation: "CreateCluster",
            request: create_cluster("two words"),
            member: "clusterName",
            constraint: RESOURCE_NAME_CONSTRAINT.to_string(),
        },
        Violation {
            case: "a queue name carrying the reserved prefix",
            operation: "CreateQueue",
            request: json!({"clusterIdentifier": cluster.clone(), "queueName": "pcs_work"}),
            member: "queueName",
            constraint: RESOURCE_NAME_CONSTRAINT.to_string(),
        },
        Violation {
            case: "a compute node group name carrying the reserved prefix",
            operation: "CreateComputeNodeGroup",
            request: create_compute_node_group(&cluster, "pcs_nodes"),
            member: "computeNodeGroupName",
            constraint: RESOURCE_NAME_CONSTRAINT.to_string(),
        },
        Violation {
            case: "a compute node group name carrying an underscore",
            operation: "CreateComputeNodeGroup",
            request: create_compute_node_group(&cluster, "node_group"),
            member: "computeNodeGroupName",
            constraint: RESOURCE_NAME_CONSTRAINT.to_string(),
        },
        Violation {
            case: "a subnet list past the ceiling a stored cluster admits",
            operation: "CreateCluster",
            request: json!({
                "clusterName": "oversized-cluster",
                "scheduler": {"type": "SLURM", "version": "24.11"},
                "size": "SMALL",
                "networking": {"subnetIds": oversized},
            }),
            member: "networking.subnetIds",
            constraint: length_constraint.clone(),
        },
        Violation {
            case: "a security group list past the same ceiling",
            operation: "CreateCluster",
            request: json!({
                "clusterName": "oversized-groups",
                "scheduler": {"type": "SLURM", "version": "24.11"},
                "size": "SMALL",
                "networking": {
                    "subnetIds": ["subnet-0123456789abcdef0"],
                    "securityGroupIds": (0..=MAX_STORED_LIST_ENTRIES)
                        .map(|index| format!("sg-{index:017x}"))
                        .collect::<Vec<String>>(),
                },
            }),
            member: "networking.securityGroupIds",
            constraint: length_constraint.clone(),
        },
    ];

    for row in rows {
        let Violation {
            case,
            operation,
            request,
            member,
            constraint,
        } = row;
        let (status, refused) = call(&router, operation, &request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (CLIENT_ERROR_STATUS, "ValidationException"),
            "case {case:?}: expected {operation} to answer the client-error status with \
             ValidationException, got {status} with {refused}"
        );
        let expected = format!(
            "1 validation error(s) detected: Value at '{member}' failed to satisfy constraint: \
             {constraint}"
        );
        assert_eq!(
            refused["message"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected {expected:?}, got {refused}"
        );
        // The same detail, structurally, for a client that does not parse the sentence.
        assert_eq!(
            refused["reason"].as_str(),
            Some("fieldValidationFailed"),
            "case {case:?}: expected the field-validation reason, got {refused}"
        );
        assert_eq!(
            refused["fieldList"],
            json!([{"name": member, "message": constraint}]),
            "case {case:?}: expected one fieldList entry naming {member:?}, got {refused}"
        );
    }

    // Not one of the refused creates left a cluster behind.
    let names = cluster_names(&router).await;
    assert_eq!(
        names,
        vec!["wording-cluster".to_string()],
        "expected only the cluster the setup created, got {names:?}"
    );
}

#[tokio::test]
async fn a_not_found_names_the_identifier_it_could_not_resolve_and_its_kind() {
    let router = emulator();
    let cluster = active_cluster(&router, "lookup-cluster").await;

    // Each row: the call, the identifier that resolves to nothing, the kind of resource
    // the sentence names, and the kind of identifier `resourceType` reports. Every row
    // either addresses the cluster itself or names a cluster that EXISTS: a child operation
    // naming a cluster that does not is refused about the caller instead
    // (`crate::cluster_scoped_refusals`).
    let rows: Vec<(&str, Value, &str, &str, &str)> = vec![
        (
            "GetCluster",
            json!({"clusterIdentifier": "pcs_00000000000000000000"}),
            "pcs_00000000000000000000",
            "cluster",
            "Cluster ID",
        ),
        (
            "GetCluster",
            json!({"clusterIdentifier": "no-such-cluster"}),
            "no-such-cluster",
            "cluster",
            "Cluster ID",
        ),
        (
            "DeleteCluster",
            json!({"clusterIdentifier": "AnotherMissingOne"}),
            "AnotherMissingOne",
            "cluster",
            "Cluster ID",
        ),
        (
            "GetQueue",
            json!({
                "clusterIdentifier": cluster.clone(),
                "queueIdentifier": "no-such-queue",
            }),
            "no-such-queue",
            "queue",
            "Queue ID",
        ),
        (
            "DeleteQueue",
            json!({
                "clusterIdentifier": "lookup-cluster",
                "queueIdentifier": "pcs_33333333333333333333",
            }),
            "pcs_33333333333333333333",
            "queue",
            "Queue ID",
        ),
        (
            "GetComputeNodeGroup",
            json!({
                "clusterIdentifier": cluster.clone(),
                "computeNodeGroupIdentifier": "no-such-group",
            }),
            "no-such-group",
            "compute node group",
            "Compute Node Group ID",
        ),
        (
            "DeleteComputeNodeGroup",
            json!({
                "clusterIdentifier": "lookup-cluster",
                "computeNodeGroupIdentifier": "pcs_44444444444444444444",
            }),
            "pcs_44444444444444444444",
            "compute node group",
            "Compute Node Group ID",
        ),
    ];

    for (operation, request, identifier, noun, resource_type) in rows {
        let case = format!("{operation} on {identifier}");
        let (status, refused) = call(&router, operation, &request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected the client-error status with ResourceNotFoundException, \
             got {status} with {refused}"
        );
        // The sentence is fixed per kind of resource: it names the kind and echoes no
        // identifier, whatever form the caller supplied.
        assert_eq!(
            refused["message"].as_str(),
            Some(format!("AWS PCS couldn't find the {noun} you specified.").as_str()),
            "case {case}: got {refused}"
        );
        assert!(
            !refused["message"]
                .as_str()
                .unwrap_or_default()
                .contains(identifier),
            "case {case}: the sentence must not echo the identifier, got {refused}"
        );
        assert_eq!(
            (
                refused["resourceId"].as_str(),
                refused["resourceType"].as_str()
            ),
            (Some(identifier), Some(resource_type)),
            "case {case}: expected the identifier and its kind in the modelled members, got \
             {refused}"
        );
    }
}

#[tokio::test]
async fn an_arn_addressed_not_found_reports_the_arn_the_request_carried() {
    let router = emulator();

    // Each row is an ARN that names no resource this emulator holds, well formed for the
    // member: `resourceArn` targets `Arn`, `@length(1..1011)` plus
    // `@pattern(^arn:aws.*:pcs:.*:[0-9]{12}:.*/[a-z0-9_\-]+$)`
    // (`smithy_query(query="operation", operation="TagResource")`). The rows differ in the
    // parts that expression leaves free — the partition, the region, the account, and the
    // resource part naming each kind of resource PCS holds and one it does not.
    let account = engine::EMULATOR_ACCOUNT_ID;
    let absent: Vec<(&str, String)> = vec![
        (
            "a cluster, by its generated id",
            format!("arn:aws:pcs:{REGION}:{account}:cluster/pcs_0123456789abcdef"),
        ),
        (
            "a queue nested under a cluster",
            format!(
                "arn:aws:pcs:{REGION}:{account}:cluster/pcs_0123456789abcdef/queue/pcs_1111111111111111"
            ),
        ),
        (
            "a compute node group nested under a cluster",
            format!(
                "arn:aws:pcs:{REGION}:{account}:cluster/pcs_0123456789abcdef/computenodegroup/pcs_2222222222222222"
            ),
        ),
        (
            "another partition and another region",
            format!("arn:aws-cn:pcs:cn-north-1:{account}:cluster/pcs_3333333333333333"),
        ),
        (
            "another account",
            format!("arn:aws:pcs:{REGION}:000000000000:cluster/pcs_4444444444444444"),
        ),
        (
            "a resource part naming a kind PCS does not define",
            format!("arn:aws:pcs:{REGION}:{account}:widget/pcs_5555555555555555"),
        ),
        (
            "a resource name rather than a generated id",
            format!("arn:aws:pcs:{REGION}:{account}:cluster/never-created"),
        ),
    ];

    for (case, arn) in &absent {
        for (operation, request) in [
            (
                "TagResource",
                json!({"resourceArn": arn, "tags": {"env": "prod"}}),
            ),
            ("ListTagsForResource", json!({"resourceArn": arn})),
            (
                "UntagResource",
                json!({"resourceArn": arn, "tagKeys": ["env"]}),
            ),
        ] {
            let (status, refused) = call(&router, operation, &request).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
                "case {operation} on {case:?}: expected the client-error status with \
                 ResourceNotFoundException, got {status} with {refused}"
            );
            // The sentence of the ARN-addressed path is fixed: it names neither the kind of
            // resource nor the ARN, whatever the ARN was.
            assert_eq!(
                refused["message"].as_str(),
                Some(ARN_ADDRESSED_NOT_FOUND),
                "case {operation} on {case:?}: expected {ARN_ADDRESSED_NOT_FOUND:?}, got \
                 {refused}"
            );
            // The ARN is the one value of the request such a refusal reports, echoed
            // exactly as supplied.
            assert_eq!(
                refused["resourceArn"].as_str(),
                Some(arn.as_str()),
                "case {operation} on {case:?}: expected the request's own ARN, got {refused}"
            );
            // The ARN-addressed path names neither the resource nor its kind: the two
            // modelled members are `@required`, so they travel empty rather than absent.
            assert_eq!(
                (
                    refused["resourceId"].as_str(),
                    refused["resourceType"].as_str()
                ),
                (Some(""), Some("")),
                "case {operation} on {case:?}: expected neither modelled member to be \
                 populated, got {refused}"
            );
        }
    }

    // The identifier-addressed path is untouched: it names the kind of resource, reports
    // the identifier in the modelled members, and carries no ARN — the caller supplied
    // none.
    let (status, refused) = call(
        &router,
        "GetCluster",
        &json!({"clusterIdentifier": "no-such-cluster"}),
    )
    .await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
        "case an identifier-addressed lookup: got {status} with {refused}"
    );
    assert_eq!(
        refused["message"].as_str(),
        Some("AWS PCS couldn't find the cluster you specified."),
        "case an identifier-addressed lookup: got {refused}"
    );
    assert!(
        refused.get("resourceArn").is_none(),
        "case an identifier-addressed lookup: expected no ARN member, got {refused}"
    );
}

#[tokio::test]
async fn registering_a_compute_node_group_instance_denies_a_caller_that_is_not_one() {
    let router = emulator();
    let cluster = active_cluster(&router, "bootstrap-cluster").await;

    // Each row: the cluster the call names and the bootstrap id it presents. They span the
    // two `ClusterIdentifier` forms, a cluster that exists and one that does not, and both
    // ends of `BootstrapId`'s modelled `@length(1..1000)`.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "a cluster that does not exist, with a bogus bootstrap id",
            "pcs_44444444444444444444".to_string(),
            "bogus-bootstrap-id".to_string(),
        ),
        (
            "a cluster name that does not exist",
            "no-such-cluster".to_string(),
            "another-bootstrap-id".to_string(),
        ),
        (
            "a cluster that DOES exist, named by its id",
            cluster.clone(),
            "bogus-bootstrap-id".to_string(),
        ),
        (
            "the same cluster named by its name",
            "bootstrap-cluster".to_string(),
            "bogus-bootstrap-id".to_string(),
        ),
        (
            "the shortest bootstrap id the model admits",
            cluster.clone(),
            "x".to_string(),
        ),
        (
            "the longest bootstrap id the model admits",
            cluster.clone(),
            "b".repeat(1000),
        ),
    ];

    for (case, cluster_identifier, bootstrap_id) in rows {
        let (status, refused) = call(
            &router,
            "RegisterComputeNodeGroupInstance",
            &json!({
                "clusterIdentifier": cluster_identifier,
                "bootstrapId": bootstrap_id,
            }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (CLIENT_ERROR_STATUS, "AccessDeniedException"),
            "case {case:?}: expected the client-error status with AccessDeniedException, got \
             {status} with {refused}"
        );
        // A refusal of the CALLER carries its sentence in the `Message` member.
        assert_eq!(
            refused["Message"].as_str(),
            Some("This API must be called from an EC2 instance."),
            "case {case:?}: got {refused}"
        );
        // None of the bootstrap material the response models is handed to such a caller.
        for member in [
            "nodeID",
            "sharedSecret",
            "endpoints",
            "clusterName",
            "computeNodeGroupId",
            "schedulerVersion",
        ] {
            assert!(
                refused.get(member).is_none(),
                "case {case:?}: a denied caller must be handed no {member}, got {refused}"
            );
        }
    }
}
