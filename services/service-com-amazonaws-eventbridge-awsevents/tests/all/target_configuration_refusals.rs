// SPDX-License-Identifier: CC0-1.0

//! What a target has to NAME for EventBridge to be able to invoke it at all, and
//! whose account its resource may belong to — driven over the wire (`awsJson1_1`,
//! `POST /` with `X-Amz-Target: AWSEvents.<Operation>`) through the real
//! `MultiServiceRouter`, so routing, deserialization, the modelled-constraint
//! validator, the handler and serialization are all exercised.
//!
//! Three rules are asserted here, each over a TABLE spanning its class:
//!
//! 1. **A target is refused unless it names what its own resource class can only be
//!    invoked through.** A class EventBridge addresses through a parameter block
//!    whose own members are `@required` (an Amazon ECS cluster's `EcsParameters`, a
//!    Batch job queue's `BatchParameters`, a Redshift cluster's
//!    `RedshiftDataParameters`, a Run Command document's `RunCommandParameters`)
//!    needs that block; the block's own members have to describe a thing the class
//!    could run (a task definition of the cluster's Region, a task count the
//!    cluster's `RunTask` would take, the members the launch type requires and only
//!    the ones it supports, collections within the sizes they document); and a
//!    target ARN of a service EventBridge invokes ONE kind of resource of has to
//!    name that kind. The refusal is HTTP 400 `ValidationException`, it names the
//!    member and the `Target$Id` it is about, and NOTHING of the request is stored —
//!    the rule's target list is read back after every row.
//! 2. **A target may only name another account's resource where EventBridge
//!    documents a cross-account destination.** That is the event BUS of another
//!    account ("set that account's event bus as a target of the rules in your
//!    account"); every other foreign resource is refused HTTP 400
//!    `AccessDeniedException`, whose sentence this service reports under `Message`.
//! 3. **`PutRule` validates the event pattern before it resolves the bus the rule
//!    would live on**, so a pattern the language does not admit is reported as
//!    `InvalidEventPatternException` whether or not the named bus exists — the
//!    precedence `CreateArchive` and `UpdateArchive` already follow — and no rule is
//!    created either way.
//!
//! The rows differ along the SERVICE and resource type the ARN names, the block the
//! entry carries, the VALUES inside it (counts at each published bound and one past
//! it, the forms a task definition can be referenced by, each launch type), the
//! ACCOUNT and Region the ARN names, the `Id` the entry carries and the number of
//! entries in the batch, so every refusal's subject is read off the request rather
//! than off a constant.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "eu-west-2";

/// The event pattern every rule below carries.
const PATTERN: &str = r#"{"source":["com.demo.orders"]}"#;

/// An account that is not this emulator's own.
const FOREIGN_ACCOUNT: &str = "111122223333";

/// The emulator: one router carrying only this service over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let credential = format!(
        "AKIDTESTTESTTEST/20260101/{REGION}/events/aws4_request, SignedHeaders=host, \
         Signature=0000"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "events.eu-west-2.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header("authorization", format!("AWS4-HMAC-SHA256 Credential={credential}"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&bytes).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&bytes)
            )
        })
    };
    (status, parsed)
}

/// The wire error code (`__type`), without its namespace; the empty string on a
/// success.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// The refusal's sentence, from whichever member this service reports it in.
fn detail(body: &Value) -> &str {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
}

/// The account every ARN below belongs to unless a row names another.
fn account() -> String {
    engine::emulator_account_id().to_string()
}

/// The ARN of an Amazon ECS cluster of this account and Region.
fn cluster_arn() -> String {
    format!("arn:aws:ecs:{REGION}:{}:cluster/orders", account())
}

/// The role EventBridge is handed to invoke a target it makes an API call for. An ECS
/// cluster target is one of them — the delivery is an `ecs:RunTask` on the cluster — so
/// every ECS row below carries it: a target naming no role is refused for THAT before any
/// of its parameters is read, which is the rule the `submitted_value_refusals` suite's own
/// table over that class states.
fn invoke_role() -> String {
    format!("arn:aws:iam::{}:role/events-invoke", account())
}

/// The ARN of a task definition of this account and Region.
fn task_definition_arn() -> String {
    format!("arn:aws:ecs:{REGION}:{}:task-definition/orders:1", account())
}

