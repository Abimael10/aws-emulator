// SPDX-License-Identifier: CC0-1.0

//! The ORDER this service lists an enumerated type's admissible values in when it
//! refuses a value outside that type, driven over the wire (`awsJson1_1`, `POST /`
//! with `X-Amz-Target: AWSEvents.<Operation>`) through the real
//! `MultiServiceRouter` — the same path a client takes, so routing,
//! deserialization, the modelled-constraint validator and the handler are all
//! exercised.
//!
//! The RULE, asserted over a table rather than one request: a value outside a
//! modelled enum is refused with HTTP 400 `ValidationException` whose message is
//! the one constraint envelope — `1 validation error detected: Value '<value>' at
//! '<member path>' failed to satisfy constraint: Member must satisfy enum value
//! set: [<values>]` — where the value and the member path come from the request
//! and `<values>` is the whole admissible set listed in the order the front door's
//! own TABLE of those values is walked: each value's bucket ascending, the bucket
//! being the low bits of its own string hash, and two values sharing a bucket read
//! from the head each was pushed onto, so the LATER-declared of the two comes
//! first. The order is computed from the values, so it belongs to the enumerated
//! TYPE rather than to the member, the operation, the nesting depth or the model's
//! declaration order.
//!
//! The rows differ along the enumerated type (six of this model's, including one
//! whose listing is the declared order, one that is a rotation of it, one that is
//! neither that nor the sorted order, and one whose set HAS a bucket collision),
//! the operation, the depth the member sits at (top level, inside a list element,
//! inside a structure under one, inside a list under that), the position in a list,
//! and the form of the offending value (the wrong case of an admissible value, a
//! value of another type, a near miss).
//!
//! Each listed set below is written out, with the bucket each of its values sits
//! in; `the_listed_order_is_the_tables_bucket_order` then checks every one of them
//! against the bucket the shared derivation computes, so a row states the order
//! rather than whatever the service happened to produce.

use std::collections::BTreeMap;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_eventbridge_awsevents::EmulatedService;
use service_common::validation_message::enum_value_bucket;
use smithy_server_runtime::MultiServiceRouter;

/// The region every request below is signed for.
const REGION: &str = "eu-west-2";

/// `LaunchType`'s values as the MODEL declares them, in declaration order
/// (`smithy_query(query="shape", shape="LaunchType")`). Written out here, not
/// imported, so the implementation cannot make a row pass by agreeing with
/// itself.
const DECLARED_LAUNCH_TYPE: [&str; 3] = ["EC2", "FARGATE", "EXTERNAL"];

/// The order this service LISTS `LaunchType` in: buckets 2, 11, 20 — a rotation of
/// the declared order.
const LISTED_LAUNCH_TYPE: [&str; 3] = ["FARGATE", "EXTERNAL", "EC2"];

/// `RuleState`'s values as the MODEL declares them, in declaration order
/// (`smithy_query(query="shape", shape="RuleState")`).
const DECLARED_RULE_STATE: [&str; 3] = [
    "ENABLED",
    "DISABLED",
    "ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENTS",
];

/// The order this service LISTS `RuleState` in: buckets 0, 1, 28 — neither the
/// declared order nor the sorted one.
const LISTED_RULE_STATE: [&str; 3] = [
    "ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENTS",
    "ENABLED",
    "DISABLED",
];

/// The other enumerated types the rows below carry, as the model declares them
/// (`smithy_query(query="shape", shape="AssignPublicIp,PropagateTags,\
/// PlacementStrategyType,ApiDestinationHttpMethod")`).
const DECLARED_ASSIGN_PUBLIC_IP: [&str; 2] = ["ENABLED", "DISABLED"];
const DECLARED_PROPAGATE_TAGS: [&str; 1] = ["TASK_DEFINITION"];
const DECLARED_PLACEMENT_STRATEGY_TYPE: [&str; 3] = ["random", "spread", "binpack"];
const DECLARED_API_DESTINATION_HTTP_METHOD: [&str; 7] =
    ["POST", "GET", "HEAD", "OPTIONS", "PUT", "PATCH", "DELETE"];

/// `AssignPublicIp` listed: buckets 1, 28 — which here IS the declared order, so
/// the rows cannot all have been produced by moving every set.
const LISTED_ASSIGN_PUBLIC_IP: [&str; 2] = ["ENABLED", "DISABLED"];

/// `PropagateTags` listed: one value, bucket 27.
const LISTED_PROPAGATE_TAGS: [&str; 1] = ["TASK_DEFINITION"];

