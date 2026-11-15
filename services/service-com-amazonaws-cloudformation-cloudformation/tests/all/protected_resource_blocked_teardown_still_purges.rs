// SPDX-License-Identifier: CC0-1.0

//! What a stack whose deletion FAILED leaves of the resources that deletion did delete, end
//! to end over each owning service's own wire.
//!
//! RULE under test: **a stack operation ENDS — completed or failed — only once every resource
//! it deleted is gone from the service that owns it, so a resource the stack reports
//! `DELETE_COMPLETE` is one that service no longer holds.** "CloudFormation deletes the
//! resources a stack delete reached even when a sibling resource blocks the stack", and the
//! service that owns one of those resources may answer the delete before it has carried it
//! out — a secret deleted with no recovery window ("Secrets Manager performs the actual
//! deletion with an asynchronous background process",
//! `DeleteSecretRequest$ForceDeleteWithoutRecovery`) and a state machine whose delete "sets
//! the state machine's status to DELETING and begins the deletion process"
//! (`DeleteStateMachine`) are both of that kind. A deletion that goes through waits those
//! answers out; a deletion a refusal ENDS has no later pass to wait in, so the resources it
//! deleted have to be gone by the time it reports `DELETE_FAILED`.
//!
//! Why it matters: a client treats `DELETE_COMPLETE` on a resource as proof the resource is
//! gone. A secret only SCHEDULED for deletion still answers `DescribeSecret`, still holds its
//! name against a redeployment of the same template, and keeps credential material readable;
//! a state machine still answers `DescribeStateMachine` and can still be executed.
//!
//! The rows differ along what the rule generalises over: WHICH service owns the resource whose
//! removal is asynchronous (AWS Secrets Manager, AWS Step Functions), HOW MANY such resources
//! one failed deletion deleted (one, two), WHERE in the teardown's walk they sit relative to
//! the resource that blocks the stack (before it, after it — the walk goes newest first, so
//! the logical ids decide), and which service refuses the deletion that fails the stack
//! (Amazon RDS for a protected DB instance, and for a protected DB cluster). Each deleted
//! resource is read back through the OWNING service, by every handle the stack published for
//! it, and the resource that blocked the stack is read back too: it has to have survived.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::stack_lifecycle::{
    call, deploy, member, output_value, rds, router, settled_stack, xml_blocks, xml_text,
};
use crate::stack_owned_secret_teardown::{error_code, secretsmanager};
use crate::step_functions_state_machine_replacement::sfn_json;
use service_com_amazonaws_cloudformation_cloudformation::state::{resource_status, status};

/// The error code Secrets Manager refuses a read of a secret it does not hold with
/// (`ResourceNotFoundException`, that service's own modelled refusal).
const SECRET_NOT_FOUND: &str = "ResourceNotFoundException";

/// The sentence that refusal carries, which names no part of the request
/// (`ResourceNotFoundException`, this emulator's one wording for a `SecretId` that resolves
/// to nothing).
const SECRET_NOT_FOUND_MESSAGE: &str = "Secrets Manager can't find the specified secret.";

/// The error code Step Functions refuses a call naming a state machine it does not hold with
/// (`StateMachineDoesNotExist`, that service's own modelled refusal).
const STATE_MACHINE_DOES_NOT_EXIST: &str = "StateMachineDoesNotExist";

/// The role a declared state machine runs its executions as, which the type requires.
const ROLE_ARN: &str = "arn:aws:iam::000000000000:role/StepFunctionsRole";

/// The workflow a declared state machine carries.
const DEFINITION: &str = r#"{"StartAt":"Done","States":{"Done":{"Type":"Pass","End":true}}}"#;

/// Which service owns a resource the failed deletion deleted, and so which wire says whether
/// it is really gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Owner {
    /// An `AWS::SecretsManager::Secret`, read back through Secrets Manager by its ARN and by
    /// its name.
    Secret,
    /// An `AWS::StepFunctions::StateMachine`, read back through Step Functions by its ARN.
    StateMachine,
}

/// One resource of a row's stack that the failed deletion is expected to have REMOVED.
#[derive(Debug, Clone, Copy)]
struct Removed {
    /// The logical id the template declares it under, which also names its output.
    logical_id: &'static str,
    owner: Owner,
    /// The name the declaration states, which is the second handle a secret is addressed by.
    declared_name: &'static str,
}