/// The parameter block an ECS cluster target is invoked through, naming nothing but
/// the task it runs.
fn ecs_parameters() -> Value {
    json!({ "TaskDefinitionArn": task_definition_arn() })
}

/// The VPC configuration block, carrying `subnets` subnets and `security_groups`
/// security groups.
fn network_configuration(subnets: usize, security_groups: usize) -> Value {
    let names = |prefix: &str, count: usize| -> Vec<String> {
        (0..count).map(|index| format!("{prefix}-{index}")).collect()
    };
    json!({
        "awsvpcConfiguration": {
            "Subnets": names("subnet", subnets),
            "SecurityGroups": names("sg", security_groups),
        }
    })
}

/// A rule on the default bus carrying [`PATTERN`], asserted to be accepted.
async fn given_rule(router: &MultiServiceRouter, case: &str, name: &str) {
    let (status, body) = call(
        router,
        "PutRule",
        &json!({ "Name": name, "EventPattern": PATTERN }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the rule must be created, got {status} {body}"
    );
}

/// The ids of the targets the rule `name` holds, in the order it reports them.
async fn target_ids(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<String> {
    let (status, body) = call(router, "ListTargetsByRule", &json!({ "Rule": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: ListTargetsByRule answered {status} {body}"
    );
    body["Targets"]
        .as_array()
        .map(|targets| {
            targets
                .iter()
                .map(|target| target["Id"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One row of the configuration table: the entries a `PutTargets` request carries
/// and the sentence it is refused with, or `None` for a request that is accepted.
struct Row {
    case: &'static str,
    targets: Value,
    refused: Option<String>,
    /// The ids the rule holds afterwards: every accepted entry, and nothing at all
    /// for a refused request.
    stored: Vec<&'static str>,
}

/// What a refusal states for a parameter block a target's own class can only be
/// invoked through and the entry did not carry.
fn must_be_specified(member: &str, target_id: &str) -> String {
    format!("Parameter(s) {member} must be specified for target: {target_id}.")
}

/// What a refusal states for a target parameter whose value the service will not
/// take.
fn not_valid_for_target(member: &str, target_id: &str) -> String {
    format!("Parameter(s) {member} not valid for target: {target_id}.")
}

/// What a refusal states for an ARN-valued member holding no ARN this service can
/// use.
fn not_an_arn(value: &str) -> String {
    format!("Parameter {value} is not valid. Reason: Provided Arn is not in correct format.")
}

/// What a refusal states for a collection outside the size it documents.
fn size_must_be(member: &str, fewest: usize, most: usize) -> String {
    format!("Parameter {member} is not valid. Reason: size must be in between {fewest} and {most}.")
}

/// The rows of the per-target configuration table.
///
/// The catalog of classes that need a parameter block is read off the `Target`
/// members documenting each one (`smithy_query(query="shape",shape="Target")`,
/// with each block's own `@required` members), so the rows include classes the
/// issue behind this suite never cited: a Batch job queue, a Redshift cluster and a
/// Run Command document.
fn configuration_rows() -> Vec<Row> {
    let account = account();
    let foreign_region_definition =
        format!("arn:aws:ecs:us-east-1:{account}:task-definition/orders:1");
    vec![
        Row {
            case: "an ECS cluster target carrying no EcsParameters",
            targets: json!([{
                "Id": "Target0",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
            }]),
            refused: Some(must_be_specified("EcsParameters", "Target0")),
            stored: vec![],
        },
        Row {
            case: "a Batch job queue target carrying no BatchParameters",
            targets: json!([{
                "Id": "batch-1",
                "Arn": format!("arn:aws:batch:{REGION}:{account}:job-queue/nightly"),
            }]),
            refused: Some(must_be_specified("BatchParameters", "batch-1")),
            stored: vec![],
        },
        Row {
            case: "a Redshift cluster target carrying no RedshiftDataParameters",
            targets: json!([{
                "Id": "warehouse",
                "Arn": format!("arn:aws:redshift:{REGION}:{account}:cluster:reports"),
            }]),
            refused: Some(must_be_specified("RedshiftDataParameters", "warehouse")),
            stored: vec![],
        },
        Row {
            case: "a Run Command document target carrying no RunCommandParameters",
            targets: json!([{
                "Id": "patch",
                "Arn": format!("arn:aws:ssm:{REGION}:{account}:document/AWS-RunShellScript"),
                "RoleArn": format!("arn:aws:iam::{account}:role/events-invoke"),
            }]),
            refused: Some(must_be_specified("RunCommandParameters", "patch")),
            stored: vec![],
        },
        Row {
            // A Systems Manager target of ANOTHER kind is invoked without that
            // block: the requirement is a property of the Run Command class.
            case: "a Systems Manager automation definition target",
            targets: json!([{
                "Id": "automation",
                "Arn": format!("arn:aws:ssm:{REGION}:{account}:automation-definition/restart:1"),
                "RoleArn": format!("arn:aws:iam::{account}:role/events-invoke"),
            }]),
            refused: None,
            stored: vec!["automation"],
        },
        Row {
            case: "a function target, a class that needs no parameter block",
            targets: json!([{
                "Id": "fn",
                "Arn": format!("arn:aws:lambda:{REGION}:{account}:function:orders"),
            }]),
            refused: None,
            stored: vec!["fn"],
        },
        Row {
            case: "an ECS cluster target carrying the task it runs",
            targets: json!([{
                "Id": "ecs-ok",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": ecs_parameters(),
            }]),
            refused: None,
            stored: vec!["ecs-ok"],
        },
        Row {
            case: "an ECS ARN naming a service rather than the cluster",
            targets: json!([{
                "Id": "svc",
                "Arn": format!("arn:aws:ecs:{REGION}:{account}:service/orders/web"),
                "EcsParameters": ecs_parameters(),
            }]),
            refused: Some(not_an_arn(&format!(
                "arn:aws:ecs:{REGION}:{account}:service/orders/web"
            ))),
            stored: vec![],
        },
        Row {
            case: "an ECS ARN naming the task definition rather than the cluster",
            targets: json!([{
                "Id": "td",
                "Arn": task_definition_arn(),
                "EcsParameters": ecs_parameters(),
            }]),
            refused: Some(not_an_arn(&task_definition_arn())),
            stored: vec![],
        },
        Row {
            case: "a task referenced by family and revision rather than by ARN",
            targets: json!([{
                "Id": "ref",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": { "TaskDefinitionArn": "orders:1" },
            }]),
            refused: Some(not_valid_for_target("TaskDefinitionArn", "ref")),
            stored: vec![],
        },
        Row {
            case: "a task referenced by a resource of another service",
            targets: json!([{
                "Id": "ref2",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": {
                    "TaskDefinitionArn": format!("arn:aws:batch:{REGION}:{account}:job-definition/orders:1"),
                },
            }]),
            refused: Some(not_valid_for_target("TaskDefinitionArn", "ref2")),
            stored: vec![],
        },
        Row {
            case: "a task definition of another Region than the cluster",
            targets: json!([{
                "Id": "ref3",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": { "TaskDefinitionArn": foreign_region_definition.clone() },
            }]),
            refused: Some(format!(
                "Region of {} does not match region of {foreign_region_definition}.",
                cluster_arn()
            )),
            stored: vec![],
        },
        Row {
            // The most tasks one firing may start (Amazon ECS `RunTask$count`),
            // and one past it.
            case: "the most tasks one firing may start",
            targets: json!([{
                "Id": "count-10",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": { "TaskDefinitionArn": task_definition_arn(), "TaskCount": 10 },
            }]),
            refused: None,
            stored: vec!["count-10"],
        },
        Row {
            case: "one task more than a firing may start",
            targets: json!([{
                "Id": "count-11",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": { "TaskDefinitionArn": task_definition_arn(), "TaskCount": 11 },
            }]),
            refused: Some(not_valid_for_target("TaskCount", "count-11")),
            stored: vec![],
        },
        Row {
            case: "a Fargate task carrying no network configuration",
            targets: json!([{
                "Id": "Target0",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": {
                    "TaskDefinitionArn": task_definition_arn(),
                    "LaunchType": "FARGATE",
                },
            }]),
            refused: Some(
                "Parameter NetworkConfiguration must be specified for target Target0 when \
                 launch type is FARGATE"
                    .to_string(),
            ),
            stored: vec![],
        },
        Row {
            case: "a Fargate task carrying its network configuration",
            targets: json!([{
                "Id": "fargate-ok",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": {
                    "TaskDefinitionArn": task_definition_arn(),
                    "LaunchType": "FARGATE",
                    "NetworkConfiguration": network_configuration(1, 0),
                    "PlatformVersion": "1.4.0",
                },
            }]),
            refused: None,
            stored: vec!["fargate-ok"],
        },
        Row {
            case: "a platform version beside a launch type that has none",
            targets: json!([{
                "Id": "ec2-platform",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": {
                    "TaskDefinitionArn": task_definition_arn(),
                    "LaunchType": "EC2",
                    "PlatformVersion": "1.4.0",
                },
            }]),
            refused: Some(
                "Parameter PlatformVersion for target ec2-platform is not supported when \
                 launch type is EC2"
                    .to_string(),
            ),
            stored: vec![],
        },
        Row {
            case: "a public IP asked for beside a launch type that cannot assign one",
            targets: json!([{
                "Id": "external-ip",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": {
                    "TaskDefinitionArn": task_definition_arn(),
                    "LaunchType": "EXTERNAL",
                    "NetworkConfiguration": {
                        "awsvpcConfiguration": {
                            "Subnets": ["subnet-0"],
                            "AssignPublicIp": "ENABLED",
                        }
                    },
                },
            }]),
            refused: Some(
                "Parameter AssignPublicIp for target external-ip is not supported when \
                 launch type is EXTERNAL"
                    .to_string(),
            ),
            stored: vec![],
        },
        Row {
            // The value every launch type states the same thing with is accepted
            // under each of them.
            case: "a public IP declined beside a launch type that cannot assign one",
            targets: json!([{
                "Id": "ec2-noip",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": {
                    "TaskDefinitionArn": task_definition_arn(),
                    "LaunchType": "EC2",
                    "NetworkConfiguration": {
                        "awsvpcConfiguration": {
                            "Subnets": ["subnet-0"],
                            "AssignPublicIp": "DISABLED",
                        }
                    },
                },
            }]),
            refused: None,
            stored: vec!["ec2-noip"],
        },
        Row {
            case: "the most subnets a task may join",
            targets: json!([{
                "Id": "subnets-16",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": {
                    "TaskDefinitionArn": task_definition_arn(),
                    "NetworkConfiguration": network_configuration(16, 5),
                },
            }]),
            refused: None,
            stored: vec!["subnets-16"],
        },
        Row {
            case: "one subnet more than a task may join",
            targets: json!([{
                "Id": "subnets-17",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": {
                    "TaskDefinitionArn": task_definition_arn(),
                    "NetworkConfiguration": network_configuration(17, 0),
                },
            }]),
            refused: Some(size_must_be("Subnets", 1, 16)),
            stored: vec![],
        },
        Row {
            case: "a network configuration naming no subnet at all",
            targets: json!([{
                "Id": "subnets-0",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": {
                    "TaskDefinitionArn": task_definition_arn(),
                    "NetworkConfiguration": { "awsvpcConfiguration": { "Subnets": [] } },
                },
            }]),
            refused: Some(size_must_be("Subnets", 1, 16)),
            stored: vec![],
        },
        Row {
            case: "one security group more than a task may join",
            targets: json!([{
                "Id": "sg-6",
                "Arn": cluster_arn(),
                "RoleArn": invoke_role(),
                "EcsParameters": {
                    "TaskDefinitionArn": task_definition_arn(),
                    "NetworkConfiguration": network_configuration(1, 6),
                },
            }]),
            refused: Some(size_must_be("SecurityGroups", 0, 5)),
            stored: vec![],
        },
        Row {
            // A BATCH is accepted or refused whole: the first entry is installable
            // and the second is not, so neither is stored.
            case: "a batch whose second entry names no parameter block",
            targets: json!([
                { "Id": "Target1", "Arn": format!("arn:aws:sns:{REGION}:{account}:orders") },
                { "Id": "Target2", "Arn": cluster_arn(), "RoleArn": invoke_role() },
            ]),
            refused: Some(must_be_specified("EcsParameters", "Target2")),
            stored: vec![],
        },
        Row {
            case: "a batch whose every entry is installable",
            targets: json!([
                { "Id": "Target1", "Arn": format!("arn:aws:sns:{REGION}:{account}:orders") },
                { "Id": "Target2", "Arn": cluster_arn(),
                  "RoleArn": invoke_role(), "EcsParameters": ecs_parameters() },
            ]),
            refused: None,
            stored: vec!["Target1", "Target2"],
        },
    ]
}