/// `PlacementStrategyType` listed: buckets 0, 3, 19 — a lowercase set, so the
/// order is over the values' own text rather than over an upper-case spelling of
/// it.
const LISTED_PLACEMENT_STRATEGY_TYPE: [&str; 3] = ["binpack", "random", "spread"];

/// `ApiDestinationHttpMethod` listed: buckets 0, 0, 8, 11, 15, 22, 30 — the one
/// set here with a COLLISION (`HEAD` and `POST` share bucket 0), read from the head
/// each value was pushed onto, so the later-declared `HEAD` is listed first.
const LISTED_API_DESTINATION_HTTP_METHOD: [&str; 7] =
    ["HEAD", "POST", "PATCH", "DELETE", "PUT", "GET", "OPTIONS"];

/// The rule every `PutTargets` row below names.
const RULE: &str = "enum-order-rule";

fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// One `awsJson1_1` call to this service.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/events/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(&payload)
            )
        })
    };
    (status, parsed)
}

/// The wire error code (`__type`) of a failure body, without its namespace.
fn error_code(body: &Value) -> &str {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .rsplit('#')
        .next()
        .unwrap_or_default()
}

/// The `message` a failure body carries.
fn message(body: &Value) -> &str {
    body.get("message").and_then(Value::as_str).unwrap_or_default()
}

/// The whole message a refusal of `value` at `path` must carry, stating `set` as
/// the value set — the envelope spelled out here rather than built from the
/// service's own helpers.
fn expected_message(value: &str, path: &str, set: &[&str]) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{path}' failed to satisfy constraint: \
         Member must satisfy enum value set: [{}]",
        set.join(", ")
    )
}

/// The bracketed value set a refusal states, as the comma-separated values it
/// lists, in the order it lists them.
fn stated_set(message: &str) -> Vec<String> {
    let (_, set) = message
        .split_once("Member must satisfy enum value set: [")
        .unwrap_or_else(|| panic!("the refusal states no enum value set: {message:?}"));
    let set = set
        .strip_suffix(']')
        .unwrap_or_else(|| panic!("the stated value set is not closed: {message:?}"));
    set.split(", ").map(str::to_string).collect()
}

/// A task definition ARN, so an `EcsParameters` carries its one required member.
fn task_definition_arn() -> String {
    format!(
        "arn:aws:ecs:{REGION}:{}:task-definition/demo:1",
        engine::emulator_account_id()
    )
}

/// An ECS cluster ARN, the target a row's `EcsParameters` belongs to.
fn cluster_arn() -> String {
    format!(
        "arn:aws:ecs:{REGION}:{}:cluster/demo",
        engine::emulator_account_id()
    )
}

/// A connection ARN in the form `ConnectionArn` is modelled with, so a
/// `CreateApiDestination` row carries a well-formed one.
fn connection_arn() -> String {
    format!(
        "arn:aws:events:{REGION}:{}:connection/conn-one/01234567-89ab-cdef-0123-456789abcdef",
        engine::emulator_account_id()
    )
}

/// `EcsParameters` carrying its required `TaskDefinitionArn` plus `members`.
fn ecs_parameters(members: Value) -> Value {
    let mut parameters = json!({ "TaskDefinitionArn": task_definition_arn() });
    let object = parameters
        .as_object_mut()
        .expect("the parameters are an object");
    for (member, value) in members.as_object().expect("the members are an object") {
        object.insert(member.clone(), value.clone());
    }
    parameters
}

/// A `PutTargets` request whose target at `position` (1-based) carries
/// `parameters`, every earlier target being one this service accepts.
fn put_targets(position: usize, parameters: Value) -> Value {
    let targets: Vec<Value> = (1..=position)
        .map(|index| {
            let mut target = json!({
                "Id": format!("Target{index}"),
                "Arn": cluster_arn(),
            });
            if index == position {
                target["EcsParameters"] = parameters.clone();
            } else {
                target["EcsParameters"] = ecs_parameters(json!({}));
            }
            target
        })
        .collect();
    json!({ "Rule": RULE, "Targets": targets })
}

/// One row: what the request is, and what the refusal must say.
struct Row {
    case: &'static str,
    enumeration: &'static str,
    operation: &'static str,
    request: Value,
    value: &'static str,
    path: &'static str,
    set: Vec<&'static str>,
}