/// One row: a stack one resource blocks the deletion of, and the resources that deletion must
/// nonetheless have removed.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: String,
    /// The logical id Amazon RDS refuses to delete, which fails the stack.
    blocked: &'static str,
    /// The read that says the blocked resource survived: the action and its identifying
    /// member.
    survivor: (&'static str, &'static str, String),
    removed: &'static [Removed],
}

/// The declaration of a DB instance whose deletion protection is ON: Amazon RDS refuses
/// `DeleteDBInstance` for one, which is what makes the stack's deletion fail.
fn protected_instance(identifier: &str) -> Value {
    json!({
        "Type": "AWS::RDS::DBInstance",
        "DeletionPolicy": "Delete",
        "UpdateReplacePolicy": "Delete",
        "Properties": {
            "DBInstanceIdentifier": identifier,
            "Engine": "mysql",
            "DBInstanceClass": "db.t3.micro",
            "DeletionProtection": true
        }
    })
}

/// The declaration of an Aurora cluster whose deletion protection is ON: the recorded shape,
/// which Amazon RDS refuses `DeleteDBCluster` for.
fn protected_cluster(identifier: &str) -> Value {
    json!({
        "Type": "AWS::RDS::DBCluster",
        "DeletionPolicy": "Delete",
        "UpdateReplacePolicy": "Delete",
        "Properties": {
            "DBClusterIdentifier": identifier,
            "Engine": "aurora-postgresql",
            "ServerlessV2ScalingConfiguration": {"MinCapacity": 0.5, "MaxCapacity": 1},
            "MasterUsername": "dbadmin",
            "MasterUserPassword": "an-example-password",
            "DeletionProtection": true
        }
    })
}

/// The declaration of a secret the stack owns outright, which its deletion removes with no
/// recovery window.
fn declared_secret(name: &str) -> Value {
    json!({
        "Type": "AWS::SecretsManager::Secret",
        "DeletionPolicy": "Delete",
        "UpdateReplacePolicy": "Delete",
        "Properties": {
            "Name": name,
            "Description": "cluster credentials, deliberately unrotated",
            "SecretString": "{\"username\":\"dbadmin\",\"password\":\"an-example-password\"}",
            "Tags": [{"Key": "probe", "Value": "blocked-teardown"}]
        }
    })
}

/// The declaration of a state machine the stack owns, whose delete that service carries out
/// asynchronously.
fn declared_machine(name: &str) -> Value {
    json!({
        "Type": "AWS::StepFunctions::StateMachine",
        "DeletionPolicy": "Delete",
        "UpdateReplacePolicy": "Delete",
        "Properties": {
            "StateMachineName": name,
            "RoleArn": ROLE_ARN,
            "DefinitionString": DEFINITION
        }
    })
}

/// A template declaring the row's resources, publishing each removed resource's physical id
/// as an output — "Ref returns the ARN of the secret", and the ARN of the state machine.
fn template(resources: Vec<(&str, Value)>, removed: &[Removed]) -> String {
    let mut declared = serde_json::Map::new();
    for (logical_id, resource) in resources {
        declared.insert(logical_id.to_string(), resource);
    }
    let mut outputs = serde_json::Map::new();
    for entry in removed {
        outputs.insert(
            arn_output(entry.logical_id),
            json!({"Value": {"Ref": entry.logical_id}}),
        );
    }
    json!({"Resources": declared, "Outputs": outputs}).to_string()
}

