// SPDX-License-Identifier: CC0-1.0

//! The `serviceRole` a created compute environment is associated with, end to end over the
//! wire.
//!
//! Every case drives real HTTP (restJson1) requests through the router the emulator mounts
//! and reads the result back through `DescribeComputeEnvironments`, so what is asserted is
//! what a client sees.
//!
//! The rule under test, over a SET of inputs: the role the create NAMES is the one the
//! environment keeps, and a create that names none leaves a `MANAGED` environment associated
//! with this account's Batch service-linked role — the substitution
//! `CreateComputeEnvironmentRequest$serviceRole` documents ("if your account already created
//! the Batch service-linked role, that role is used by default for your compute environment
//! unless you specify a different role here... This automatic service-linked role creation
//! only applies to MANAGED compute environments") — while an environment of any other type
//! is associated with no role at all.
//!
//! The rows differ along the type of the environment, its compute type (every modelled
//! `CRType`), the orchestration it uses, and whether the request carries a role and in which
//! form, because none of those is what the rule turns on: only the environment's type is.

use serde_json::{json, Value};

use service_com_amazonaws_batch_awsbatchv20160810::ops::types::C_R_TYPE_VALUES;

use crate::job_lifecycle::Batch;

/// The ARN of this account's Batch service-linked role, written out here rather than read
/// from the crate under test, so the test pins the STRING a client reads.
///
/// Its fixed parts are the `aws` partition, the `iam` service of the role itself (IAM is
/// global, so the ARN carries no Region), the reserved service-linked-role path
/// `/aws-service-role/<service principal>/` under this service's own principal, and the role
/// name AWS Batch publishes, `AWSServiceRoleForBatch`. The only part that comes from state is
/// the account, which is this emulator's own.
fn service_linked_role_arn() -> String {
    format!(
        "arn:aws:iam::{}:role/aws-service-role/batch.amazonaws.com/AWSServiceRoleForBatch",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// An IAM role ARN of the account, for the rows that name a role of their own.
fn role_arn(path_and_name: &str) -> String {
    format!(
        "arn:aws:iam::{}:role/{path_and_name}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// The compute resources a `MANAGED` environment is created with here: the two required
/// members of `ComputeResource` (`type`, `maxvCpus`) plus the network the Fargate types take.
fn managed_compute_resources(compute_type: &str) -> Value {
    json!({
        "type": compute_type,
        "maxvCpus": 4,
        "subnets": ["subnet-1111"],
        "securityGroupIds": ["sg-1111"],
    })
}

/// Create the compute environment `name` from `members` (everything but the name).
async fn create_environment(batch: &Batch, name: &str, members: &Value) {
    let mut request = members.clone();
    request["computeEnvironmentName"] = json!(name);
    batch.ok("/v1/createcomputeenvironment", request).await;
}

/// The one `ComputeEnvironmentDetail` for `reference`.
async fn describe(batch: &Batch, reference: &str) -> Value {
    let body = batch
        .ok(
            "/v1/describecomputeenvironments",
            json!({"computeEnvironments": [reference]}),
        )
        .await;
    let environments = body["computeEnvironments"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        environments.len(),
        1,
        "case describe {reference}: expected exactly one ComputeEnvironmentDetail, got {body}"
    );
    environments[0].clone()
}

/// One row: an environment created from `create`, whose described `serviceRole` must be
/// `expected` (`None` meaning the member is absent).
struct Row {
    case: String,
    create: Value,
    expected: Option<String>,
}

#[tokio::test]
async fn a_managed_environment_created_without_a_role_runs_as_the_batch_service_linked_role() {
    let substituted = service_linked_role_arn();
    let mut rows = Vec::new();

    // Every modelled compute type, MANAGED and carrying no `serviceRole`: the substitution
    // turns on the environment's TYPE, so none of them escapes it.
    for compute_type in C_R_TYPE_VALUES {
        rows.push(Row {
            case: format!("MANAGED over {compute_type} compute resources, no serviceRole"),
            create: json!({
                "type": "MANAGED",
                "state": "ENABLED",
                "computeResources": managed_compute_resources(compute_type),
            }),
            expected: Some(substituted.clone()),
        });
    }
    // The same for a MANAGED environment orchestrated with Amazon EKS rather than Amazon ECS,
    // and for one created DISABLED: neither is what the rule turns on.
    rows.push(Row {
        case: "MANAGED, orchestrated with Amazon EKS, no serviceRole".to_string(),
        create: json!({
            "type": "MANAGED",
            "state": "ENABLED",
            "computeResources": managed_compute_resources("EC2"),
            "eksConfiguration": {
                "eksClusterArn": format!(
                    "arn:aws:eks:us-east-1:{}:cluster/batch",
                    engine::EMULATOR_ACCOUNT_ID
                ),
                "kubernetesNamespace": "batch-jobs",
            },
        }),
        expected: Some(substituted.clone()),
    });
    rows.push(Row {
        case: "MANAGED and DISABLED, no serviceRole".to_string(),
        create: json!({
            "type": "MANAGED",
            "state": "DISABLED",
            "computeResources": managed_compute_resources("FARGATE"),
        }),
        expected: Some(substituted.clone()),
    });
    // A role the request NAMES is the one the environment keeps, whichever form it takes and
    // whichever type the environment is: "that role is used by default for your compute
    // environment unless you specify a different role here".
    for named in [
        role_arn("AWSBatchServiceRole"),
        role_arn("service-role/AWSBatchServiceRole"),
        role_arn("team/nested/BatchRole"),
    ] {
        rows.push(Row {
            case: format!("MANAGED, serviceRole {named}"),
            create: json!({
                "type": "MANAGED",
                "state": "ENABLED",
                "computeResources": managed_compute_resources("EC2"),
                "serviceRole": named,
            }),
            expected: Some(named.clone()),
        });
        rows.push(Row {
            case: format!("UNMANAGED, serviceRole {named}"),
            create: json!({"type": "UNMANAGED", "state": "ENABLED", "serviceRole": named}),
            expected: Some(named),
        });
    }
    // The automatic substitution "only applies to MANAGED compute environments", so an
    // UNMANAGED environment that names none is associated with none.
    rows.push(Row {
        case: "UNMANAGED, no serviceRole".to_string(),
        create: json!({"type": "UNMANAGED", "state": "ENABLED"}),
        expected: None,
    });
    rows.push(Row {
        case: "UNMANAGED with unmanagedvCpus, no serviceRole".to_string(),
        create: json!({"type": "UNMANAGED", "state": "ENABLED", "unmanagedvCpus": 4}),
        expected: None,
    });

    for (index, row) in rows.iter().enumerate() {
        // One emulator per row: what a row asserts is this environment's own state.
        let batch = Batch::new();
        let name = format!("ce-service-role-{index}");
        let case = row.case.as_str();
        create_environment(&batch, &name, &row.create).await;

        let detail = describe(&batch, &name).await;
        let actual = detail.get("serviceRole").cloned();
        match &row.expected {
            Some(expected) => {
                assert_eq!(
                    actual.as_ref().and_then(Value::as_str),
                    Some(expected.as_str()),
                    "case {case}: expected serviceRole {expected}, got {actual:?} in {detail}"
                );
            }
            None => {
                assert!(
                    actual.is_none() || actual == Some(Value::Null),
                    "case {case}: expected no serviceRole at all, got {actual:?} in {detail}"
                );
            }
        }

        // The role is settled at the create, so a second read reports the same one: nothing
        // about it is decided by a read.
        let again = describe(&batch, &name).await;
        assert_eq!(
            again.get("serviceRole"),
            detail.get("serviceRole"),
            "case {case}: two reads must report the same serviceRole, got {again}"
        );
    }
}

#[tokio::test]
async fn the_substituted_role_is_the_published_batch_service_linked_role() {
    // The role the substitution names is a service-linked role of this service: the one AWS
    // Batch publishes, `AWSServiceRoleForBatch`, filed under the reserved path IAM gives a
    // service-linked role of the `batch.amazonaws.com` principal. Source: the role name this
    // service's own model names in `UpdateComputeEnvironment` ("the AWSServiceRoleForBatch
    // service-linked role"), and the path IAM documents for a service-linked role.
    let batch = Batch::new();
    create_environment(
        &batch,
        "ce-published-role",
        &json!({
            "type": "MANAGED",
            "state": "ENABLED",
            "computeResources": managed_compute_resources("FARGATE"),
        }),
    )
    .await;

    let detail = describe(&batch, "ce-published-role").await;
    let role = detail
        .get("serviceRole")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        role.ends_with("/AWSServiceRoleForBatch"),
        "case the published role name: expected the ARN to end in /AWSServiceRoleForBatch, \
         got {role:?} in {detail}"
    );
    assert!(
        role.contains(":role/aws-service-role/batch.amazonaws.com/"),
        "case the service-linked role path: expected the reserved path under this service's \
         principal, got {role:?} in {detail}"
    );
    assert_eq!(
        role,
        service_linked_role_arn(),
        "case the whole ARN: expected {}, got {role:?}",
        service_linked_role_arn()
    );
    // IAM is global, so the role's ARN carries no Region — unlike the environment's own.
    let environment_arn = detail
        .get("computeEnvironmentArn")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        environment_arn.contains(":us-east-1:"),
        "case the environment's own ARN: expected the signed Region, got {environment_arn:?}"
    );
    assert!(
        !role.contains(":us-east-1:"),
        "case the role's ARN: IAM is global, so it carries no Region, got {role:?}"
    );
}

#[tokio::test]
async fn an_update_replaces_the_substituted_role_and_keeps_it_otherwise() {
    let batch = Batch::new();
    create_environment(
        &batch,
        "ce-updated-role",
        &json!({
            "type": "MANAGED",
            "state": "ENABLED",
            "computeResources": managed_compute_resources("FARGATE"),
        }),
    )
    .await;

    // An update that names other members leaves the substituted role in place.
    batch
        .ok(
            "/v1/updatecomputeenvironment",
            json!({"computeEnvironment": "ce-updated-role", "state": "DISABLED"}),
        )
        .await;
    let kept = describe(&batch, "ce-updated-role").await;
    assert_eq!(
        kept.get("serviceRole").and_then(Value::as_str),
        Some(service_linked_role_arn().as_str()),
        "case an update naming other members: expected the substituted role to stay, got {kept}"
    );

    // An update that names a role replaces it: the caller's role is the one Batch assumes
    // from then on.
    let named = role_arn("AWSBatchServiceRole");
    batch
        .ok(
            "/v1/updatecomputeenvironment",
            json!({"computeEnvironment": "ce-updated-role", "serviceRole": named}),
        )
        .await;
    let replaced = describe(&batch, "ce-updated-role").await;
    assert_eq!(
        replaced.get("serviceRole").and_then(Value::as_str),
        Some(named.as_str()),
        "case an update naming a role: expected {named}, got {replaced}"
    );
}
