// SPDX-License-Identifier: CC0-1.0

//! How PCS refuses a CLIENT: with one HTTP status whatever the class, and — for a request
//! about a resource inside a cluster the caller cannot be shown to reach — as a permissions
//! failure rather than as a not-found.
//!
//! Driven over the awsJson1_0 wire (`POST /` with
//! `X-Amz-Target: AWSParallelComputingService.<Operation>`) through the same router the
//! emulator serves from, on the harness [`crate::cluster_lifecycle`] already builds.
//!
//! Two rules under test:
//!
//! 1. every client-error refusal carries ONE status, the service's
//!    [`CLIENT_ERROR_STATUS`], whatever `@httpError` the error shape declares — the class a
//!    client branches on travels in the body's `__type`. A server-side failure keeps its own
//!    status, which the scaffold channel's 501 shows.
//! 2. the cluster is the authorization boundary of every resource nested in one, so a
//!    cluster-scoped operation whose `clusterIdentifier` resolves to no cluster of this
//!    account is refused `AccessDeniedException`, stating the IAM sentence (the caller, the
//!    action `pcs:<Operation>` and the identifier the request named) in the `Message`
//!    member. An operation whose OWN resource is the cluster reports
//!    `ResourceNotFoundException` instead, and so does a child lookup whose cluster DID
//!    resolve.
//!
//! 3. the ORDER in which a cluster-scoped operation decides the two questions: the cluster
//!    it is scoped to first, the child it names only once that cluster has resolved — so
//!    what the call is refused about follows the `clusterIdentifier` alone and never
//!    whether some cluster holds a child of the name it gave.
//!
//! The rows span the class the rules generalise over: for rule 1 one row per error class
//! the model declares for these operations, each with a different `@httpError`; for rule 2
//! every cluster-scoped operation — read, list, create and delete, of both child kinds — in
//! each of the two forms `ClusterIdentifier` admits; for rule 3 both cluster-scoped deletes
//! against every form of cluster identifier at each published bound of its length, each
//! naming children that exist and children nothing holds.

/// What the generated-ID form of a `clusterIdentifier` that resolves to nothing earns, over
/// the widths that form admits and across the state a refused child create must not leave.
///
/// Declared here rather than in `tests/all.rs`, which declares the suites directly under
/// `tests/all/`: a file deeper than that is declared by its own parent module, which this
/// suite is. The `#[path]` is relative to this file's own directory, and is needed because
/// `tests/all.rs` pulled this file in with a `#[path]` of its own.
#[path = "cluster_scoped_refusals/absent_generated_cluster_id.rs"]
mod absent_generated_cluster_id;

use serde_json::{json, Value};
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use service_com_amazonaws_pcs_awsparallelcomputingservice::state::MAX_TAGS_PER_RESOURCE;

use super::cluster_lifecycle::{call, emulator};

/// The fixed tail of the sentence IAM states a refusal it cannot decide the question on
/// with: the resource may not exist, a policy may deny the action, or no policy may grant
/// it — and the refusal does not say which.
const UNDECIDABLE_REASON: &str = "as either the resource does not exist, some policy \
                                  explicitly denies access, or no policy grants access";

/// The principal every call of this suite speaks for: the IAM user the account's own
/// published credential belongs to, which is what `sts:GetCallerIdentity` answers for it —
/// the same source the refusal reads (one statement of it for the tree).
fn caller() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

/// The whole sentence a cluster-scoped refusal of `operation` naming `identifier` states.
fn not_authorized(operation: &str, identifier: &str) -> String {
    format!(
        "User: {} is not authorized to perform: pcs:{operation} on resource: {identifier} \
         {UNDECIDABLE_REASON}",
        caller()
    )
}

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