/// The output key one removed resource's physical id is published under.
fn arn_output(logical_id: &str) -> String {
    format!("{logical_id}Arn")
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded shape: an independent secret beside a protected Aurora cluster, and
            // the walk reaches the SECRET first (it goes newest first, and the cluster is
            // declared before it).
            case: "a secret deleted before the protected cluster that blocks the stack",
            stack: "blocked-teardown-cluster",
            template: template(
                vec![
                    ("ACluster", protected_cluster("blocked-teardown-cluster-db")),
                    ("ZSecret", declared_secret("blocked-teardown-cluster-master")),
                ],
                FIRST_ROW_REMOVED,
            ),
            blocked: "ACluster",
            survivor: (
                "DescribeDBClusters",
                "DBClusterIdentifier",
                "blocked-teardown-cluster-db".to_string(),
            ),
            removed: FIRST_ROW_REMOVED,
        },
        Row {
            // The other ORDER, TWO secrets rather than one, and a different refusing resource:
            // the walk reaches the protected DB instance first and the secrets after it, so the
            // refusal is already recorded when their deletes are accepted.
            case: "two secrets deleted after the protected DB instance that blocks the stack",
            stack: "blocked-teardown-instance",
            template: template(
                vec![
                    ("ASecret", declared_secret("blocked-teardown-first")),
                    ("BSecret", declared_secret("blocked-teardown-second")),
                    ("ZDb", protected_instance("blocked-teardown-instance-db")),
                ],
                SECOND_ROW_REMOVED,
            ),
            blocked: "ZDb",
            survivor: (
                "DescribeDBInstances",
                "DBInstanceIdentifier",
                "blocked-teardown-instance-db".to_string(),
            ),
            removed: SECOND_ROW_REMOVED,
        },
        Row {
            // ANOTHER owning service whose delete is asynchronous: a Step Functions state
            // machine, beside a secret, with the protected instance between them in the walk.
            case: "a state machine and a secret deleted around the protected DB instance",
            stack: "blocked-teardown-machine",
            template: template(
                vec![
                    ("ASecret", declared_secret("blocked-teardown-machine-master")),
                    ("MDb", protected_instance("blocked-teardown-machine-db")),
                    ("ZMachine", declared_machine("blocked-teardown-machine")),
                ],
                THIRD_ROW_REMOVED,
            ),
            blocked: "MDb",
            survivor: (
                "DescribeDBInstances",
                "DBInstanceIdentifier",
                "blocked-teardown-machine-db".to_string(),
            ),
            removed: THIRD_ROW_REMOVED,
        },
    ]
}

const FIRST_ROW_REMOVED: &[Removed] = &[Removed {
    logical_id: "ZSecret",
    owner: Owner::Secret,
    declared_name: "blocked-teardown-cluster-master",
}];

const SECOND_ROW_REMOVED: &[Removed] = &[
    Removed {
        logical_id: "ASecret",
        owner: Owner::Secret,
        declared_name: "blocked-teardown-first",
    },
    Removed {
        logical_id: "BSecret",
        owner: Owner::Secret,
        declared_name: "blocked-teardown-second",
    },
];

const THIRD_ROW_REMOVED: &[Removed] = &[
    Removed {
        logical_id: "ASecret",
        owner: Owner::Secret,
        declared_name: "blocked-teardown-machine-master",
    },
    Removed {
        logical_id: "ZMachine",
        owner: Owner::StateMachine,
        declared_name: "blocked-teardown-machine",
    },
];

/// The `ResourceStatus` `DescribeStackResources` reports for `logical_id`.
async fn resource_statuses(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
) -> Vec<(String, String)> {
    let (code, body) = call(
        router,
        "DescribeStackResources",
        &[member("StackName", stack)],
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: DescribeStackResources must answer 200, got {code} {body}"
    );
    xml_blocks(&body, "member")
        .iter()
        .map(|entry| {
            (
                xml_text(entry, "LogicalResourceId").unwrap_or_default(),
                xml_text(entry, "ResourceStatus").unwrap_or_default(),
            )
        })
        .collect()
}