/// Every row this suite asserts, spanning the enumerated type, the operation, the
/// member's depth, the position in a list and the form of the offending value.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "PutRule State in the wrong case",
            enumeration: "RuleState",
            operation: "PutRule",
            request: json!({ "Name": RULE, "State": "enabled" }),
            value: "enabled",
            path: "state",
            set: LISTED_RULE_STATE.to_vec(),
        },
        Row {
            case: "PutRule State as a near miss of an admissible value",
            enumeration: "RuleState",
            operation: "PutRule",
            request: json!({
                "Name": RULE,
                "State": "ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENT",
            }),
            value: "ENABLED_WITH_ALL_CLOUDTRAIL_MANAGEMENT_EVENT",
            path: "state",
            set: LISTED_RULE_STATE.to_vec(),
        },
        Row {
            case: "PutRule State on a named bus, carrying a value of another type",
            enumeration: "RuleState",
            operation: "PutRule",
            request: json!({ "Name": RULE, "EventBusName": "custom-bus", "State": "FARGATE" }),
            value: "FARGATE",
            path: "state",
            set: LISTED_RULE_STATE.to_vec(),
        },
        Row {
            case: "PutTargets LaunchType in the wrong case, first target",
            enumeration: "LaunchType",
            operation: "PutTargets",
            request: put_targets(1, ecs_parameters(json!({ "LaunchType": "fargate" }))),
            value: "fargate",
            path: "targets.1.member.ecsParameters.launchType",
            set: LISTED_LAUNCH_TYPE.to_vec(),
        },
        Row {
            case: "PutTargets LaunchType in the wrong case, third target of a batch",
            enumeration: "LaunchType",
            operation: "PutTargets",
            request: put_targets(3, ecs_parameters(json!({ "LaunchType": "ec2" }))),
            value: "ec2",
            path: "targets.3.member.ecsParameters.launchType",
            set: LISTED_LAUNCH_TYPE.to_vec(),
        },
        Row {
            case: "PutTargets LaunchType carrying a value of another type",
            enumeration: "LaunchType",
            operation: "PutTargets",
            request: put_targets(2, ecs_parameters(json!({ "LaunchType": "ENABLED" }))),
            value: "ENABLED",
            path: "targets.2.member.ecsParameters.launchType",
            set: LISTED_LAUNCH_TYPE.to_vec(),
        },
        Row {
            case: "PutTargets PropagateTags in the wrong case",
            enumeration: "PropagateTags",
            operation: "PutTargets",
            request: put_targets(1, ecs_parameters(json!({ "PropagateTags": "task_definition" }))),
            value: "task_definition",
            path: "targets.1.member.ecsParameters.propagateTags",
            set: LISTED_PROPAGATE_TAGS.to_vec(),
        },
        Row {
            case: "PutTargets AssignPublicIp, two structures below the list element",
            enumeration: "AssignPublicIp",
            operation: "PutTargets",
            request: put_targets(
                1,
                ecs_parameters(json!({
                    "LaunchType": "FARGATE",
                    "NetworkConfiguration": {
                        "awsvpcConfiguration": {
                            "Subnets": ["subnet-0123456789abcdef0"],
                            "AssignPublicIp": "enabled",
                        },
                    },
                })),
            ),
            value: "enabled",
            path: "targets.1.member.ecsParameters.networkConfiguration.awsvpcConfiguration.assignPublicIp",
            set: LISTED_ASSIGN_PUBLIC_IP.to_vec(),
        },
        Row {
            case: "PutTargets PlacementStrategy type, in a list under the list element",
            enumeration: "PlacementStrategyType",
            operation: "PutTargets",
            request: put_targets(
                1,
                ecs_parameters(json!({
                    "PlacementStrategy": [
                        { "type": "spread", "field": "instanceId" },
                        { "type": "Spread" },
                    ],
                })),
            ),
            value: "Spread",
            path: "targets.1.member.ecsParameters.placementStrategy.2.member.type",
            set: LISTED_PLACEMENT_STRATEGY_TYPE.to_vec(),
        },
        Row {
            case: "CreateApiDestination HttpMethod in the wrong case",
            enumeration: "ApiDestinationHttpMethod",
            operation: "CreateApiDestination",
            request: json!({
                "Name": "enum-order-destination",
                "ConnectionArn": connection_arn(),
                "InvocationEndpoint": "https://example.com/hook",
                "HttpMethod": "post",
            }),
            value: "post",
            path: "httpMethod",
            set: LISTED_API_DESTINATION_HTTP_METHOD.to_vec(),
        },
    ]
}