/// Every cluster-scoped operation, as `(operation, the request naming `cluster`)`: the read,
/// list, create and delete of both kinds of resource a cluster holds.
fn cluster_scoped_calls(cluster: &str) -> Vec<(&'static str, Value)> {
    vec![
        (
            "GetQueue",
            json!({"clusterIdentifier": cluster, "queueIdentifier": "work"}),
        ),
        ("ListQueues", json!({"clusterIdentifier": cluster})),
        (
            "CreateQueue",
            json!({"clusterIdentifier": cluster, "queueName": "work"}),
        ),
        (
            "DeleteQueue",
            json!({"clusterIdentifier": cluster, "queueIdentifier": "work"}),
        ),
        (
            "GetComputeNodeGroup",
            json!({
                "clusterIdentifier": cluster,
                "computeNodeGroupIdentifier": "nodes",
            }),
        ),
        (
            "ListComputeNodeGroups",
            json!({"clusterIdentifier": cluster}),
        ),
        (
            "CreateComputeNodeGroup",
            create_compute_node_group(cluster, "nodes"),
        ),
        (
            "DeleteComputeNodeGroup",
            json!({
                "clusterIdentifier": cluster,
                "computeNodeGroupIdentifier": "nodes",
            }),
        ),
    ]
}

/// RULE: a client error is answered with one status whatever class it is, and the class is
/// named in the body. The rows span the error classes these operations model, which declare
/// four different `@httpError` values between them
/// (`smithy_query(query="operation",operation="CreateCluster,GetCluster,GetQueue,TagResource")`:
/// `ValidationException` 400, `ServiceQuotaExceededException` 402, `AccessDeniedException`
/// 403, `ResourceNotFoundException` 404, `ConflictException` 409).
#[tokio::test]
async fn every_client_error_carries_one_status_and_names_its_class_in_the_body() {
    let router = emulator();
    let (status, created) = call(&router, "CreateCluster", &create_cluster("boundary-cluster")).await;
    assert_eq!(status, 200, "setup: CreateCluster: got {created}");
    let cluster_arn = created["cluster"]["arn"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // A tag write past the ceiling the service holds a resource's tag set to.
    let past_the_tag_ceiling: Value = (0..=MAX_TAGS_PER_RESOURCE)
        .map(|index| (format!("fill-{index:03}"), json!("value")))
        .collect::<serde_json::Map<String, Value>>()
        .into();

    // Each row: the case, the call, and (the error class the body must name, the
    // `@httpError` that class declares in the model).
    let rows: Vec<(&str, &str, Value, &str, u16)> = vec![
        (
            "a cluster name outside its pattern",
            "CreateCluster",
            create_cluster("pcs_reserved-name"),
            "ValidationException",
            400,
        ),
        (
            "a cluster identifier that resolves to nothing",
            "GetCluster",
            json!({"clusterIdentifier": "pcs_00000000000000000000"}),
            "ResourceNotFoundException",
            404,
        ),
        (
            "a queue read naming a cluster that resolves to nothing",
            "GetQueue",
            json!({"clusterIdentifier": "no-such-cluster", "queueIdentifier": "work"}),
            "AccessDeniedException",
            403,
        ),
        (
            "a second cluster of a name already taken",
            "CreateCluster",
            create_cluster("boundary-cluster"),
            "ConflictException",
            409,
        ),
        (
            "a tag write past the resource tag ceiling",
            "TagResource",
            json!({"resourceArn": &cluster_arn, "tags": past_the_tag_ceiling}),
            "ServiceQuotaExceededException",
            402,
        ),
    ];

    for (case, operation, request, expected_code, modelled_status) in rows {
        let (status, refused) = call(&router, operation, &request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (CLIENT_ERROR_STATUS, expected_code),
            "case {case:?}: expected {CLIENT_ERROR_STATUS} {expected_code} from {operation}, \
             got {status} with {refused}"
        );
        // The status is the service's, not the one the error shape declares: every row but
        // the one whose shape already declares it proves the two differ.
        if modelled_status != CLIENT_ERROR_STATUS {
            assert_ne!(
                status, modelled_status,
                "case {case:?}: expected the status NOT to be the modelled \
                 @httpError({modelled_status}), got {status} with {refused}"
            );
        }
    }

    // A failure that is NOT the client's keeps its own status: an operation that is
    // scaffolded and not implemented answers the scaffold channel's 501.
    let (status, not_implemented) = call(
        &router,
        "UpdateQueue",
        &json!({"clusterIdentifier": "boundary-cluster", "queueIdentifier": "work"}),
    )
    .await;
    assert_eq!(
        status, 501,
        "case a scaffolded operation: expected a server-side status to be kept, got \
         {status} with {not_implemented}"
    );
}

/// RULE: a cluster-scoped operation whose `clusterIdentifier` resolves to no cluster of this
/// account is refused by the FORM of that identifier — a generated cluster id (`pcs_…`) is the
/// not-found naming the cluster, a cluster NAME the permissions failure naming the caller, the
/// action and the identifier, which discloses nothing about whether such a cluster exists.
#[tokio::test]
async fn a_cluster_scoped_call_naming_no_cluster_is_refused_about_the_caller() {
    let router = emulator();

    // The two forms `ClusterIdentifier` admits, neither of which names a cluster here
    // (`smithy_query(query="shape",shape="ClusterIdentifier")`:
    // `^(pcs_[a-zA-Z0-9]+|[A-Za-z][A-Za-z0-9-]{2,40})$`), each with the refusal its form
    // earns.
    for identifier in ["no-such-cluster", "AnotherMissingOne"] {
        for (operation, request) in cluster_scoped_calls(identifier) {
            let case = format!("{operation} naming {identifier}");
            let (status, refused) = call(&router, operation, &request).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (CLIENT_ERROR_STATUS, "AccessDeniedException"),
                "case {case}: expected the permissions failure, got {status} with {refused}"
            );
            assert_eq!(
                refused["Message"].as_str(),
                Some(not_authorized(operation, identifier).as_str()),
                "case {case}: expected the IAM sentence in the Message member, got {refused}"
            );
            // ONE member carries the sentence, and nothing of a not-found is reported: the
            // refusal does not claim the resource was looked for.
            for member in ["message", "resourceId", "resourceType"] {
                assert!(
                    refused.get(member).is_none(),
                    "case {case}: a refusal of the caller must carry no {member}, got \
                     {refused}"
                );
            }
        }
    }

    // The generated-id form: a value only this service mints, so one nothing holds names a
    // cluster that is not there and the refusal is the modelled not-found, carrying the
    // identifier as supplied and the kind of identifier that did not resolve.
    for identifier in ["pcs_00000000000000000000", "pcs_0"] {
        for (operation, request) in cluster_scoped_calls(identifier) {
            let case = format!("{operation} naming {identifier}");
            let (status, refused) = call(&router, operation, &request).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
                "case {case}: expected the not-found, got {status} with {refused}"
            );
            assert_eq!(
                (
                    refused["message"].as_str(),
                    refused["resourceId"].as_str(),
                    refused["resourceType"].as_str()
                ),
                (
                    Some("AWS PCS couldn't find the cluster you specified."),
                    Some(identifier),
                    Some("Cluster ID")
                ),
                "case {case}: expected the cluster not-found and its modelled members, got \
                 {refused}"
            );
            assert!(
                refused.get("Message").is_none(),
                "case {case}: a not-found carries no IAM sentence, got {refused}"
            );
        }
    }
}

