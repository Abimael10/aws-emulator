// SPDX-License-Identifier: CC0-1.0

//! End-to-end wire tests for how SageMaker RENDERS a `ValidationException` report: the
//! order its clauses come out in, the form a constraint broken by a list's ELEMENTS takes,
//! and the value set an enum-typed member is told.
//!
//! Every case goes over the wire (`POST /` with `X-Amz-Target: SageMaker.<Op>`,
//! `awsJson1_1`) through the same `MultiServiceRouter` the production router uses, via the
//! `Emulator` harness `inference_deployment_lifecycle` owns, so what these tests exercise is
//! what a real SDK client gets.
//!
//! Each test asserts ONE rule over a TABLE of inputs rather than one recorded call:
//!
//! 1. the clauses of one refusal are stated in the order the layer's findings table is
//!    walked — keyed on the NAME of the member each clause is about — which is neither the
//!    order the model declares those members nor the order the request wrote them;
//! 2. a constraint broken by a LIST's ELEMENTS is stated ONCE at the LIST MEMBER, quoting
//!    the whole list that member held and nesting the element shape's complete declared
//!    constraint set, however many of its entries offend;
//! 3. an enum-typed member outside its value set is told that set over the values the
//!    SERVICE holds — more than the public model declares, for two of this model's enums —
//!    listed in that same table's order.

use serde_json::{json, Value};

use crate::inference_deployment_lifecycle::{error_code, message, Emulator, ACCOUNT};

/// The region every ARN in this suite is minted in.
const REGION: &str = "us-east-1";

/// A cluster id of the form `ClusterArn`'s pattern fixes (`…:cluster/[a-z0-9]{12}`,
/// `smithy_query(query="shape",shape="ClusterArn")`).
const CLUSTER_ID: &str = "aaaa0000bbbb";

/// A well-formed `ClusterArn` for [`CLUSTER_ID`].
fn cluster_arn() -> String {
    format!("arn:aws:sagemaker:{REGION}:{ACCOUNT}:cluster/{CLUSTER_ID}")
}

/// A well-formed `RoleArn` (`^arn:aws[a-z\-]*:iam::\d{12}:role/?[a-zA-Z_0-9+=,.@\-_/]+$`).
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/service-role/sagemaker-role")
}

/// The `ValidationException` envelope around the clauses of one refusal, as this service
/// words it: one clause per constraint, counted in the opening sentence and joined by
/// `; `.
fn report(clauses: &[&str]) -> String {
    let noun = if clauses.len() == 1 { "error" } else { "errors" };
    format!(
        "{} validation {noun} detected: {}",
        clauses.len(),
        clauses.join("; ")
    )
}

/// One clause about a member whose own value broke a constraint.
fn clause(value: &str, path: &str, requirement: &str) -> String {
    format!("Value '{value}' at '{path}' failed to satisfy constraint: {requirement}")
}

/// The requirement a `@pattern` violation states: the expression WITHOUT the `^`/`$` the
/// model spells the whole-value match with.
fn pattern(expression: &str) -> String {
    format!("Member must satisfy regular expression pattern: {expression}")
}

/// The requirement a clause stated at a COLLECTION member nests: everything an ELEMENT has
/// to satisfy, in the order a clause lists them.
fn element_set(requirements: &[String]) -> String {
    format!("Member must satisfy constraint: [{}]", requirements.join(", "))
}

/// The expressions the model declares for the two node-identifier element shapes, as a
/// clause reports them. Sources: `smithy_query(query="shape",shape="ClusterNodeId")`
/// (`^i-[a-f0-9]{8}(?:[a-f0-9]{9})?$`, `@length 1..256`) and
/// `smithy_query(query="shape",shape="ClusterNodeLogicalId")`
/// (`^[a-zA-Z0-9][a-zA-Z0-9\-]*[a-zA-Z0-9]$`, `@length 1..128`).
const NODE_ID_EXPRESSION: &str = r"i-[a-f0-9]{8}(?:[a-f0-9]{9})?";
/// See [`NODE_ID_EXPRESSION`].
const NODE_LOGICAL_ID_EXPRESSION: &str = r"[a-zA-Z0-9][a-zA-Z0-9\-]*[a-zA-Z0-9]";

/// The whole constraint set an entry of `NodeIds` has to satisfy, as a clause stated at
/// that member nests it: the element shape's declared maximum, its minimum, its expression,
/// and that the entry be there at all (the list is not `@sparse`).
fn node_id_set() -> String {
    element_set(&[
        "Member must have length less than or equal to 256".to_string(),
        "Member must have length greater than or equal to 1".to_string(),
        pattern(NODE_ID_EXPRESSION),
        "Member must not be null".to_string(),
    ])
}