#[tokio::test]
async fn a_target_is_refused_unless_it_names_what_its_own_class_is_invoked_through() {
    let router = emulator();

    for (index, row) in configuration_rows().into_iter().enumerate() {
        let case = row.case;
        let rule = format!("target-configuration-{index}");
        given_rule(&router, case, &rule).await;

        let (status, body) = call(
            &router,
            "PutTargets",
            &json!({ "Rule": rule, "Targets": row.targets }),
        )
        .await;

        match &row.refused {
            Some(expected) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the request to be refused, got {status} {body}"
                );
                let code = error_code(&body);
                assert_eq!(
                    code, "ValidationException",
                    "case {case:?}: expected ValidationException, got {code:?} in {body}"
                );
                let actual = detail(&body);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected {expected:?}, got {actual:?}"
                );
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected the request to be accepted, got {status} {body}"
            ),
        }

        let stored = target_ids(&router, case, &rule).await;
        assert_eq!(
            stored, row.stored,
            "case {case:?}: expected the rule to hold {:?}, got {stored:?}",
            row.stored
        );
    }
}

/// One row of the account table: the target entry, and whether the account its ARN
/// names makes it a target this rule refuses.
struct AccountRow {
    case: &'static str,
    target: Value,
    refused: bool,
}

/// What a refusal states for a target in an account EventBridge will not deliver
/// across to.
fn cross_account_refusal(arn: &str) -> String {
    format!(
        "Access to the resource {arn} is denied. Reason: Adding cross-account target is not \
         permitted."
    )
}