/// RULE, the other half: once the cluster DOES resolve, the same operations are served or
/// refused about the resource they name, and an operation whose own resource is the cluster
/// reports a not-found for an identifier that resolves to nothing.
#[tokio::test]
async fn a_resolvable_cluster_is_refused_about_the_resource_the_call_names() {
    let router = emulator();
    let (status, created) = call(&router, "CreateCluster", &create_cluster("held-cluster")).await;
    assert_eq!(status, 200, "setup: CreateCluster: got {created}");
    let cluster_id = created["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // Addressed by either form of the identifier of a cluster that EXISTS, no call is
    // refused about the caller: it is served, or refused about the child it names.
    for identifier in [cluster_id.as_str(), "held-cluster"] {
        for (operation, request) in cluster_scoped_calls(identifier) {
            let case = format!("{operation} naming {identifier}");
            let (status, answered) = call(&router, operation, &request).await;
            assert_ne!(
                error_code(&answered),
                "AccessDeniedException",
                "case {case}: a cluster that resolves must not be refused about the caller, \
                 got {status} with {answered}"
            );
            assert!(
                status == 200
                    || matches!(
                        error_code(&answered).as_str(),
                        "ResourceNotFoundException" | "ConflictException"
                    ),
                "case {case}: expected the call served or refused about the resource it \
                 names, got {status} with {answered}"
            );
        }
    }

    // An operation whose OWN resource is the cluster reports a not-found, carrying the
    // service's fixed sentence, the identifier as supplied and the kind of identifier that
    // did not resolve.
    let own_calls: Vec<(&str, Value)> = vec![
        (
            "GetCluster",
            json!({"clusterIdentifier": "no-such-cluster"}),
        ),
        (
            "DeleteCluster",
            json!({"clusterIdentifier": "pcs_00000000000000000000"}),
        ),
        (
            "UpdateCluster",
            json!({
                "clusterIdentifier": "AnotherMissingOne",
                "slurmConfiguration": {"scaleDownIdleTimeInSeconds": 700},
            }),
        ),
    ];
    for (operation, request) in own_calls {
        let identifier = request["clusterIdentifier"].as_str().unwrap_or_default();
        let case = format!("{operation} naming {identifier}");
        let (status, refused) = call(&router, operation, &request).await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
            "case {case}: expected the not-found, got {status} with {refused}"
        );
        assert_eq!(
            refused["message"].as_str(),
            Some("AWS PCS couldn't find the cluster you specified."),
            "case {case}: got {refused}"
        );
        assert_eq!(
            (
                refused["resourceId"].as_str(),
                refused["resourceType"].as_str()
            ),
            (Some(identifier), Some("Cluster ID")),
            "case {case}: expected the identifier and its kind in the modelled members, got \
             {refused}"
        );
    }
}