/// Assert that every read of a REMOVED resource is refused by the service that owned it,
/// through each handle the stack published for it.
async fn assert_gone(router: &MultiServiceRouter, case: &str, entry: &Removed, physical_id: &str) {
    match entry.owner {
        Owner::Secret => {
            for addressed in [physical_id, entry.declared_name] {
                for operation in ["DescribeSecret", "GetSecretValue"] {
                    let (code, refused) =
                        secretsmanager(router, operation, &json!({"SecretId": addressed})).await;
                    assert_eq!(
                        code, 400,
                        "case {case:?}: expected {operation}({addressed}) to be refused once \
                         {} reports {}, got {code} {refused}",
                        entry.logical_id,
                        resource_status::DELETE_COMPLETE
                    );
                    assert_eq!(
                        error_code(case, &refused),
                        SECRET_NOT_FOUND,
                        "case {case:?}: expected {operation}({addressed}) to be refused with \
                         {SECRET_NOT_FOUND}, got {refused}"
                    );
                    assert_eq!(
                        refused
                            .get("Message")
                            .or_else(|| refused.get("message"))
                            .and_then(Value::as_str),
                        Some(SECRET_NOT_FOUND_MESSAGE),
                        "case {case:?}: expected the refusal of {operation}({addressed}) to \
                         carry {SECRET_NOT_FOUND_MESSAGE:?}, got {refused}"
                    );
                }
            }
        }
        Owner::StateMachine => {
            let (code, refused) = sfn_json(
                router,
                "DescribeStateMachine",
                &json!({"stateMachineArn": physical_id}),
            )
            .await;
            assert_eq!(
                code, 400,
                "case {case:?}: expected DescribeStateMachine({physical_id}) to be refused \
                 once {} reports {}, got {code} {refused}",
                entry.logical_id,
                resource_status::DELETE_COMPLETE
            );
            assert_eq!(
                error_code(case, &refused),
                STATE_MACHINE_DOES_NOT_EXIST,
                "case {case:?}: expected DescribeStateMachine({physical_id}) to be refused \
                 with {STATE_MACHINE_DOES_NOT_EXIST}, got {refused}"
            );
        }
    }
}

#[tokio::test]
async fn a_blocked_teardown_has_removed_every_resource_it_reports_deleted() {
    for row in rows() {
        let router = router();
        deploy(&router, row.stack, &row.template).await;
        let created = settled_stack(&router, row.case, row.stack).await;
        assert_eq!(
            xml_text(&created, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {:?}: expected the create to complete, got {created}",
            row.case
        );
        // The handles the stack published, read BEFORE the deletion: a read of the owning
        // service afterwards is held to the id the stack itself reported.
        let published: Vec<(Removed, String)> = row
            .removed
            .iter()
            .map(|entry| {
                let output = arn_output(entry.logical_id);
                let id = output_value(&created, &output).unwrap_or_else(|| {
                    panic!(
                        "case {:?}: the stack published no {output} in {created}",
                        row.case
                    )
                });
                (*entry, id)
            })
            .collect();

        let (code, body) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
        assert_eq!(
            code, 200,
            "case {:?}: DeleteStack must be accepted — the stack reports the outcome — got \
             {code} {body}",
            row.case
        );
        // Waits for the CONDITION: the first status the stack reports that is not an
        // operation in flight. Everything after this read is what a client sees the moment
        // the deletion ended.
        let described = settled_stack(&router, row.case, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::DELETE_FAILED),
            "case {:?}: expected the refused deletion to fail the stack, got {described}",
            row.case
        );

        let statuses = resource_statuses(&router, row.case, row.stack).await;
        let reported = |logical_id: &str| {
            statuses
                .iter()
                .find(|(id, _)| id == logical_id)
                .map(|(_, status)| status.clone())
                .unwrap_or_default()
        };
        assert_eq!(
            reported(row.blocked),
            resource_status::DELETE_FAILED,
            "case {:?}: expected {} to report the refused delete, got {statuses:?}",
            row.case,
            row.blocked
        );
        for (entry, physical_id) in &published {
            assert_eq!(
                reported(entry.logical_id),
                resource_status::DELETE_COMPLETE,
                "case {:?}: expected {} to report its deletion as complete, got {statuses:?}",
                row.case,
                entry.logical_id
            );
            // ...and the record is BACKED: the service that owned the resource holds nothing
            // under the handle the stack published, on the FIRST read after the deletion
            // ended.
            assert_gone(&router, row.case, entry, physical_id).await;
        }

        // The resource that blocked the deletion survived it: a refusal removes nothing.
        let (action, identifier, value) = &row.survivor;
        let (code, survivor) = rds(&router, action, &[(identifier, value)]).await;
        assert_eq!(
            code, 200,
            "case {:?}: expected {action}({value}) to still report the protected resource, \
             got {code} {survivor}",
            row.case
        );
        assert_eq!(
            xml_text(&survivor, "DeletionProtection").as_deref(),
            Some("true"),
            "case {:?}: expected the refused deletion to leave deletion protection on, got \
             {survivor}",
            row.case
        );
    }
}