fn account_rows() -> Vec<AccountRow> {
    let account = account();
    vec![
        AccountRow {
            case: "an ECS cluster of another account",
            target: json!({
                "Id": "t1",
                "Arn": format!("arn:aws:ecs:{REGION}:{FOREIGN_ACCOUNT}:cluster/orders"),
                "RoleArn": invoke_role(),
                "EcsParameters": {
                    "TaskDefinitionArn": format!(
                        "arn:aws:ecs:{REGION}:{FOREIGN_ACCOUNT}:task-definition/orders:1"
                    ),
                },
            }),
            refused: true,
        },
        AccountRow {
            case: "a queue of another account",
            target: json!({
                "Id": "t2",
                "Arn": format!("arn:aws:sqs:{REGION}:{FOREIGN_ACCOUNT}:orders"),
            }),
            refused: true,
        },
        AccountRow {
            case: "a function of another account",
            target: json!({
                "Id": "t3",
                "Arn": format!("arn:aws:lambda:{REGION}:{FOREIGN_ACCOUNT}:function:orders"),
            }),
            refused: true,
        },
        AccountRow {
            // The one cross-account destination `PutTargets` documents.
            case: "the event bus of another account",
            target: json!({
                "Id": "t4",
                "Arn": format!("arn:aws:events:{REGION}:{FOREIGN_ACCOUNT}:event-bus/partner"),
                "RoleArn": format!("arn:aws:iam::{account}:role/events-invoke"),
            }),
            refused: false,
        },
        AccountRow {
            // Another REGION of the caller's own account is a resource the account
            // owns: this rule is about whose resource it is.
            case: "a queue of another Region of this account",
            target: json!({
                "Id": "t5",
                "Arn": format!("arn:aws:sqs:us-west-2:{account}:orders"),
            }),
            refused: false,
        },
        AccountRow {
            // An ARN carrying no account at all is nobody's account to be outside
            // of.
            case: "an ARN carrying no account",
            target: json!({ "Id": "t6", "Arn": "arn:aws:s3:::orders-archive" }),
            refused: false,
        },
    ]
}