/// Both cluster-scoped DELETEs, as `(operation, the request naming `cluster` and `child`,
/// the kind of identifier a refusal about that child reports)`. Each addresses a child of a
/// cluster in the member its own input declares for it
/// (`smithy_query(query="operation",operation="DeleteQueue,DeleteComputeNodeGroup")`).
fn cluster_scoped_deletes(
    cluster: &str,
    child: &str,
) -> Vec<(&'static str, Value, &'static str)> {
    vec![
        (
            "DeleteQueue",
            json!({"clusterIdentifier": cluster, "queueIdentifier": child}),
            "Queue ID",
        ),
        (
            "DeleteComputeNodeGroup",
            json!({
                "clusterIdentifier": cluster,
                "computeNodeGroupIdentifier": child,
            }),
            "Compute Node Group ID",
        ),
    ]
}

/// RULE (rule 3 of the header): a cluster-scoped DELETE decides the cluster it is scoped to
/// BEFORE the child it names, so what it is refused about follows the `clusterIdentifier`
/// alone — a cluster NAME nothing resolves is the permissions failure naming the caller, the
/// action `pcs:<Operation>` and the identifier as supplied, in the `Message` member; a
/// generated cluster id nothing holds is the cluster's own not-found; and only once the
/// cluster HAS resolved is the call answered about the child, served when the child is there
/// and the child's own not-found when it is not.
///
/// A delete is the call a teardown loop branches on — a not-found reads as "already gone",
/// an authorization failure as terminal — so the rows span what the rule generalises over:
/// both cluster-scoped deletes, every form of cluster identifier `ClusterIdentifier` admits
/// at each published bound of its length, and children that a cluster of this account DOES
/// hold beside children nothing holds, because what a refusal states must not depend on
/// whether some other cluster holds a child of the name the request gave.
#[tokio::test]
async fn a_cluster_scoped_delete_is_refused_by_what_its_cluster_identifier_resolves_to() {
    let router = emulator();

    // A cluster that DOES exist, holding a queue and a compute node group under the very
    // names the refused calls below name.
    let (status, created) = call(&router, "CreateCluster", &create_cluster("held-cluster")).await;
    assert_eq!(status, 200, "setup: CreateCluster: got {created}");
    let cluster_id = created["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let (status, group) = call(
        &router,
        "CreateComputeNodeGroup",
        &create_compute_node_group("held-cluster", "nodes"),
    )
    .await;
    assert_eq!(status, 200, "setup: CreateComputeNodeGroup: got {group}");
    let (status, queue) = call(
        &router,
        "CreateQueue",
        &json!({
            "clusterIdentifier": "held-cluster",
            "queueName": "work",
            "computeNodeGroupConfigurations": [],
        }),
    )
    .await;
    assert_eq!(status, 200, "setup: CreateQueue: got {queue}");

    // The forms and widths the NAME alternative of `ClusterIdentifier` admits
    // (`smithy_query(query="shape",shape="ClusterIdentifier")`:
    // `^(pcs_[a-zA-Z0-9]+|[A-Za-z][A-Za-z0-9-]{2,40})$`), at each published bound of its
    // length — none of them a cluster this account holds.
    let longest_name = format!("A{}", "a".repeat(40));
    let unresolvable_names: [(&str, &str); 5] = [
        ("the shortest name the pattern allows", "abc"),
        ("a hyphenated name", "no-such-cluster"),
        ("a mixed-case name", "AnotherMissingOne"),
        ("a name opening with this service's own letters", "pcsx-cluster"),
        ("the longest name the pattern allows", longest_name.as_str()),
    ];
    // What the child member names: a queue and a compute node group the EXISTING cluster
    // holds, a child nothing holds anywhere, and the generated-id form of one.
    let children = ["work", "nodes", "no-such-child", "pcs_00000000000000000000"];

    for (named, identifier) in unresolvable_names {
        for child in children {
            for (operation, request, _) in cluster_scoped_deletes(identifier, child) {
                let case = format!("{operation} on {named} {identifier:?} naming {child:?}");
                let (status, refused) = call(&router, operation, &request).await;
                assert_eq!(
                    (status, error_code(&refused).as_str()),
                    (CLIENT_ERROR_STATUS, "AccessDeniedException"),
                    "case {case}: expected the permissions failure, got {status} with {refused}"
                );
                assert_eq!(
                    refused["Message"].as_str(),
                    Some(not_authorized(operation, identifier).as_str()),
                    "case {case}: expected the IAM sentence in the Message member, got \
                     {refused}"
                );
                // Nothing of a not-found is reported: the refusal does not claim the child
                // was looked for, nor disclose whether such a cluster exists.
                for member in ["message", "resourceId", "resourceType"] {
                    assert!(
                        refused.get(member).is_none(),
                        "case {case}: a refusal of the caller must carry no {member}, got \
                         {refused}"
                    );
                }
            }
        }
    }

    // The generated-id form: a value only this service mints, so one nothing holds names a
    // cluster that is not there — the modelled not-found, carrying the identifier as
    // supplied and the kind of identifier that did not resolve.
    for identifier in ["pcs_0", "pcs_00000000000000000000", "pcs_9aF3"] {
        for (operation, request, _) in cluster_scoped_deletes(identifier, "work") {
            let case = format!("{operation} naming the absent cluster id {identifier:?}");
            let (status, refused) = call(&router, operation, &request).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
                "case {case}: expected the cluster's not-found, got {status} with {refused}"
            );
            assert_eq!(
                (
                    refused["message"].as_str(),
                    refused["resourceId"].as_str(),
                    refused["resourceType"].as_str()
                ),
                (
                    Some("AWS PCS couldn't find the cluster you specified."),
                    Some(identifier),
                    Some("Cluster ID")
                ),
                "case {case}: expected the cluster not-found and its modelled members, got \
                 {refused}"
            );
            assert!(
                refused.get("Message").is_none(),
                "case {case}: a not-found carries no IAM sentence, got {refused}"
            );
        }
    }

    // A value the identifier's own pattern does not admit is refused for its FORM, before
    // either question is decided: one character past the longest name it allows, and one
    // short of the shortest.
    let past_the_longest = format!("A{}", "a".repeat(41));
    for (named, identifier) in [
        ("one past the longest name", past_the_longest.as_str()),
        ("one short of the shortest name", "ab"),
    ] {
        for (operation, request, _) in cluster_scoped_deletes(identifier, "work") {
            let case = format!("{operation} naming {named} {identifier:?}");
            let (status, refused) = call(&router, operation, &request).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (CLIENT_ERROR_STATUS, "ValidationException"),
                "case {case}: expected the identifier refused for its form, got {status} \
                 with {refused}"
            );
        }
    }

    // Once the cluster DOES resolve — addressed by either form of ITS identifier — the same
    // call is answered about the CHILD: the child's own not-found for one the cluster does
    // not hold, and no refusal of the caller.
    for cluster in [cluster_id.as_str(), "held-cluster"] {
        for (operation, request, identifier_label) in
            cluster_scoped_deletes(cluster, "no-such-child")
        {
            let case = format!("{operation} in the cluster {cluster:?}");
            let (status, refused) = call(&router, operation, &request).await;
            assert_eq!(
                (status, error_code(&refused).as_str()),
                (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
                "case {case}: expected the child's not-found, got {status} with {refused}"
            );
            assert_eq!(
                (
                    refused["resourceId"].as_str(),
                    refused["resourceType"].as_str()
                ),
                (Some("no-such-child"), Some(identifier_label)),
                "case {case}: expected the child identifier it could not resolve and its \
                 kind, got {refused}"
            );
            assert!(
                refused.get("Message").is_none(),
                "case {case}: a cluster that resolves must not be refused about the caller, \
                 got {refused}"
            );
        }
    }

    // And a child that IS there deletes, so the refusals above are the cluster boundary and
    // not a delete that never works: the queue addressed by the cluster's name, the compute
    // node group by its generated id. The removal each delete owes is materialised before
    // the next request is answered, so the read that FOLLOWS it reports the settled child —
    // gone for a queue, and `DELETED` for a compute node group, the one resource whose
    // status enum declares that member.
    for (operation, request, reader, member, settled) in [
        (
            "DeleteQueue",
            json!({"clusterIdentifier": "held-cluster", "queueIdentifier": "work"}),
            "GetQueue",
            "queue",
            None,
        ),
        (
            "DeleteComputeNodeGroup",
            json!({
                "clusterIdentifier": cluster_id.clone(),
                "computeNodeGroupIdentifier": "nodes",
            }),
            "GetComputeNodeGroup",
            "computeNodeGroup",
            Some("DELETED"),
        ),
    ] {
        let (status, deleted) = call(&router, operation, &request).await;
        assert_eq!(
            status, 200,
            "case {operation} of a child the cluster holds: got {status} with {deleted}"
        );
        let (status, read) = call(&router, reader, &request).await;
        match settled {
            Some(expected) => assert_eq!(
                (status, read[member]["status"].as_str()),
                (200, Some(expected)),
                "case {operation}: expected the child reported {expected} on the request \
                 that follows the delete, got {status} with {read}"
            ),
            None => assert_eq!(
                (status, error_code(&read).as_str()),
                (CLIENT_ERROR_STATUS, "ResourceNotFoundException"),
                "case {operation}: expected the child gone on the request that follows the \
                 delete, got {status} with {read}"
            ),
        }
    }

    // None of the refused calls wrote anything: the account still holds the one cluster the
    // setup created.
    let (status, listed) = call(&router, "ListClusters", &json!({})).await;
    assert_eq!(
        (status, listed["clusters"].as_array().map(Vec::len)),
        (200, Some(1)),
        "case after every refusal: expected the refusals to have created nothing, got \
         {status} with {listed}"
    );
}