/// The rule: every refusal of a value outside an enumerated type is the one
/// constraint envelope, stating the value and the member path from the request and
/// the whole admissible set in the order this service lists that type's values in.
#[tokio::test]
async fn an_enum_refusal_lists_the_value_set_in_the_tables_order() {
    let emulator = emulator();
    // A rule to aim the target rows at, so a refused row is refused for its
    // enum rather than for a rule that is not there.
    let (created, body) = call(
        &emulator,
        "PutRule",
        &json!({ "Name": RULE, "EventPattern": r#"{"source":["com.demo"]}"# }),
    )
    .await;
    assert_eq!(created, 200, "the rule the rows name: {body:?}");

    for row in rows() {
        let (status, body) = call(&emulator, row.operation, &row.request).await;
        assert_eq!(
            status, 400,
            "case {:?}: expected HTTP 400, got {status} with {body:?}",
            row.case
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {:?}: expected ValidationException, got {body:?}",
            row.case
        );
        assert_eq!(
            message(&body),
            expected_message(row.value, row.path, &row.set),
            "case {:?}: the refusal is worded differently",
            row.case
        );
    }
}

/// The listing order is a property of the TYPE: every member targeting one
/// enumerated type — of any operation, at any depth, at any list position — lists
/// that type's set in the one order, and that set holds exactly the values the
/// model declares, each once.
#[tokio::test]
async fn one_enum_is_listed_in_one_order_wherever_it_is_refused() {
    let emulator = emulator();
    let (created, body) = call(
        &emulator,
        "PutRule",
        &json!({ "Name": RULE, "EventPattern": r#"{"source":["com.demo"]}"# }),
    )
    .await;
    assert_eq!(created, 200, "the rule the rows name: {body:?}");

    // The values each enumerated type declares, so a listed set can be checked
    // for holding exactly them.
    let declared: BTreeMap<&str, Vec<&str>> = BTreeMap::from([
        ("RuleState", DECLARED_RULE_STATE.to_vec()),
        ("LaunchType", DECLARED_LAUNCH_TYPE.to_vec()),
        ("PropagateTags", DECLARED_PROPAGATE_TAGS.to_vec()),
        ("AssignPublicIp", DECLARED_ASSIGN_PUBLIC_IP.to_vec()),
        (
            "PlacementStrategyType",
            DECLARED_PLACEMENT_STRATEGY_TYPE.to_vec(),
        ),
        (
            "ApiDestinationHttpMethod",
            DECLARED_API_DESTINATION_HTTP_METHOD.to_vec(),
        ),
    ]);

    // enum -> (the order it was first stated in, the case that stated it).
    let mut seen: BTreeMap<&str, (Vec<String>, &str)> = BTreeMap::new();
    for row in rows() {
        let (_, body) = call(&emulator, row.operation, &row.request).await;
        let stated = stated_set(message(&body));

        let mut sorted = stated.clone();
        sorted.sort();
        let mut expected: Vec<String> = declared
            .get(row.enumeration)
            .unwrap_or_else(|| panic!("case {:?}: the row names no declared enum", row.case))
            .iter()
            .map(|value| (*value).to_string())
            .collect();
        expected.sort();
        assert_eq!(
            sorted, expected,
            "case {:?}: the stated set does not hold exactly {}'s declared values, it is {stated:?}",
            row.case, row.enumeration
        );

        match seen.get(row.enumeration) {
            Some((first, first_case)) => assert_eq!(
                &stated, first,
                "case {:?}: {} is stated in another order here than in case {first_case:?}",
                row.case, row.enumeration
            ),
            None => {
                seen.insert(row.enumeration, (stated, row.case));
            }
        }
    }
    assert_eq!(
        seen.len(),
        declared.len(),
        "every enum of the table was refused at least once, got {:?}",
        seen.keys().collect::<Vec<_>>()
    );
}

/// A value the type DOES admit is not refused for its value set: the rows above
/// cannot have been produced by refusing every member of an enumerated type.
#[tokio::test]
async fn a_modelled_value_is_not_refused() {
    let emulator = emulator();
    let (created, body) = call(
        &emulator,
        "PutRule",
        &json!({ "Name": RULE, "EventPattern": r#"{"source":["com.demo"]}"# }),
    )
    .await;
    assert_eq!(created, 200, "the rule the targets are put on: {body:?}");

    // One accepted request per enum this suite refuses a value of, each carrying
    // a value from the set the refusals state.
    let accepted: Vec<(&str, &str, Value)> = vec![
        (
            "PutRule with a modelled State",
            "PutRule",
            json!({
                "Name": RULE,
                "State": LISTED_RULE_STATE[0],
                "EventPattern": r#"{"source":["com.demo"]}"#,
            }),
        ),
        (
            "PutTargets with a modelled LaunchType and placement strategy",
            "PutTargets",
            put_targets(
                1,
                ecs_parameters(json!({
                    "LaunchType": LISTED_LAUNCH_TYPE[0],
                    "PropagateTags": DECLARED_PROPAGATE_TAGS[0],
                    "PlacementStrategy": [{ "type": DECLARED_PLACEMENT_STRATEGY_TYPE[1] }],
                    "NetworkConfiguration": {
                        "awsvpcConfiguration": {
                            "Subnets": ["subnet-0123456789abcdef0"],
                            "AssignPublicIp": DECLARED_ASSIGN_PUBLIC_IP[0],
                        },
                    },
                })),
            ),
        ),
    ];

    for (case, operation, request) in &accepted {
        let (status, body) = call(&emulator, operation, request).await;
        // Whether such a request is accepted on OTHER grounds is not this suite's
        // subject — only that no admissible value is refused for its own value
        // set, which is what the clause the rows above assert would say.
        assert!(
            !message(&body).contains("enum value set"),
            "case {case:?}: an admissible value was refused for its value set \
             (HTTP {status}): {body:?}"
        );
    }
}

/// Every listed order this suite expects IS the table's order: the values of one
/// type sorted by the bucket each sits in (ascending), two values of one bucket
/// read from the head each was pushed onto, so the LATER-declared comes first.
///
/// The bucket comes from the one derivation of it
/// (`service_common::validation_message::enum_value_bucket`); the orders the rows
/// above assert are written out by hand, so this is what ties each of them to the
/// rule rather than to whatever the service produced.
#[test]
fn the_listed_order_is_the_tables_bucket_order() {
    // (the type, its declared values, the order this suite expects it listed in)
    let types: Vec<(&str, Vec<&str>, Vec<&str>)> = vec![
        (
            "RuleState",
            DECLARED_RULE_STATE.to_vec(),
            LISTED_RULE_STATE.to_vec(),
        ),
        (
            "LaunchType",
            DECLARED_LAUNCH_TYPE.to_vec(),
            LISTED_LAUNCH_TYPE.to_vec(),
        ),
        (
            "AssignPublicIp",
            DECLARED_ASSIGN_PUBLIC_IP.to_vec(),
            LISTED_ASSIGN_PUBLIC_IP.to_vec(),
        ),
        (
            "PropagateTags",
            DECLARED_PROPAGATE_TAGS.to_vec(),
            LISTED_PROPAGATE_TAGS.to_vec(),
        ),
        (
            "PlacementStrategyType",
            DECLARED_PLACEMENT_STRATEGY_TYPE.to_vec(),
            LISTED_PLACEMENT_STRATEGY_TYPE.to_vec(),
        ),
        (
            "ApiDestinationHttpMethod",
            DECLARED_API_DESTINATION_HTTP_METHOD.to_vec(),
            LISTED_API_DESTINATION_HTTP_METHOD.to_vec(),
        ),
    ];

    for (name, declared, listed) in &types {
        let mut held = listed.clone();
        held.sort_unstable();
        let mut expected = declared.clone();
        expected.sort_unstable();
        assert_eq!(
            held, expected,
            "case {name:?}: the listed order holds other values than the declared \
             {expected:?}, it is {listed:?}"
        );

        let declared_at = |value: &str| {
            declared
                .iter()
                .position(|declared| declared == &value)
                .unwrap_or_else(|| panic!("case {name:?}: {value:?} is not a declared value"))
        };
        for pair in listed.windows(2) {
            let (before, after) = (pair[0], pair[1]);
            let (one, other) = (enum_value_bucket(before), enum_value_bucket(after));
            assert!(
                one <= other,
                "case {name:?}: {before:?} (bucket {one}) is listed before {after:?} \
                 (bucket {other}), so the buckets do not ascend"
            );
            if one == other {
                assert!(
                    declared_at(before) > declared_at(after),
                    "case {name:?}: {before:?} and {after:?} share bucket {one}, so the \
                     later-declared of the two is listed first, but {before:?} is declared \
                     at {} and {after:?} at {}",
                    declared_at(before),
                    declared_at(after)
                );
            }
        }
    }
}