#[tokio::test]
async fn a_target_in_another_account_is_refused_unless_its_class_is_deliverable_across_accounts() {
    let router = emulator();

    for (index, row) in account_rows().into_iter().enumerate() {
        let case = row.case;
        let rule = format!("target-account-{index}");
        given_rule(&router, case, &rule).await;
        let arn = row.target["Arn"].as_str().unwrap_or_default().to_string();
        let id = row.target["Id"].as_str().unwrap_or_default().to_string();

        let (status, body) = call(
            &router,
            "PutTargets",
            &json!({ "Rule": rule, "Targets": [row.target] }),
        )
        .await;

        let stored = target_ids(&router, case, &rule).await;
        if !row.refused {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the target to be accepted, got {status} {body}"
            );
            assert_eq!(
                stored,
                vec![id],
                "case {case:?}: expected the rule to hold the target, got {stored:?}"
            );
            continue;
        }

        assert_eq!(
            status, 400,
            "case {case:?}: expected the target to be refused, got {status} {body}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "AccessDeniedException",
            "case {case:?}: expected AccessDeniedException, got {code:?} in {body}"
        );
        let expected = cross_account_refusal(&arn);
        let actual = body["Message"].as_str().unwrap_or_default();
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?} under Message, got {body}"
        );
        assert!(
            body.get("message").is_none(),
            "case {case:?}: this error's sentence travels under Message alone, got {body}"
        );
        assert!(
            stored.is_empty(),
            "case {case:?}: a refused target must not be stored, got {stored:?}"
        );
    }
}