/// The same set for an entry of `NodeLogicalIds`.
fn node_logical_id_set() -> String {
    element_set(&[
        "Member must have length less than or equal to 128".to_string(),
        "Member must have length greater than or equal to 1".to_string(),
        pattern(NODE_LOGICAL_ID_EXPRESSION),
        "Member must not be null".to_string(),
    ])
}

/// Send `body` to `target` and answer the message of the `ValidationException` it must be
/// refused with, asserting the status and the code first so a wording assertion never
/// reads a success.
async fn refused_message(emulator: &Emulator, case: &str, target: &str, body: &Value) -> String {
    let (status, refused) = emulator.call(target, body).await;
    assert_eq!(
        (status, error_code(&refused).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {status} with {refused}"
    );
    message(&refused)
}

/// RULE: a refusal about several members states its clauses in the order the layer's
/// findings table is walked — a function of the member NAMES alone — so neither the order
/// the model declares those members in nor the order the request wrote them decides it.
#[tokio::test]
async fn the_clauses_of_one_refusal_come_out_in_the_tables_own_order() {
    let emulator = Emulator::new();
    // Sources: `smithy_query(query="operation",operation="AttachClusterNodeVolume")` —
    // `ClusterArn` (`^arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:cluster/[a-z0-9]{12}$`),
    // `NodeId` (`^i-[a-f0-9]{8}(?:[a-f0-9]{9})?$`), `VolumeId`
    // (`^vol-[a-f0-9]{8}(?:[a-f0-9]{9})?$`), declared in that order.
    let cluster_arn_expression =
        r"arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:cluster/[a-z0-9]{12}";
    let volume_expression = r"vol-[a-f0-9]{8}(?:[a-f0-9]{9})?";
    let cluster_clause =
        |value: &str| clause(value, "clusterArn", &pattern(cluster_arn_expression));
    let node_clause = |value: &str| clause(value, "nodeId", &pattern(NODE_ID_EXPRESSION));
    let volume_clause = |value: &str| clause(value, "volumeId", &pattern(volume_expression));

    // Rows differ along WHICH members break their constraint, how many do, and the shape of
    // the values that break them. The three-member row is the one a recording covers: AWS
    // states it `clusterArn`, `volumeId`, `nodeId`, while the model declares the members
    // `ClusterArn`, `NodeId`, `VolumeId`.
    let rows: [(&str, Value, String); 4] = [
        (
            "all three members of a volume attach",
            json!({
                "ClusterArn": "not-a-cluster-arn",
                "NodeId": "not-a-node",
                "VolumeId": "not-a-volume",
            }),
            report(&[
                &cluster_clause("not-a-cluster-arn"),
                &volume_clause("not-a-volume"),
                &node_clause("not-a-node"),
            ]),
        ),
        (
            "the same three members with other values",
            json!({ "ClusterArn": "x", "NodeId": "i-0", "VolumeId": "vol" }),
            report(&[
                &cluster_clause("x"),
                &volume_clause("vol"),
                &node_clause("i-0"),
            ]),
        ),
        (
            "the volume and the node, with a well-formed cluster",
            json!({
                "ClusterArn": cluster_arn(),
                "NodeId": "node-7",
                "VolumeId": "volume-7",
            }),
            report(&[&volume_clause("volume-7"), &node_clause("node-7")]),
        ),
        (
            "the cluster and the node, with a well-formed volume",
            json!({
                "ClusterArn": "arn:aws:sagemaker:us-east-1:000000000000:cluster/NOT",
                "NodeId": "node-7",
                "VolumeId": "vol-0123456789abcdef0",
            }),
            report(&[
                &cluster_clause("arn:aws:sagemaker:us-east-1:000000000000:cluster/NOT"),
                &node_clause("node-7"),
            ]),
        ),
    ];

    for (case, body, expected) in rows {
        let stated = refused_message(&emulator, case, "AttachClusterNodeVolume", &body).await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}

/// RULE: a constraint broken by a LIST's ELEMENTS is stated once at the LIST MEMBER — the
/// subject is the whole list that member held, and the requirement is everything an element
/// has to satisfy — so one list is ONE validation error however many of its entries offend
/// and whatever position they sit at.
#[tokio::test]
async fn a_broken_element_constraint_is_stated_at_the_list_member() {
    let emulator = Emulator::new();
    let node_ids_clause = |rendered: &str| clause(rendered, "nodeIds", &node_id_set());

    // Rows differ along the operation carrying the list, which list member carries it, how
    // many of its entries offend, where in the list they sit, and whether another member of
    // the same request breaks a constraint of its own.
    let rows: [(&str, &str, Value, String); 7] = [
        (
            "one offending entry, the only entry",
            "BatchDeleteClusterNodes",
            json!({ "ClusterName": "hyperpod", "NodeIds": ["not-an-instance"] }),
            report(&[&node_ids_clause("[not-an-instance]")]),
        ),
        (
            "two offending entries, which is still one validation error",
            "BatchDeleteClusterNodes",
            json!({ "ClusterName": "hyperpod", "NodeIds": ["first-bad", "second-bad"] }),
            report(&[&node_ids_clause("[first-bad, second-bad]")]),
        ),
        (
            "one offending entry after a well-formed one: the whole list is the subject",
            "BatchDeleteClusterNodes",
            json!({
                "ClusterName": "hyperpod",
                "NodeIds": ["i-0123456789abcdef0", "bad-tail"],
            }),
            report(&[&node_ids_clause("[i-0123456789abcdef0, bad-tail]")]),
        ),
        (
            "the same list on the reboot call",
            "BatchRebootClusterNodes",
            json!({ "ClusterName": "hyperpod", "NodeIds": ["not-an-instance"] }),
            report(&[&node_ids_clause("[not-an-instance]")]),
        ),
        (
            "the same list on the replace call",
            "BatchReplaceClusterNodes",
            json!({ "ClusterName": "hyperpod", "NodeIds": ["not-an-instance"] }),
            report(&[&node_ids_clause("[not-an-instance]")]),
        ),
        (
            "the logical-id list, whose elements declare another set",
            "BatchDeleteClusterNodes",
            json!({ "ClusterName": "hyperpod", "NodeLogicalIds": ["bad logical id!"] }),
            report(&[&clause(
                "[bad logical id!]",
                "nodeLogicalIds",
                &node_logical_id_set(),
            )]),
        ),
        (
            "a list element beside another member of the same request",
            "BatchDeleteClusterNodes",
            json!({ "ClusterName": "bad name!", "NodeIds": ["not-an-instance"] }),
            report(&[
                &node_ids_clause("[not-an-instance]"),
                // Source: `smithy_query(query="shape",shape="ClusterNameOrArn")`, reported
                // without the `^`/`$` the model spells the whole-value match with.
                &clause(
                    "bad name!",
                    "clusterName",
                    &pattern(
                        r"(arn:aws[a-z\-]*:sagemaker:[a-z0-9\-]*:[0-9]{12}:cluster/[a-z0-9]{12})|([a-zA-Z0-9](-*[a-zA-Z0-9]){0,62})",
                    ),
                ),
            ]),
        ),
    ];

    for (case, target, body, expected) in rows {
        let stated = refused_message(&emulator, case, target, &body).await;
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}

/// RULE: an enum-typed member whose value is not one of its type's is told that type's
/// value set as the SERVICE holds it — which for two of this model's enums is more values
/// than the public model declares — listed in the order the table holding them is walked,
/// so every member targeting one enum is told the same listing.
#[tokio::test]
async fn an_enum_value_set_is_listed_over_the_values_the_service_holds() {
    let emulator = Emulator::new();
    let listed = |values: &[&str]| format!("Member must satisfy enum value set: [{}]", values.join(", "));
    // The eight sizes a recorded `CreateMlflowTrackingServer` refusal lists, in the order it
    // lists them: the three the public model declares
    // (`smithy_query(query="shape",shape="TrackingServerSize")`) and the five `mm1.*` sizes
    // the service holds beside them.
    let tracking_server_sizes = listed(&[
        "mm1.xlarge",
        "Small",
        "mm1.small",
        "mm1.medium",
        "Medium",
        "mm1.2xlarge",
        "Large",
        "mm1.large",
    ]);
    // The three a recorded `CreateComputeQuota` refusal lists, in its order; the model
    // declares them `Lend`, `DontLend`, `LendAndBorrow`
    // (`smithy_query(query="shape",shape="ResourceSharingStrategy")`).
    let sharing_strategies = listed(&["DontLend", "Lend", "LendAndBorrow"]);
    // ...and the three of `ModelApprovalStatus`, which the model declares `Approved`,
    // `Rejected`, `PendingManualApproval`.
    let approval_statuses = listed(&["PendingManualApproval", "Approved", "Rejected"]);

    let tracking_server = |size: &str| {
        json!({
            "TrackingServerName": "mlflow-enum-set",
            "ArtifactStoreUri": "s3://artifacts-bucket/mlflow",
            "RoleArn": role_arn(),
            "TrackingServerSize": size,
        })
    };
    let compute_quota = |strategy: &str| {
        json!({
            "Name": "quota-enum-set",
            "ClusterArn": cluster_arn(),
            "ComputeQuotaConfig": {
                "ComputeQuotaResources": [{ "InstanceType": "ml.g5.xlarge", "Count": 2 }],
                "ResourceSharingConfig": { "Strategy": strategy },
            },
            "ComputeQuotaTarget": { "TeamName": "research" },
        })
    };

    // Rows differ along the enum, the operation carrying it, the member's depth in the
    // request, and the shape of the value that is not one of the set (a plain word, a
    // differently-cased member, a member with trailing text, the empty string).
    let rows: [(&str, &str, Value, &str, &String); 7] = [
        (
            "a tracking server size outside the set",
            "CreateMlflowTrackingServer",
            tracking_server("ExtraLarge"),
            "trackingServerSize",
            &tracking_server_sizes,
        ),
        (
            "a tracking server size in the wrong case",
            "CreateMlflowTrackingServer",
            tracking_server("small"),
            "trackingServerSize",
            &tracking_server_sizes,
        ),
        (
            "the same member on the update call",
            "UpdateMlflowTrackingServer",
            json!({ "TrackingServerName": "mlflow-enum-set", "TrackingServerSize": "mm2.small" }),
            "trackingServerSize",
            &tracking_server_sizes,
        ),
        (
            "a resource-sharing strategy outside the set",
            "CreateComputeQuota",
            compute_quota("Share"),
            "computeQuotaConfig.resourceSharingConfig.strategy",
            &sharing_strategies,
        ),
        (
            "a resource-sharing strategy spelled as the doc spells it",
            "CreateComputeQuota",
            compute_quota("LendandBorrow"),
            "computeQuotaConfig.resourceSharingConfig.strategy",
            &sharing_strategies,
        ),
        (
            "an approval status outside the set",
            "CreateModelPackage",
            json!({
                "ModelPackageGroupName": "enum-set-group",
                "ModelApprovalStatus": "Maybe",
            }),
            "modelApprovalStatus",
            &approval_statuses,
        ),
        (
            "an approval status that is the empty string",
            "CreateModelPackage",
            json!({ "ModelPackageGroupName": "enum-set-group", "ModelApprovalStatus": "" }),
            "modelApprovalStatus",
            &approval_statuses,
        ),
    ];

    for (case, target, body, path, requirement) in rows {
        let rejected = rejected_value(&body, path);
        let stated = refused_message(&emulator, case, target, &body).await;
        let expected = report(&[&clause(&rejected, path, requirement)]);
        assert_eq!(
            stated, expected,
            "case {case:?}: expected {expected:?}, got {stated:?}"
        );
    }
}

/// The value `body` carries at the member `path` names, read back so a row states its
/// rejected value once.
fn rejected_value(body: &Value, path: &str) -> String {
    let mut at = body;
    for step in path.split('.') {
        // The request writes members in the model's spelling (UpperCamel); a violation path
        // spells the same member lowerCamel.
        let mut upper = step.to_string();
        upper.replace_range(..1, &step[..1].to_uppercase());
        at = at.get(&upper).unwrap_or(&Value::Null);
    }
    at.as_str().unwrap_or_default().to_string()
}

/// RULE: the listing is the SET the service holds for that enum, reordered — the model's own
/// set for every enum it holds no more values for, with nothing added and nothing dropped.
#[tokio::test]
async fn a_listing_holds_exactly_the_values_the_enum_admits() {
    let emulator = Emulator::new();
    // The deployment instance types of an optimization job: 55 values, the largest value set
    // of this model a refusal lists, so the one that shows the listing is the whole set and
    // not a sample of it (`smithy_query(query="shape",
    // shape="OptimizationJobDeploymentInstanceType")`, read through the generated constant).
    let declared: Vec<&str> =
        service_com_amazonaws_sagemaker_sagemaker::ops::types::OPTIMIZATION_JOB_DEPLOYMENT_INSTANCE_TYPE_VALUES
            .to_vec();
    let body = json!({
        "OptimizationJobName": "optimization-enum-set",
        "RoleArn": role_arn(),
        "ModelSource": { "S3": { "S3Uri": "s3://artifacts-bucket/model/" } },
        "DeploymentInstanceType": "ml.not.an.instance",
        "OptimizationConfigs": [],
        "OutputConfig": { "S3OutputLocation": "s3://artifacts-bucket/out/" },
        "StoppingCondition": { "MaxRuntimeInSeconds": 3600 },
    });

    let stated = refused_message(
        &emulator,
        "a deployment instance type outside the set",
        "CreateOptimizationJob",
        &body,
    )
    .await;
    let opened = "Member must satisfy enum value set: [";
    let (_, tail) = stated
        .split_once(opened)
        .unwrap_or_else(|| panic!("expected a listed value set, got {stated:?}"));
    let listed: Vec<&str> = tail
        .trim_end_matches(']')
        .split(", ")
        .collect();
    let mut sorted_listing = listed.clone();
    let mut sorted_declared = declared.clone();
    sorted_listing.sort_unstable();
    sorted_declared.sort_unstable();
    assert_eq!(
        sorted_listing, sorted_declared,
        "expected the listing to hold exactly the values the model declares, got {listed:?}"
    );
    assert_ne!(
        listed, declared,
        "expected the listing to be the table's order rather than the model's declaration \
         order, got {listed:?}"
    );
}