/// One row of the precedence table: the bus the request names and whether it
/// exists, the pattern it carries, and the error class it earns.
struct PatternRow {
    case: &'static str,
    bus_exists: bool,
    pattern: &'static str,
    /// The error code, or `None` for a request that is accepted.
    code: Option<&'static str>,
}

fn pattern_rows() -> Vec<PatternRow> {
    vec![
        PatternRow {
            // The recorded form: a top-level value that is a bare string, on a bus
            // that does not exist.
            case: "a bare string value on a bus that does not exist",
            bus_exists: false,
            pattern: r#"{"source": "aws.health"}"#,
            code: Some("InvalidEventPatternException"),
        },
        PatternRow {
            case: "a pattern that is not an object, on a bus that does not exist",
            bus_exists: false,
            pattern: r#"["aws.health"]"#,
            code: Some("InvalidEventPatternException"),
        },
        PatternRow {
            case: "text that is not JSON at all, on a bus that does not exist",
            bus_exists: false,
            pattern: "{source:",
            code: Some("InvalidEventPatternException"),
        },
        PatternRow {
            case: "a bare string value on a bus that exists",
            bus_exists: true,
            pattern: r#"{"source": "aws.health"}"#,
            code: Some("InvalidEventPatternException"),
        },
        PatternRow {
            // A VALID pattern leaves the missing bus as the thing that is wrong.
            case: "a valid pattern on a bus that does not exist",
            bus_exists: false,
            pattern: PATTERN,
            code: Some("ResourceNotFoundException"),
        },
        PatternRow {
            case: "a valid pattern on a bus that exists",
            bus_exists: true,
            pattern: PATTERN,
            code: None,
        },
    ]
}

#[tokio::test]
async fn put_rule_judges_the_event_pattern_before_the_bus_it_would_live_on() {
    let router = emulator();

    for (index, row) in pattern_rows().into_iter().enumerate() {
        let case = row.case;
        let bus = format!("pattern-precedence-{index}");
        if row.bus_exists {
            let (status, body) = call(&router, "CreateEventBus", &json!({ "Name": bus })).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the bus must be created, got {status} {body}"
            );
        }
        let rule = format!("pattern-precedence-rule-{index}");

        let (status, body) = call(
            &router,
            "PutRule",
            &json!({ "Name": rule, "EventBusName": bus, "EventPattern": row.pattern }),
        )
        .await;

        match row.code {
            Some(expected) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected the request to be refused, got {status} {body}"
                );
                let code = error_code(&body);
                assert_eq!(
                    code, expected,
                    "case {case:?}: expected {expected}, got {code:?} in {body}"
                );
                let sentence = detail(&body);
                let opening = match expected {
                    "InvalidEventPatternException" => "Event pattern is not valid. Reason: ",
                    _ => "Event bus ",
                };
                assert!(
                    sentence.starts_with(opening),
                    "case {case:?}: expected a sentence opening {opening:?}, got {sentence:?}"
                );
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected the rule to be created, got {status} {body}"
            ),
        }

        // Nothing is created by a refused request: the bus that exists holds the
        // rule only where the request was accepted.
        if row.bus_exists {
            let (listed, rules) =
                call(&router, "ListRules", &json!({ "EventBusName": bus })).await;
            assert_eq!(
                listed, 200,
                "case {case:?}: ListRules answered {listed} {rules}"
            );
            let held = rules["Rules"].as_array().map_or(0, Vec::len);
            let expected_held = usize::from(row.code.is_none());
            assert_eq!(
                held, expected_held,
                "case {case:?}: expected {expected_held} rule(s) on the bus, got {rules}"
            );
        }
    }
}
